// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{registry, CapabilityTrait, ConversionError, Open};
use fidl::endpoints::{create_request_stream, ClientEnd, ControlHandle, RequestStream, ServerEnd};
use fidl::epitaph::ChannelEpitaphExt;
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{channel::mpsc, TryStreamExt};
use std::fmt::Debug;
use tracing::warn;
use vfs::execution_scope::ExecutionScope;

#[derive(Debug)]
pub struct Message {
    pub payload: fsandbox::ProtocolPayload,
}

/// A capability that transfers another capability to a [Receiver].
#[derive(Debug)]
pub struct Sender {
    inner: mpsc::UnboundedSender<Message>,

    /// The FIDL representation of this `Sender`.
    ///
    /// This will be `Some` if was previously converted into a `ClientEnd`, such as by calling
    /// [into_fidl], and the capability is not currently in the registry.
    client_end: Option<ClientEnd<fsandbox::SenderMarker>>,
}

impl Clone for Sender {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), client_end: None }
    }
}

impl Sender {
    pub(crate) fn new(sender: mpsc::UnboundedSender<Message>) -> Self {
        Self { inner: sender, client_end: None }
    }

    pub(crate) fn send_channel(
        &self,
        channel: zx::Channel,
        flags: fio::OpenFlags,
    ) -> Result<(), mpsc::TrySendError<Message>> {
        let msg = Message { payload: fsandbox::ProtocolPayload { channel, flags } };
        self.send(msg)
    }

    pub fn send(&self, msg: Message) -> Result<(), mpsc::TrySendError<Message>> {
        self.inner.unbounded_send(msg)
    }

    async fn serve_sender(self, mut stream: fsandbox::SenderRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::SenderRequest::Send_ { channel, flags, control_handle: _ } => {
                    if let Err(_err) = self.send_channel(channel, flags) {
                        stream.control_handle().shutdown_with_epitaph(zx::Status::PEER_CLOSED);
                        return;
                    }
                }
                fsandbox::SenderRequest::Clone2 { request, control_handle: _ } => {
                    // The clone is registered under the koid of the client end.
                    let koid = request.basic_info().unwrap().related_koid;
                    let server_end: ServerEnd<fsandbox::SenderMarker> =
                        request.into_channel().into();
                    let stream = server_end.into_stream().unwrap();
                    self.clone().serve_and_register(stream, koid);
                }
                fsandbox::SenderRequest::_UnknownMethod { ordinal, .. } => {
                    warn!("Received unknown Sender request with ordinal {ordinal}");
                }
            }
        }
    }

    /// Serves the `fuchsia.sandbox.Sender` protocol for this Sender and moves it into the registry.
    pub fn serve_and_register(self, stream: fsandbox::SenderRequestStream, koid: zx::Koid) {
        let sender = self.clone();
        let fut = sender.serve_sender(stream);

        // Move this capability into the registry.
        let task = fasync::Task::spawn(registry::remove_when_done(koid, fasync::Task::spawn(fut)));
        registry::insert_with_task(self.into(), koid, task);
    }

    /// Sets this Sender's client end to the provided one.
    ///
    /// This should only be used to put a remoted client end back into the Sender after it is
    /// removed from the registry.
    pub(crate) fn set_client_end(&mut self, client_end: ClientEnd<fsandbox::SenderMarker>) {
        self.client_end = Some(client_end)
    }
}

impl From<Sender> for Open {
    fn from(sender: Sender) -> Self {
        let open_fn = move |_scope: ExecutionScope,
                            flags: fio::OpenFlags,
                            path: vfs::path::Path,
                            server_end: zx::Channel| {
            if !path.is_empty() {
                // Only an empty path is valid.
                let _ = server_end.close_with_epitaph(zx::Status::NOT_DIR);
                return;
            }
            let _ = sender.send_channel(server_end.into(), flags);
        };
        Self::new(open_fn, fio::DirentType::Service)
    }
}

impl CapabilityTrait for Sender {
    fn try_into_open(self) -> Result<Open, ConversionError> {
        Ok(self.into())
    }
}

impl From<Sender> for ClientEnd<fsandbox::SenderMarker> {
    /// Serves the `fuchsia.sandbox.Sender` protocol for this Sender and moves it into the registry.
    fn from(mut sender: Sender) -> ClientEnd<fsandbox::SenderMarker> {
        sender.client_end.take().unwrap_or_else(|| {
            let (client_end, sender_stream) =
                create_request_stream::<fsandbox::SenderMarker>().unwrap();
            sender.serve_and_register(sender_stream, client_end.get_koid().unwrap());
            client_end
        })
    }
}

impl From<Sender> for fsandbox::Capability {
    fn from(sender: Sender) -> Self {
        fsandbox::Capability::Sender(sender.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Receiver;
    use fidl::endpoints::create_endpoints;
    use fidl_fuchsia_unknown as funknown;

    // NOTE: sending-and-receiving tests are written in `receiver.rs`.

    /// Tests that a Sender can be cloned via `fuchsia.unknown/Cloneable.Clone2`
    /// and capabilities sent to the original and clone arrive at the same Receiver.
    #[fuchsia::test]
    async fn fidl_clone() {
        let (receiver, sender) = Receiver::new();

        // Send a channel through the Sender.
        let (ch1, _ch2) = zx::Channel::create();
        sender.send_channel(ch1, fio::OpenFlags::empty()).unwrap();

        // Convert the Sender to a FIDL proxy.
        let client_end: ClientEnd<fsandbox::SenderMarker> = sender.into();
        let sender_proxy = client_end.into_proxy().unwrap();

        // Clone the Sender with `Clone2`.
        let (clone_client_end, clone_server_end) = create_endpoints::<funknown::CloneableMarker>();
        let _ = sender_proxy.clone2(clone_server_end);
        let clone_client_end: ClientEnd<fsandbox::SenderMarker> =
            clone_client_end.into_channel().into();
        let clone_proxy = clone_client_end.into_proxy().unwrap();

        // Send a channel through the cloned Sender.
        let (ch1, _ch2) = zx::Channel::create();
        clone_proxy.send_(ch1, fio::OpenFlags::empty()).unwrap();

        // The Receiver should receive two channels, one from each sender.
        for _ in 0..2 {
            let _ch = receiver.receive().await.unwrap();
        }
    }
}
