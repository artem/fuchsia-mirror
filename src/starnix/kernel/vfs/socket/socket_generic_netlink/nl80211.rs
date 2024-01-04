// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{GenericMessage, GenericNetlinkFamily};
use starnix_logging::{log_debug, log_error, log_warn};

use anyhow::{bail, format_err, Context as _, Error};
use async_trait::async_trait;
use fidl_fuchsia_wlan_wlanix as fidl_wlanix;
use futures::{channel::mpsc, StreamExt};
use netlink::{messaging::Sender, NETLINK_LOG_TAG};
use netlink_packet_core::{
    buffer::NETLINK_HEADER_LEN, DoneMessage, ErrorMessage, NetlinkHeader, NetlinkMessage,
    NetlinkPayload,
};
use netlink_packet_utils::Emitable;

#[derive(Clone)]
pub struct Nl80211Family {
    nl80211_proxy: fidl_wlanix::Nl80211Proxy,
}

impl Nl80211Family {
    pub fn new() -> Result<Self, Error> {
        let wlanix_svc =
            fuchsia_component::client::connect_to_protocol::<fidl_wlanix::WlanixMarker>()
                .context("failed to connect to wlanix")?;
        let (nl80211_proxy, nl80211_server) = fidl::endpoints::create_proxy()?;
        wlanix_svc
            .get_nl80211(fidl_wlanix::WlanixGetNl80211Request {
                nl80211: Some(nl80211_server),
                ..Default::default()
            })
            .map_err(|status| format_err!("Failed to get Nl80211 server with status {}", status))?;
        Ok(Self { nl80211_proxy })
    }
}

fn fidl_message_to_netlink(
    header: NetlinkHeader,
    message: fidl_wlanix::Nl80211Message,
) -> Result<NetlinkMessage<GenericMessage>, Error> {
    // TODO(https://fxbug.dev/128857): Deduplicate these calls with similar calls used by rtnetlink.
    let payload = match message.message_type {
        Some(fidl_wlanix::Nl80211MessageType::Message) => {
            NetlinkPayload::InnerMessage(GenericMessage::Other {
                family: header.message_type,
                payload: message.payload.unwrap_or(vec![]),
            })
        }
        Some(fidl_wlanix::Nl80211MessageType::Ack) => {
            let mut buffer = [0; NETLINK_HEADER_LEN];
            header.emit(&mut buffer[..NETLINK_HEADER_LEN]);
            let mut ack = ErrorMessage::default();
            // Netlink uses an error payload with no error code to indicate a
            // successful ack.
            ack.code = None;
            ack.header = buffer.to_vec();
            NetlinkPayload::Error(ack)
        }
        Some(fidl_wlanix::Nl80211MessageType::Done) => {
            let done = DoneMessage::default();
            NetlinkPayload::Done(done)
        }
        other => bail!("Dropping unsupported nl80211 netlink protocol response type: {:?}", other),
    };
    let mut netlink_message = NetlinkMessage::new(header, payload);
    netlink_message.finalize();
    Ok(netlink_message)
}

#[async_trait]
impl<S: Sender<GenericMessage>> GenericNetlinkFamily<S> for Nl80211Family {
    fn name(&self) -> String {
        "nl80211".into()
    }

    fn multicast_groups(&self) -> Vec<String> {
        vec![
            "scan".to_string(),
            "regulatory".to_string(),
            "mlme".to_string(),
            "vendor".to_string(),
            "config".to_string(),
        ]
    }

    async fn stream_multicast_messages(
        &self,
        group: String,
        assigned_family_id: u16,
        message_sink: mpsc::UnboundedSender<NetlinkMessage<GenericMessage>>,
    ) {
        let (client_end, mut stream) =
            fidl::endpoints::create_request_stream().expect("Failed to create multicast stream");
        if let Err(e) = self.nl80211_proxy.get_multicast(fidl_wlanix::Nl80211GetMulticastRequest {
            group: Some(group.clone()),
            multicast: Some(client_end),
            ..Default::default()
        }) {
            log_error!(tag = NETLINK_LOG_TAG, "Failed to request multicast stream: {}", e,);
            return;
        }
        loop {
            match stream.next().await {
                Some(Ok(fidl_wlanix::Nl80211MulticastRequest::Message { payload, .. })) => {
                    if let Some(message) = payload.message {
                        let mut header = NetlinkHeader::default();
                        header.message_type = assigned_family_id;
                        match fidl_message_to_netlink(header, message) {
                            Ok(m) => {
                                if let Err(e) = message_sink.unbounded_send(m) {
                                    log_error!(
                                        tag = NETLINK_LOG_TAG,
                                        "Failed to send multicast message to handler: {}",
                                        e,
                                    );
                                }
                            }
                            Err(e) => log_error!(
                                tag = NETLINK_LOG_TAG,
                                "Failed to forward multicast message: {}",
                                e,
                            ),
                        }
                    }
                }
                Some(Ok(unexpected)) => {
                    log_error!(
                        tag = NETLINK_LOG_TAG,
                        "Received unexpected message from multicast protocol: {:?}",
                        unexpected,
                    );
                }
                None => {
                    log_warn!(
                        tag = NETLINK_LOG_TAG,
                        "Nl80211 multicast stream {} terminated",
                        group
                    );
                    return;
                }
                Some(Err(e)) => log_error!(
                    tag = NETLINK_LOG_TAG,
                    "Nl80211 multicast stream returned an error: {}",
                    e
                ),
            }
        }
    }

