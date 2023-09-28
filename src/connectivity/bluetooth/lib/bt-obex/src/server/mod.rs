// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::Channel;
use futures::future::Future;
use futures::stream::StreamExt;
use packet_encoding::{Decodable, Encodable};
use tracing::{info, trace, warn};

use crate::error::{Error, PacketError};
use crate::header::HeaderSet;
use crate::operation::{OpCode, RequestPacket, ResponseCode, ResponsePacket, SetPathFlags};
use crate::transport::max_packet_size_from_transport;
pub use crate::transport::TransportType;

/// Defines an interface for handling OBEX requests. All profiles & services should implement this
/// interface.
mod handler;
pub use handler::{ObexOperationError, ObexServerHandler};

/// Implements the OBEX GET operation.
mod get;
use get::GetOperation;

/// Implements the OBEX PUT operation.
mod put;
use put::PutOperation;

/// Represents a request to be handled by the OBEX Server during a multi-step operation.
#[derive(Debug)]
pub enum OperationRequest {
    /// Request to send a response packet to the remote peer.
    SendPacket(ResponsePacket),
    /// Request to get the payload from the upper layer application -- occurs in a GET operation.
    GetApplicationData(HeaderSet),
    /// Request to give the payload to the upper layer application -- occurs in a PUT operation.
    PutApplicationData(Vec<u8>, HeaderSet),
}

/// Represents a response from the upper layer application during a multi-step operation.
#[derive(Debug)]
pub enum ApplicationResponse {
    /// The application responded successfully to the GET request by providing the data payload
    /// and informational headers.
    Get((Vec<u8>, HeaderSet)),
    /// The application responded successfully to the PUT request.
    Put,
}

impl ApplicationResponse {
    #[cfg(test)]
    fn accept_get(data: Vec<u8>, headers: HeaderSet) -> Result<Self, ObexOperationError> {
        Ok(ApplicationResponse::Get((data, headers)))
    }

    #[cfg(test)]
    fn accept_put() -> Result<Self, ObexOperationError> {
        Ok(ApplicationResponse::Put)
    }
}

/// An interface for implementing a multi-step OBEX operation. Currently, the only two such
/// operations are GET and PUT.
/// See OBEX 1.5 Sections 3.4.3 & 3.4.4.
pub trait ServerOperation {
    /// Returns true if the operation is complete (e.g. all response packets have been sent).
    fn is_complete(&self) -> bool;

    /// Handle a `request` packet received from the OBEX client.
    /// Returns an `OperationRequest` to be handled by the OBEX server on success, Error if the
    /// request was invalid or couldn't be handled.
    fn handle_peer_request(&mut self, request: RequestPacket) -> Result<OperationRequest, Error>;

    /// Handle a response received from the upper layer application profile.
    /// `response` is Ok<T> if the application accepted the GET or PUT request.
    /// `response` is Err<E> if the application rejected the GET or PUT request.
    /// Returns a response packet to be sent to the remote peer if the application `response` was
    /// successfully handled.
    /// Returns Error if there was an internal operation error.
    fn handle_application_response(
        &mut self,
        response: Result<ApplicationResponse, ObexOperationError>,
    ) -> Result<ResponsePacket, Error>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ConnectionStatus {
    /// The transport is created but the CONNECT operation has not been completed.
    Initialized,
    /// The transport is connected and the CONNECT operation has been completed.
    Connected,
    /// The transport is connected but a DISCONNECT request has been received. The `ObexServer`
    /// will no longer process requests from the remote peer.
    DisconnectReceived,
}

/// Implements the Server role for the OBEX protocol.
/// Provides an interface for receiving and responding to OBEX requests made by a remote OBEX client
/// service. Supports the operations defined in OBEX 1.5.
pub struct ObexServer {
    /// The current connection status of the server.
    connected: ConnectionStatus,
    /// The maximum OBEX packet length for this OBEX session.
    max_packet_size: u16,
    /// The active OBEX operation. The only two supported multi-step operations are GET and PUT.
    /// This is Some<T> when an operation is in progress, and None otherwise. There can only be one
    /// active multi-step operation. An operation is considered complete when
    /// `ServerOperation::is_complete` returns true.
    /// The active operation is cleaned up lazily -- when a request to start a new operation is
    /// received, the previously finished operation is removed.
    active_operation: Option<Box<dyn ServerOperation>>,
    /// The data channel that is used to read & write OBEX packets.
    channel: Channel,
    /// The handler provided by the application profile. This handler should implement the
    /// operations defined in OBEX 1.5 and will be used to provide a response to an incoming
    /// request made by the remote OBEX client.
    handler: Box<dyn ObexServerHandler>,
}

impl ObexServer {
    pub fn new(channel: Channel, handler: Box<dyn ObexServerHandler>) -> Self {
        let max_packet_size = max_packet_size_from_transport(channel.max_tx_size());
        Self {
            connected: ConnectionStatus::Initialized,
            max_packet_size,
            active_operation: None,
            channel,
            handler,
        }
    }

