// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::internal_message::*,
    fidl_fuchsia_ui_pointer as fptr, fuchsia_async as fasync,
    futures::channel::mpsc::UnboundedSender,
    tracing::{error, info},
};

pub fn spawn_mouse_source_watcher(
    mouse_source: fptr::MouseSourceProxy,
    sender: UnboundedSender<InternalMessage>,
) {
    fasync::Task::spawn(async move {
        // We expect these to be received before any pointer sample events.
        let mut device_info: Option<fptr::MouseDeviceInfo> = None;
        let mut view_parameters: Option<fptr::ViewParameters> = None;

        loop {
            match mouse_source.watch().await {
                Ok(events) => {
                    for e in events.iter() {
                        let timestamp = e.timestamp.unwrap();
                        // TODO(https://fxbug.dev/42163110): currently the API does not guarantee that the
                        // trace ID will be present.  However, the current implementation behaves
                        // this way, and in the future it will also be guaranteed by the API.
                        let trace_flow_id = e.trace_flow_id.unwrap();

                        // Handle `device_info` field, if it exists.
                        // TODO(https://fxbug.dev/42172483): We don't use this info; currently all of the
                        // fields except for `id` are absent.  Furthermore, the documentation is
                        // insufficient to understand how to handle these fields, if present.
                        if let Some(new_device_info) = e.device_info.clone() {
                            info!(
                                new = ?new_device_info,
                                replacing = ?device_info,
                                "Received device info",
                            );

                            device_info = Some(new_device_info);
                        }

                        // Handle `view_parameters` field, if it exists.
                        if let Some(new_view_parameters) = e.view_parameters.clone() {
                            info!(
                                new = ?new_view_parameters,
                                replacing = ?view_parameters,
                                "Received view parameters",
                            );
                            view_parameters = Some(new_view_parameters);
                        }

                        // Handle `pointer_sample` field, if it exists.
                        if let Some(fptr::MousePointerSample {
                            // If there are multiple mouse devices, then the device ID could be used
                            // to disambiguate them, but we don't worry about that case.
                            device_id: _,
                            position_in_viewport: Some(position_in_viewport),
                            relative_motion,
                            scroll_v,
                            scroll_h,
                            pressed_buttons,
                            ..
                        }) = e.pointer_sample.clone()
                        {
                            // TODO(https://fxbug.dev/42172483): Relative motion is currently ignored.
                            if let Some(relative_motion) = &relative_motion {
                                info!(
                                    "https://fxbug.dev/42172483: Relative motion ignored: {:?}",
                                    relative_motion
                                );
                            }

                            // We expect `view_parameters` to have already been received before any
                            // pointer samples are received.
                            // TODO(https://fxbug.dev/42172483): We currently ignore these parameters, but real
                            // apps/frameworks will want to apply the `viewport_to_view_transform`
                            // instead of passing `position_in_viewport` through unchanged.
                            if let Some(_view_parameters) = &view_parameters {
                                if let Err(e) = sender.unbounded_send(InternalMessage::MouseEvent {
                                    timestamp,
                                    trace_flow_id,
                                    position_in_viewport,
                                    scroll_v,
                                    scroll_h,
                                    pressed_buttons,
                                }) {
                                    error!("Failed to send MouseEvent message: {}", e);
                                    return;
                                }
                            } else {
                                panic!("Expected ViewParameters before MousePointerSample");
                            }
                        }

                        // Handle `stream_info` field, if it exists.
                        if let Some(fptr::MouseEventStreamInfo { device_id, status }) =
                            e.stream_info.clone()
                        {
                            info!(%device_id, ?status, "Unhandled MouseEventStreamInfo");
                        }
                    }
                }
                _ => {
                    info!("MouseSource connection closed");
                    return;
                }
            }
        }
    })
    .detach();
}
