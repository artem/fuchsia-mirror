// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A Netstack3 worker to serve fuchsia.net.root.Interfaces API requests.

use async_utils::channel::TrySend as _;
use fidl::endpoints::{ControlHandle as _, ProtocolMarker as _, ServerEnd};
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_root as fnet_root;
use fuchsia_zircon as zx;
use futures::TryStreamExt as _;
use tracing::{debug, error};

use crate::bindings::{devices::BindingId, interfaces_admin, DeviceIdExt as _, Netstack};

// Serve a stream of fuchsia.net.root.Interfaces API requests for a single
// channel (e.g. a single client connection).
pub(crate) async fn serve_interfaces(
    ns: Netstack,
    rs: fnet_root::InterfacesRequestStream,
) -> Result<(), fidl::Error> {
    debug!(protocol = fnet_root::InterfacesMarker::DEBUG_NAME, "serving");
    rs.try_for_each(|req| async {
        match req {
            fnet_root::InterfacesRequest::GetAdmin { id, control, control_handle: _ } => {
                handle_get_admin(&ns, id, control).await;
            }
        }
        Ok(())
    })
    .await
}

async fn handle_get_admin(
    ns: &Netstack,
    interface_id: u64,
    control: ServerEnd<fnet_interfaces_admin::ControlMarker>,
) {
    debug!(interface_id, "handling fuchsia.net.root.Interfaces::GetAdmin");
    let ctx = ns.ctx.clone();
    let core_id =
        BindingId::new(interface_id).and_then(|id| ctx.non_sync_ctx.devices.get_core_id(id));
    let core_id = match core_id {
        Some(c) => c,
        None => {
            control.close_with_epitaph(zx::Status::NOT_FOUND).unwrap_or_else(|e| {
                if e.is_closed() {
                    debug!(err = ?e, "control handle closed before sending epitaph")
                } else {
                    error!(err = ?e, "failed to send epitaph")
                }
            });
            return;
        }
    };

    let mut sender = core_id.external_state().with_common_info(|i| i.control_hook.clone());

    match sender.try_send_fut(interfaces_admin::OwnedControlHandle::new_unowned(control)).await {
        Ok(()) => {}
        Err(owned_control_handle) => {
            owned_control_handle.into_control_handle().shutdown_with_epitaph(zx::Status::NOT_FOUND)
        }
    }
}