    /// Returns `true` if the OBEX connection is currently active (e.g. CONNECT operation done).
    fn is_connected(&self) -> bool {
        matches!(self.connected, ConnectionStatus::Connected)
    }

    fn set_connected(&mut self, status: ConnectionStatus) {
        self.connected = status;
    }

    fn set_max_packet_size(&mut self, peer_max_packet_size: u16) {
        // Use the smaller of the peer max and local max for maximum compatibility.
        let min_ = std::cmp::min(peer_max_packet_size, self.max_packet_size);
        self.max_packet_size = min_;
        trace!("Max packet size set to {}", self.max_packet_size);
    }

    /// Encodes and sends the OBEX `data` to the remote peer.
    /// Returns Error if the send operation could not be completed.
    fn send(&self, data: impl Encodable<Error = PacketError>) -> Result<(), Error> {
        let mut buf = vec![0; data.encoded_len()];
        data.encode(&mut buf[..])?;
        let _ = self.channel.as_ref().write(&buf)?;
        Ok(())
    }

    async fn connect_request(&mut self, request: RequestPacket) -> Result<ResponsePacket, Error> {
        // Parse the additional data first - the data length is already validated during decoding.
        let data = request.data();
        let version = data[0];
        let flags = data[1];
        let peer_max_packet_size = u16::from_be_bytes(data[2..4].try_into().unwrap());
        trace!(version, flags, peer_max_packet_size, "Additional data in CONNECT request");
        self.set_max_packet_size(peer_max_packet_size);

        let headers = HeaderSet::from(request);
        // TODO(fxbug.dev/129950): Check `headers` for Target header. If present, generate a
        // ConnectionId for a directed OBEX connection.
        let (code, response_headers) = match self.handler.connect(headers).await {
            Ok(headers) => {
                trace!("Application accepted CONNECT request");
                self.set_connected(ConnectionStatus::Connected);
                (ResponseCode::Ok, headers)
            }
            Err(reject_parameters) => {
                trace!("Application rejected CONNECT request");
                reject_parameters
            }
        };
        let response_packet =
            ResponsePacket::new_connect(code, self.max_packet_size, response_headers);
        Ok(response_packet)
    }

    /// Handles a Disconnect request made by the remote OBEX client.
    /// Returns a response packet to be sent on success, Error if the request couldn't be handled.
    async fn disconnect_request(
        &mut self,
        request: RequestPacket,
    ) -> Result<ResponsePacket, Error> {
        let headers = HeaderSet::from(request);
        let response_headers = self.handler.disconnect(headers).await;
        let response_packet = ResponsePacket::new_disconnect(response_headers);
        self.set_connected(ConnectionStatus::DisconnectReceived);
        Ok(response_packet)
    }

    /// Handles a SetPath request made by the remote OBEX client.
    /// Returns a response packet to be sent on success, Error if the request couldn't be handled.
    async fn setpath_request(&mut self, request: RequestPacket) -> Result<ResponsePacket, Error> {
        if !self.is_connected() {
            return Err(Error::operation(OpCode::SetPath, "CONNECT not completed"));
        }
        // Parse the additional data first - the data length is already validated during decoding.
        // Only the `flags` field is used in OBEX 1.5. `constants` is RFA.
        let data = request.data();
        let flags = SetPathFlags::from_bits_truncate(data[0]);
        let backup = flags.contains(SetPathFlags::BACKUP);
        let create = !flags.contains(SetPathFlags::DONT_CREATE);

        let headers = HeaderSet::from(request);
        let (code, response_headers) = match self.handler.set_path(headers, backup, create).await {
            Ok(headers) => {
                trace!("Application accepted SETPATH request");
                (ResponseCode::Ok, headers)
            }
            Err(reject_parameters) => {
                trace!("Application rejected SETPATH request");
                reject_parameters
            }
        };
        let response_packet = ResponsePacket::new_setpath(code, response_headers);
        Ok(response_packet)
    }