    async fn handle_message(
        &self,
        mut netlink_header: NetlinkHeader,
        payload: Vec<u8>,
        sender: &mut S,
    ) {
        log_debug!(
            tag = NETLINK_LOG_TAG,
            "Received nl80211 netlink protocol message: {:?}  -- {:?}",
            netlink_header,
            payload
        );
        let message = fidl_wlanix::Nl80211Message {
            message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
            payload: Some(payload),
            ..Default::default()
        };
        let response = match self
            .nl80211_proxy
            .message(fidl_wlanix::Nl80211MessageRequest {
                message: Some(message),
                ..Default::default()
            })
            .await
        {
            Err(e) => {
                log_error!(tag = NETLINK_LOG_TAG, "Failed to send nl80211 message: {}", e,);
                return;
            }
            Ok(Err(e)) => {
                log_error!(tag = NETLINK_LOG_TAG, "Nl80211 message returned an error: {}", e,);
                return;
            }
            Ok(Ok(response)) => response,
        };

        if let Some(responses) = response.responses {
            // It's not valid to send multiple messages with the same sequence
            // number unless the multipart flag is set.
            if responses.len() > 1 {
                netlink_header.flags |= netlink_packet_core::NLM_F_MULTIPART;
            }
            for resp_message in responses {
                match fidl_message_to_netlink(netlink_header, resp_message) {
                    Ok(resp) => sender.send(resp, None),
                    Err(e) => {
                        log_error!(tag = NETLINK_LOG_TAG, "Failed to send response message: {}", e)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::socket::socket_generic_netlink::test_utils::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::create_proxy_and_stream;
    use fuchsia_async::TestExecutor;
    use futures::{pin_mut, StreamExt};
    use std::task::Poll;

    #[test]
    fn test_fidl_message_to_netlink() {
        let mut header = NetlinkHeader::default();
        header.message_type = 123;
        header.sequence_number = 10;
        let fidl_message = fidl_wlanix::Nl80211Message {
            message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
            payload: Some(vec![1, 2, 3, 4, 5, 6, 7, 8]),
            ..Default::default()
        };

        let netlink_message = fidl_message_to_netlink(header, fidl_message)
            .expect("Netlink message conversion failed");
        assert_eq!(netlink_message.header.message_type, 123);
        assert_eq!(netlink_message.header.sequence_number, 10);
        let payload = assert_matches!(
            netlink_message.payload,
            NetlinkPayload::InnerMessage(GenericMessage::Other {
                payload,
                ..
            }) => payload
        );
        assert_eq!(payload, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn test_fidl_ack_to_netlink() {
        let mut header = NetlinkHeader::default();
        header.message_type = 123;
        header.sequence_number = 10;
        let fidl_message = fidl_wlanix::Nl80211Message {
            message_type: Some(fidl_wlanix::Nl80211MessageType::Ack),
            payload: Some(vec![1, 2, 3, 4, 5, 6, 7, 8]),
            ..Default::default()
        };

        let netlink_message = fidl_message_to_netlink(header, fidl_message)
            .expect("Netlink message conversion failed");
        assert_eq!(netlink_message.header.message_type, netlink_packet_core::NLMSG_ERROR);
        assert_eq!(netlink_message.header.sequence_number, 10);
        assert_matches!(netlink_message.payload, NetlinkPayload::Error(_));
    }

    #[test]
    fn test_get_multicast() {
        let mut exec = TestExecutor::new();
        let (nl80211_proxy, mut nl80211_stream) =
            create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>()
                .expect("Failed to create client and proxy");
        let family = Nl80211Family { nl80211_proxy };
        let (sender, mut receiver) = mpsc::unbounded();
        let mcast_fut =
            GenericNetlinkFamily::<TestSender<GenericMessage>>::stream_multicast_messages(
                &family,
                "test_group".to_string(),
                123,
                sender,
            );
        pin_mut!(mcast_fut);

        let next_mcast_recv = receiver.select_next_some();
        pin_mut!(next_mcast_recv);

        assert_matches!(exec.run_until_stalled(&mut mcast_fut), Poll::Pending);
        let multicast_req = assert_matches!(
            exec.run_until_stalled(&mut nl80211_stream.select_next_some()),
            Poll::Ready(Ok(fidl_wlanix::Nl80211Request::GetMulticast { payload, ..})) => payload);
        assert_eq!(multicast_req.group, Some("test_group".to_string()));
        let client_proxy: fidl_wlanix::Nl80211MulticastProxy = multicast_req
            .multicast
            .expect("No client endpoint")
            .into_proxy()
            .expect("Failed to create multicast client proxy");
        assert_matches!(exec.run_until_stalled(&mut next_mcast_recv), Poll::Pending);

        let message = fidl_wlanix::Nl80211Message {
            message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
            payload: Some(vec![1, 2, 3, 4, 5, 6, 7, 8]),
            ..Default::default()
        };
        client_proxy
            .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                message: Some(message),
                ..Default::default()
            })
            .expect("Failed to send multicast message");
        assert_matches!(exec.run_until_stalled(&mut mcast_fut), Poll::Pending);

        assert_matches!(exec.run_until_stalled(&mut next_mcast_recv), Poll::Ready(_));
    }
}