    /// Potentially initializes a new multi-step operation.
    /// Returns true if a new operation was initialized, false otherwise.
    fn maybe_start_new_operation(&mut self, code: &OpCode) -> bool {
        if self.active_operation.as_ref().map_or(false, |o| !o.is_complete()) {
            return false;
        }

        let op: Box<dyn ServerOperation> = match code {
            OpCode::Get | OpCode::GetFinal => Box::new(GetOperation::new(self.max_packet_size)),
            OpCode::Put | OpCode::PutFinal => Box::new(PutOperation::new()),
            _ => unreachable!("only called from `Self::multistep_request`"),
        };
        trace!("Started new operation ({code:?})");
        self.active_operation = Some(op);
        return true;
    }

    /// Handles a request made by the remote OBEX client for a potentially multi-step
    /// operation (PUT or GET).
    /// Returns a response packet to be sent to the peer on success, Error if the request can't
    /// be handled or is invalid.
    async fn multistep_request(&mut self, request: RequestPacket) -> Result<ResponsePacket, Error> {
        let _ = self.maybe_start_new_operation(request.code());
        let operation = self.active_operation.as_mut().expect("just initialized");

        let application_response = match operation.handle_peer_request(request) {
            Ok(OperationRequest::SendPacket(response)) => return Ok(response),
            Ok(OperationRequest::GetApplicationData(request_headers)) => {
                self.handler.get(request_headers).await.map(|x| ApplicationResponse::Get(x))
            }
            Ok(OperationRequest::PutApplicationData(data, request_headers)) => {
                self.handler.put(data, request_headers).await.map(|_| ApplicationResponse::Put)
            }
            Err(e) => {
                warn!("Internal error in operation: {e:?}");
                return Ok(ResponsePacket::new_no_data(
                    ResponseCode::InternalServerError,
                    HeaderSet::new(),
                ));
            }
        };

        operation.handle_application_response(application_response)
    }

    /// Processes a raw data `packet` received from the remote peer acting as an OBEX client.
    /// Returns a `ResponsePacket` on success, Error if the request couldn't be handled.
    async fn receive_packet(&mut self, packet: Vec<u8>) -> Result<ResponsePacket, Error> {
        let decoded = RequestPacket::decode(&packet[..])?;
        trace!(packet = ?decoded, "Received request from OBEX client");
        match decoded.code() {
            OpCode::Connect => self.connect_request(decoded).await,
            OpCode::Disconnect => self.disconnect_request(decoded).await,
            OpCode::SetPath => self.setpath_request(decoded).await,
            OpCode::Put | OpCode::PutFinal | OpCode::Get | OpCode::GetFinal => {
                self.multistep_request(decoded).await
            }
            _code => todo!("Support other OBEX requests"),
        }
    }

    pub fn run(mut self) -> impl Future<Output = Result<(), Error>> {
        async move {
            while let Some(packet) = self.channel.next().await {
                match packet {
                    Ok(bytes) => {
                        let response = self.receive_packet(bytes).await?;
                        self.send(response)?;

                        // The OBEX Client requested to close the OBEX connection.
                        if self.connected == ConnectionStatus::DisconnectReceived {
                            trace!("Disconnect request - closing transport");
                            return Ok(());
                        }
                    }
                    Err(e) => warn!("Error reading data from transport: {e:?}"),
                }
            }
            info!("Peer disconnected transport");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use async_utils::PollExt;
    use fuchsia_async as fasync;
    use futures::pin_mut;

    use crate::header::{Header, HeaderIdentifier, HeaderSet};
    use crate::server::handler::test_utils::TestApplicationProfile;
    use crate::transport::test_utils::{expect_response, send_packet};

    /// Returns an ObexServer, a testonly object representing an upper layer profile, & remote
    /// peer's side of the transport.
    fn new_obex_server() -> (ObexServer, TestApplicationProfile, Channel) {
        let (local, remote) = Channel::create();
        let app = TestApplicationProfile::new();
        let obex_server = ObexServer::new(local, Box::new(app.clone()));
        (obex_server, app, remote)
    }

    #[fuchsia::test]
    fn obex_server_terminates_when_channel_closes() {
        let mut exec = fasync::TestExecutor::new();
        let (obex_server, _test_app, remote) = new_obex_server();

        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server still active");

        drop(remote);
        let result = exec.run_until_stalled(&mut server_fut).expect("server finished");
        assert_matches!(result, Ok(_));
    }

    #[fuchsia::test]
    fn connect_accepted_by_app_success() {
        let mut exec = fasync::TestExecutor::new();
        let (obex_server, test_app, mut remote) = new_obex_server();
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let headers = HeaderSet::from_header(Header::Target(vec![5, 6])).unwrap();
        let connect_request = RequestPacket::new_connect(500, headers);
        send_packet(&mut remote, connect_request);

        // Expect the ObexServer to receive the request, parse it, ask the application, and reply.
        // Simulate application accepting the request.
        let headers = HeaderSet::from_header(Header::name("foo")).unwrap();
        test_app.set_response(Ok(headers));
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        // Expect the remote peer to receive our CONNECT response.
        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::Ok);
            assert_eq!(response.data(), &[0x10, 0, 0x01, 0xf4]);
            let headers = HeaderSet::from(response);
            assert!(headers.contains_header(&HeaderIdentifier::Name));
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::Connect);
    }

    #[fuchsia::test]
    fn connect_rejected_by_app_is_ok() {
        let mut exec = fasync::TestExecutor::new();
        let (obex_server, test_app, mut remote) = new_obex_server();
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let connect_request = RequestPacket::new_connect(255, HeaderSet::new());
        send_packet(&mut remote, connect_request);

        // The ObexServer should receive the request and hand it to the profile - profile rejects.
        test_app.set_response(Err((ResponseCode::Forbidden, HeaderSet::new())));
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        // Expect the remote peer to receive our negative CONNECT response.
        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::Forbidden);
            assert_eq!(response.data(), &[0x10, 0, 0x00, 0xff]);
            let headers = HeaderSet::from(response);
            assert!(headers.is_empty());
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::Connect);
    }

    #[fuchsia::test]
    fn invalid_connect_request_is_error() {
        let mut exec = fasync::TestExecutor::new();
        let (obex_server, _test_app, remote) = new_obex_server();

        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server still active");

        // Invalid CONNECT request. Missing the 2 byte max packet size field.
        let _ = remote.as_ref().write(&[0x80, 0x00, 0x05, 0x00, 0x00]).expect("can send data");

        let result = exec.run_until_stalled(&mut server_fut).expect("terminate due to error");
        assert_matches!(result, Err(Error::Packet(_)));
    }

    #[fuchsia::test]
    fn peer_disconnect_request_terminates_server() {
        let mut exec = fasync::TestExecutor::new();
        let (obex_server, test_app, mut remote) = new_obex_server();
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let headers = HeaderSet::from_header(Header::Description("done".into())).unwrap();
        let disconnect_request = RequestPacket::new_disconnect(headers);
        send_packet(&mut remote, disconnect_request);

        // Expect the ObexServer to receive the request, parse it, get response headers from the
        // application, and reply. Because this is a Disconnect request, the server run loop
        // should finish.
        let headers = HeaderSet::from_header(Header::Description("disconnecting".into())).unwrap();
        test_app.set_response(Ok(headers));
        let result =
            exec.run_until_stalled(&mut server_fut).expect("server terminated from disconnect");
        assert_matches!(result, Ok(_));

        // Expect the remote peer to receive our DISCONNECT response.
        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::Ok);
            let headers = HeaderSet::from(response);
            assert!(headers.contains_header(&HeaderIdentifier::Description));
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::Disconnect);
    }

    #[fuchsia::test]
    fn setpath_request_accepted_by_app_success() {
        let mut exec = fasync::TestExecutor::new();
        let (mut obex_server, test_app, mut remote) = new_obex_server();
        // Set to the Connected state to bypass CONNECT operation.
        obex_server.set_connected(ConnectionStatus::Connected);
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let headers = HeaderSet::from_header(Header::name("folder1")).unwrap();
        let setpath_request =
            RequestPacket::new_set_path(SetPathFlags::all(), headers).expect("valid request");
        send_packet(&mut remote, setpath_request);

        // The ObexServer should receive the request and hand it to the profile - profile accepts.
        test_app.set_response(Ok(HeaderSet::new()));
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        // Expect the remote peer to receive our SETPATH response.
        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::Ok);
            let headers = HeaderSet::from(response);
            assert!(headers.is_empty());
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::SetPath);
    }

    #[fuchsia::test]
    fn setpath_request_rejected_by_app_success() {
        let mut exec = fasync::TestExecutor::new();
        let (mut obex_server, test_app, mut remote) = new_obex_server();
        // Set to the Connected state to bypass CONNECT operation.
        obex_server.set_connected(ConnectionStatus::Connected);
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let setpath_request = RequestPacket::new_set_path(SetPathFlags::BACKUP, HeaderSet::new())
            .expect("valid request");
        send_packet(&mut remote, setpath_request);

        // The ObexServer should receive the request and hand it to the profile - profile rejects.
        test_app.set_response(Err((ResponseCode::Forbidden, HeaderSet::new())));
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        // Expect the remote peer to receive our negative SETPATH response.
        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::Forbidden);
            let headers = HeaderSet::from(response);
            assert!(headers.is_empty());
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::SetPath);
    }

    #[fuchsia::test]
    fn setpath_request_before_connect_is_error() {
        let mut exec = fasync::TestExecutor::new();
        let (obex_server, _test_app, mut remote) = new_obex_server();
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let setpath_request = RequestPacket::new_set_path(SetPathFlags::BACKUP, HeaderSet::new())
            .expect("valid request");
        send_packet(&mut remote, setpath_request);
        let result = exec
            .run_until_stalled(&mut server_fut)
            .expect("server terminated from invalid setpath");
        assert_matches!(result, Err(Error::OperationError { operation: OpCode::SetPath, .. }));
    }

    #[fuchsia::test]
    fn get_request_accepted_by_app_success() {
        let mut exec = fasync::TestExecutor::new();
        let (mut obex_server, test_app, mut remote) = new_obex_server();
        // Set to the Connected state to bypass CONNECT operation.
        obex_server.set_connected(ConnectionStatus::Connected);
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        // Send an example GET_FINAL request with a header describing the name of the object.
        let headers = HeaderSet::from_header(Header::name("random object")).unwrap();
        let get_request = RequestPacket::new_get_final(headers);
        send_packet(&mut remote, get_request);

        // The ObexServer should receive the request and hand it to the profile. Set the profile
        // handler to return a static buffer.
        let application_response_buf = vec![1, 2, 3, 4, 5, 6];
        let response_headers = HeaderSet::from_header(Header::Description("foo".into())).unwrap();
        test_app.set_get_response((application_response_buf.clone(), response_headers));
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::Ok);
            let mut headers = HeaderSet::from(response);
            assert!(headers.contains_header(&HeaderIdentifier::Description));
            let received_body = headers.remove_body(/*final_=*/ true).expect("contains body");
            assert_eq!(received_body, application_response_buf);
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::GetFinal);
    }

    #[fuchsia::test]
    fn get_request_rejected_by_app_success() {
        let mut exec = fasync::TestExecutor::new();
        let (mut obex_server, _test_app, mut remote) = new_obex_server();
        obex_server.set_connected(ConnectionStatus::Connected);
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        // Send an example GET_FINAL request with a header describing the name of the object.
        let headers = HeaderSet::from_header(Header::name("random object123")).unwrap();
        let get_request = RequestPacket::new_get_final(headers);
        send_packet(&mut remote, get_request);

        // The ObexServer receives request and hands to application. By default, it rejects the
        // request.
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");
        // Expect the peer to received the rejection code.
        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::NotImplemented);
            assert!(response.headers().is_empty());
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::GetFinal);
    }

    #[fuchsia::test]
    fn put_request_accepted_by_app_success() {
        let mut exec = fasync::TestExecutor::new();
        let (mut obex_server, test_app, mut remote) = new_obex_server();
        // Set to the Connected state to bypass CONNECT operation.
        obex_server.set_connected(ConnectionStatus::Connected);
        let server_fut = obex_server.run();
        pin_mut!(server_fut);
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");

        let headers = HeaderSet::from_headers(vec![
            Header::name("random object"),
            Header::EndOfBody(vec![1, 2, 3, 4, 5]),
        ])
        .unwrap();
        let put_request = RequestPacket::new_put_final(headers);
        send_packet(&mut remote, put_request);

        // The ObexServer should receive the request and hand it to the profile. Profile accepts.
        test_app.set_put_response(Ok(()));
        let _ = exec.run_until_stalled(&mut server_fut).expect_pending("server active");
        // Verify profile received correct data.
        let (rec_data, rec_headers) = test_app.put_data();
        assert_eq!(rec_data, vec![1, 2, 3, 4, 5]);
        assert!(rec_headers.contains_header(&HeaderIdentifier::Name));

        let expectation = |response: ResponsePacket| {
            assert_eq!(*response.code(), ResponseCode::Ok);
            assert!(response.headers().is_empty());
        };
        expect_response(&mut exec, &mut remote, expectation, OpCode::PutFinal);
    }
}
