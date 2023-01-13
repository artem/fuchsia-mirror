// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_input_report::InputDeviceGetFeatureReportResult;

use {
    crate::input_reports_reader::InputReportsReader,
    anyhow::{format_err, Context as _, Error},
    fidl::endpoints::ServerEnd,
    fidl::Error as FidlError,
    fidl_fuchsia_input_report::{
        DeviceDescriptor, FeatureReport, InputDeviceRequest, InputDeviceRequestStream, InputReport,
        InputReportsReaderMarker,
    },
    fuchsia_async as fasync,
    futures::{future, pin_mut, StreamExt, TryFutureExt},
};

/// Implements the server side of the
/// `fuchsia.input.report.InputDevice` FIDL protocol. This struct also enables users to inject
/// input reports `as fuchsia.ui.input.InputReport`.
///
/// # Notes
/// * Some of the methods of `fuchsia.input.report.InputDevice` are not relevant to
///   input injection, so this implemnentation does not support them:
///   * `SendOutputReport` provides a way to change keyboard LED state.
///   If these FIDL methods are invoked, `InputDevice::flush()` will resolve to Err.
/// * This implementation does not support multiple calls to `GetInputReportsReader`,
///   since:
///   * The ideal semantics for multiple calls are not obvious, and
///   * Each `InputDevice` has a single FIDL client (an input pipeline implementation),
///     and the current input pipeline implementation is happy to use a single
///     `InputReportsReader` for the lifetime of the `InputDevice`.
pub(crate) struct InputDevice {
    /// FIFO queue of reports to be consumed by calls to
    /// `fuchsia.input.report.InputReportsReader.ReadInputReports()`.
    /// Populated by `input_device::InputDevice`.
    report_sender: futures::channel::mpsc::UnboundedSender<InputReport>,

    /// `Task` to keep serving the `fuchsia.input.report.InputDevice` protocol.
    _input_device_task: fasync::Task<Result<(), Error>>,
}

impl InputDevice {
    /// Creates a new `InputDevice` that will create a task to:
    /// a) process requests from `request_stream`, and
    /// b) respond to `GetDescriptor` calls with the descriptor generated by `descriptor_generator()`
    pub(super) fn new(
        request_stream: InputDeviceRequestStream,
        descriptor: DeviceDescriptor,
    ) -> Self {
        let (report_sender, report_receiver) = futures::channel::mpsc::unbounded::<InputReport>();

        // Create a `Task` to keep serving the `fuchsia.input.report.InputDevice` protocol.
        let input_device_task =
            fasync::Task::local(Self::serve_reports(request_stream, descriptor, report_receiver));

        Self { report_sender, _input_device_task: input_device_task }
    }

    /// Enqueues an input report, to be read by the input reports reader.
    pub(super) fn send_input_report(&self, input_report: InputReport) -> Result<(), Error> {
        self.report_sender
            .unbounded_send(input_report)
            .context("failed to send input report to reader")
    }

    /// Returns a `Future` which resolves when all input reports for this device
    /// have been sent to the FIDL peer, or when an error occurs.
    ///
    /// The possible errors are implementation-specific, but may include:
    /// * Errors reading from the FIDL peer
    /// * Errors writing to the FIDL peer
    ///
    /// # Resolves to
    /// * `Ok(())` if all reports were written successfully
    /// * `Err` otherwise
    ///
    /// # Note
    /// When the future resolves, input reports may still be sitting unread in the
    /// channel to the FIDL peer.
    #[cfg(test)]
    pub(super) async fn flush(self) -> Result<(), Error> {
        let Self { _input_device_task: input_device_task, report_sender } = self;
        std::mem::drop(report_sender); // Drop `report_sender` to close channel.
        input_device_task.await
    }

    /// Returns a `Future` which resolves when all `InputReport`s for this device
    /// have been sent to a `fuchsia.input.InputReportsReader` client, or when
    /// an error occurs.
    ///
    /// # Resolves to
    /// * `Ok(())` if all reports were written successfully
    /// * `Err` otherwise. For example:
    ///   * The `fuchsia.input.InputDevice` client sent an invalid request.
    ///   * A FIDL error occurred while trying to read a FIDL request.
    ///   * A FIDL error occurred while trying to write a FIDL response.
    ///
    /// # Corner cases
    /// Resolves to `Err` if the `fuchsia.input.InputDevice` client did not call
    /// `GetInputReportsReader()`, even if no `InputReport`s were queued.
    ///
    /// # Note
    /// When the `Future` resolves, `InputReports` may still be sitting unread in the
    /// channel to the `fuchsia.input.InputReportsReader` client. (The client will
    /// typically be an input pipeline implementation.)
    async fn serve_reports(
        request_stream: InputDeviceRequestStream,
        descriptor: DeviceDescriptor,
        report_receiver: futures::channel::mpsc::UnboundedReceiver<InputReport>,
    ) -> Result<(), Error> {
        // Process `fuchsia.input.report.InputDevice` requests, waiting for the `InputDevice`
        // client to provide a `ServerEnd<InputReportsReader>` by calling `GetInputReportsReader()`.
        let mut input_reports_reader_server_end_stream = request_stream
            .filter_map(|r| future::ready(Self::handle_device_request(r, &descriptor)));
        let input_reports_reader_fut = {
            let reader_server_end = input_reports_reader_server_end_stream
                .next()
                .await
                .ok_or(format_err!("stream ended without a call to GetInputReportsReader"))?
                .context("handling InputDeviceRequest")?;
            InputReportsReader {
                request_stream: reader_server_end
                    .into_stream()
                    .context("converting ServerEnd<InputReportsReader>")?,
                report_receiver,
            }
            .into_future()
        };
        pin_mut!(input_reports_reader_fut);

        // Create a `Future` to keep serving the `fuchsia.input.report.InputDevice` protocol.
        // This time, receiving a `ServerEnd<InputReportsReaderMarker>` will be an `Err`.
        let input_device_server_fut = async {
            match input_reports_reader_server_end_stream.next().await {
                Some(Ok(_server_end)) => {
                    // There are no obvious "best" semantics for how to handle multiple
                    // `GetInputReportsReader` calls, and there is no current need to
                    // do so. Instead of taking a guess at what the client might want
                    // in such a case, just return `Err`.
                    Err(format_err!(
                        "InputDevice does not support multiple GetInputReportsReader calls"
                    ))
                }
                Some(Err(e)) => Err(e.context("handling InputDeviceRequest")),
                None => Ok(()),
            }
        };
        pin_mut!(input_device_server_fut);

        // Now, process both `fuchsia.input.report.InputDevice` requests, and
        // `fuchsia.input.report.InputReportsReader` requests. And keep processing
        // `InputReportsReader` requests even if the `InputDevice` connection
        // is severed.
        future::select(
            input_device_server_fut.and_then(|_: ()| future::pending()),
            input_reports_reader_fut,
        )
        .await
        .factor_first()
        .0
    }

    /// Processes a single request from an `InputDeviceRequestStream`
    ///
    /// # Returns
    /// * Some(Ok(ServerEnd<InputReportsReaderMarker>)) if the request yielded an
    ///   `InputReportsReader`. `InputDevice` should route its `InputReports` to the yielded
    ///   `InputReportsReader`.
    /// * Some(Err) if the request yielded an `Error`
    /// * None if the request was fully processed by `handle_device_request()`
    fn handle_device_request(
        request: Result<InputDeviceRequest, FidlError>,
        descriptor: &DeviceDescriptor,
    ) -> Option<Result<ServerEnd<InputReportsReaderMarker>, Error>> {
        match request {
            Ok(InputDeviceRequest::GetInputReportsReader { reader: reader_server_end, .. }) => {
                Some(Ok(reader_server_end))
            }
            Ok(InputDeviceRequest::GetDescriptor { responder }) => {
                match responder.send(descriptor.clone()) {
                    Ok(()) => None,
                    Err(e) => {
                        Some(Err(anyhow::Error::from(e).context("sending GetDescriptor response")))
                    }
                }
            }
            Ok(InputDeviceRequest::GetFeatureReport { responder }) => {
                let mut result: InputDeviceGetFeatureReportResult = Ok(FeatureReport::EMPTY);
                match responder.send(&mut result) {
                    Ok(()) => None,
                    Err(e) => Some(Err(
                        anyhow::Error::from(e).context("sending GetFeatureReport response")
                    )),
                }
            }
            Err(e) => {
                // Fail fast.
                //
                // Panic here, since we don't have a good way to report an error from a
                // background task. InputDevice::flush() exists, but this is unlikely
                // to be called in tests, and it may get called way too late, after
                // an error in this background task already caused some other error.
                panic!("InputDevice got an error while reading request: {:?}", &e);
            }
            _ => {
                // See the previous branch.
                panic!(
                    "InputDevice::handle_device_request does not support this request: {:?}",
                    &request
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints,
        fidl_fuchsia_input_report::{DeviceDescriptor, InputDeviceMarker},
        fuchsia_async as fasync,
    };

    mod responds_to_get_feature_report_request {
        use super::*;

        #[fasync::run_until_stalled(test)]
        async fn single_request_before_call_to_get_feature_report() -> Result<(), Error> {
            let (proxy, request_stream) = endpoints::create_proxy_and_stream::<InputDeviceMarker>()
                .context("creating InputDevice proxy and stream")?;
            let input_device_server_fut =
                Box::new(InputDevice::new(request_stream, DeviceDescriptor::EMPTY)).flush();
            let get_feature_report_fut = proxy.get_feature_report();
            std::mem::drop(proxy); // Drop `proxy` to terminate `request_stream`.

            let (_, get_feature_report_result) =
                future::join(input_device_server_fut, get_feature_report_fut).await;
            assert_eq!(get_feature_report_result.context("fidl error")?, Ok(FeatureReport::EMPTY));
            Ok(())
        }
    }

    mod responds_to_get_descriptor_request {
        use {
            super::{
                utils::{make_input_device_proxy_and_struct, make_touchscreen_descriptor},
                *,
            },
            assert_matches::assert_matches,
            fidl_fuchsia_input_report::InputReportsReaderMarker,
            futures::task::Poll,
        };

        #[fasync::run_until_stalled(test)]
        async fn single_request_before_call_to_get_input_reports_reader() -> Result<(), Error> {
            let (proxy, request_stream) = endpoints::create_proxy_and_stream::<InputDeviceMarker>()
                .context("creating InputDevice proxy and stream")?;
            let input_device_server_fut =
                Box::new(InputDevice::new(request_stream, make_touchscreen_descriptor())).flush();
            let get_descriptor_fut = proxy.get_descriptor();
            std::mem::drop(proxy); // Drop `proxy` to terminate `request_stream`.

            let (_, get_descriptor_result) =
                future::join(input_device_server_fut, get_descriptor_fut).await;
            assert_eq!(get_descriptor_result.context("fidl error")?, make_touchscreen_descriptor());
            Ok(())
        }

        #[test]
        fn multiple_requests_before_call_to_get_input_reports_reader() -> Result<(), Error> {
            let mut executor = fasync::TestExecutor::new();
            let (proxy, request_stream) = endpoints::create_proxy_and_stream::<InputDeviceMarker>()
                .context("creating InputDevice proxy and stream")?;
            let input_device_server_fut =
                Box::new(InputDevice::new(request_stream, make_touchscreen_descriptor())).flush();
            pin_mut!(input_device_server_fut);

            let mut get_descriptor_fut = proxy.get_descriptor();
            assert_matches!(
                executor.run_until_stalled(&mut input_device_server_fut),
                Poll::Pending
            );
            std::mem::drop(executor.run_until_stalled(&mut get_descriptor_fut));

            let mut get_descriptor_fut = proxy.get_descriptor();
            let _ = executor.run_until_stalled(&mut input_device_server_fut);
            assert_matches!(
                executor.run_until_stalled(&mut get_descriptor_fut),
                Poll::Ready(Ok(_))
            );

            Ok(())
        }

        #[test]
        fn after_call_to_get_input_reports_reader_with_report_pending() -> Result<(), Error> {
            let mut executor = fasync::TestExecutor::new();
            let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
            input_device
                .send_input_report(InputReport {
                    event_time: None,
                    touch: None,
                    ..InputReport::EMPTY
                })
                .context("internal error queuing input event")?;

            let input_device_server_fut = input_device.flush();
            pin_mut!(input_device_server_fut);

            let (_input_reports_reader_proxy, input_reports_reader_server_end) =
                endpoints::create_proxy::<InputReportsReaderMarker>()
                    .context("internal error creating InputReportsReader proxy and server end")?;
            input_device_proxy
                .get_input_reports_reader(input_reports_reader_server_end)
                .context("sending get_input_reports_reader request")?;
            assert_matches!(
                executor.run_until_stalled(&mut input_device_server_fut),
                Poll::Pending
            );

            let mut get_descriptor_fut = input_device_proxy.get_descriptor();
            assert_matches!(
                executor.run_until_stalled(&mut input_device_server_fut),
                Poll::Pending
            );
            assert_matches!(executor.run_until_stalled(&mut get_descriptor_fut), Poll::Ready(_));
            Ok(())
        }
    }

    mod future_resolution {
        use {
            super::{
                utils::{make_input_device_proxy_and_struct, make_input_reports_reader_proxy},
                *,
            },
            futures::task::Poll,
        };

        mod yields_ok_after_all_reports_are_sent_to_input_reports_reader {
            use {super::*, assert_matches::assert_matches};

            #[test]
            fn if_device_request_channel_was_closed() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_reports_reader_proxy =
                    make_input_reports_reader_proxy(&input_device_proxy);
                input_device
                    .send_input_report(InputReport {
                        event_time: None,
                        touch: None,
                        ..InputReport::EMPTY
                    })
                    .expect("queuing input report");

                let _input_reports_fut = input_reports_reader_proxy.read_input_reports();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                std::mem::drop(input_device_proxy); // Close device request channel.
                assert_matches!(
                    executor.run_until_stalled(&mut input_device_fut),
                    Poll::Ready(Ok(()))
                );
            }

            #[test]
            fn even_if_device_request_channel_is_open() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_reports_reader_proxy =
                    make_input_reports_reader_proxy(&input_device_proxy);
                input_device
                    .send_input_report(InputReport {
                        event_time: None,
                        touch: None,
                        ..InputReport::EMPTY
                    })
                    .expect("queuing input report");

                let _input_reports_fut = input_reports_reader_proxy.read_input_reports();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                assert_matches!(
                    executor.run_until_stalled(&mut input_device_fut),
                    Poll::Ready(Ok(()))
                );
            }

            #[test]
            fn even_if_reports_was_empty_and_device_request_channel_is_open() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_reports_reader_proxy =
                    make_input_reports_reader_proxy(&input_device_proxy);
                let _input_reports_fut = input_reports_reader_proxy.read_input_reports();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                assert_matches!(
                    executor.run_until_stalled(&mut input_device_fut),
                    Poll::Ready(Ok(()))
                );
            }
        }

        mod yields_err_if_peer_closed_device_channel_without_calling_get_input_reports_reader {
            use super::*;
            use assert_matches::assert_matches;

            #[test]
            fn if_reports_were_available() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                input_device
                    .send_input_report(InputReport {
                        event_time: None,
                        touch: None,
                        ..InputReport::EMPTY
                    })
                    .expect("queuing input report");

                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                std::mem::drop(input_device_proxy);
                assert_matches!(
                    executor.run_until_stalled(&mut input_device_fut),
                    Poll::Ready(Err(_))
                )
            }

            #[test]
            fn even_if_no_reports_were_available() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                std::mem::drop(input_device_proxy);
                assert_matches!(
                    executor.run_until_stalled(&mut input_device_fut),
                    Poll::Ready(Err(_))
                )
            }
        }

        mod is_pending_if_peer_has_device_channel_open_and_has_not_called_get_input_reports_reader {
            use super::*;
            use assert_matches::assert_matches;

            #[test]
            fn if_reports_were_available() {
                let mut executor = fasync::TestExecutor::new();
                let (_input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                input_device
                    .send_input_report(InputReport {
                        event_time: None,
                        touch: None,
                        ..InputReport::EMPTY
                    })
                    .expect("queuing input report");

                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                assert_matches!(executor.run_until_stalled(&mut input_device_fut), Poll::Pending)
            }

            #[test]
            fn even_if_no_reports_were_available() {
                let mut executor = fasync::TestExecutor::new();
                let (_input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                assert_matches!(executor.run_until_stalled(&mut input_device_fut), Poll::Pending)
            }

            #[test]
            fn even_if_get_device_descriptor_has_been_called() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                let _get_descriptor_fut = input_device_proxy.get_descriptor();
                assert_matches!(executor.run_until_stalled(&mut input_device_fut), Poll::Pending)
            }
        }

        mod is_pending_if_peer_has_not_read_any_reports_when_a_report_is_available {
            use super::*;
            use assert_matches::assert_matches;

            #[test]
            fn if_device_request_channel_is_open() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let _input_reports_reader_proxy =
                    make_input_reports_reader_proxy(&input_device_proxy);
                input_device
                    .send_input_report(InputReport {
                        event_time: None,
                        touch: None,
                        ..InputReport::EMPTY
                    })
                    .expect("queuing input report");

                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                assert_matches!(executor.run_until_stalled(&mut input_device_fut), Poll::Pending)
            }

            #[test]
            fn even_if_device_channel_is_closed() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let _input_reports_reader_proxy =
                    make_input_reports_reader_proxy(&input_device_proxy);
                input_device
                    .send_input_report(InputReport {
                        event_time: None,
                        touch: None,
                        ..InputReport::EMPTY
                    })
                    .expect("queuing input report");

                let input_device_fut = input_device.flush();
                std::mem::drop(input_device_proxy); // Terminate `InputDeviceRequestStream`.
                pin_mut!(input_device_fut);
                assert_matches!(executor.run_until_stalled(&mut input_device_fut), Poll::Pending)
            }
        }

        mod is_pending_if_peer_did_not_read_all_reports {
            use {
                super::*, assert_matches::assert_matches,
                fidl_fuchsia_input_report::MAX_DEVICE_REPORT_COUNT,
            };

            #[test]
            fn if_device_request_channel_is_open() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_reports_reader_proxy =
                    make_input_reports_reader_proxy(&input_device_proxy);
                (0..=MAX_DEVICE_REPORT_COUNT).for_each(|_| {
                    input_device
                        .send_input_report(InputReport {
                            event_time: None,
                            touch: None,
                            ..InputReport::EMPTY
                        })
                        .expect("queuing input report");
                });

                // One query isn't enough to consume all of the reports queued above.
                let _input_reports_fut = input_reports_reader_proxy.read_input_reports();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                assert_matches!(executor.run_until_stalled(&mut input_device_fut), Poll::Pending)
            }

            #[test]
            fn even_if_device_request_channel_is_closed() {
                let mut executor = fasync::TestExecutor::new();
                let (input_device_proxy, input_device) = make_input_device_proxy_and_struct();
                let input_reports_reader_proxy =
                    make_input_reports_reader_proxy(&input_device_proxy);
                (0..=MAX_DEVICE_REPORT_COUNT).for_each(|_| {
                    input_device
                        .send_input_report(InputReport {
                            event_time: None,
                            touch: None,
                            ..InputReport::EMPTY
                        })
                        .expect("queuing input report");
                });

                // One query isn't enough to consume all of the reports queued above.
                let _input_reports_fut = input_reports_reader_proxy.read_input_reports();
                let input_device_fut = input_device.flush();
                pin_mut!(input_device_fut);
                std::mem::drop(input_device_proxy); // Terminate `InputDeviceRequestStream`.
                assert_matches!(executor.run_until_stalled(&mut input_device_fut), Poll::Pending)
            }
        }
    }

    mod utils {
        use {
            super::*,
            fidl_fuchsia_input_report::{
                Axis, ContactInputDescriptor, DeviceDescriptor, InputDeviceMarker,
                InputDeviceProxy, InputReportsReaderMarker, InputReportsReaderProxy, Range,
                TouchDescriptor, TouchInputDescriptor, TouchType, Unit, UnitType,
            },
            //fuchsia_zircon as zx,
        };

        /// Creates a `DeviceDescriptor` for a touchscreen that spans [-1000, 1000] on both axes.
        pub(super) fn make_touchscreen_descriptor() -> DeviceDescriptor {
            DeviceDescriptor {
                touch: Some(TouchDescriptor {
                    input: Some(TouchInputDescriptor {
                        contacts: Some(
                            std::iter::repeat(ContactInputDescriptor {
                                position_x: Some(Axis {
                                    range: Range { min: -1000, max: 1000 },
                                    unit: Unit { type_: UnitType::Other, exponent: 0 },
                                }),
                                position_y: Some(Axis {
                                    range: Range { min: -1000, max: 1000 },
                                    unit: Unit { type_: UnitType::Other, exponent: 0 },
                                }),
                                contact_width: Some(Axis {
                                    range: Range { min: -1000, max: 1000 },
                                    unit: Unit { type_: UnitType::Other, exponent: 0 },
                                }),
                                contact_height: Some(Axis {
                                    range: Range { min: -1000, max: 1000 },
                                    unit: Unit { type_: UnitType::Other, exponent: 0 },
                                }),
                                ..ContactInputDescriptor::EMPTY
                            })
                            .take(10)
                            .collect(),
                        ),
                        max_contacts: Some(10),
                        touch_type: Some(TouchType::Touchscreen),
                        buttons: Some(vec![]),
                        ..TouchInputDescriptor::EMPTY
                    }),
                    ..TouchDescriptor::EMPTY
                }),
                ..DeviceDescriptor::EMPTY
            }
        }

        /// Creates an `InputDeviceProxy`, for sending `fuchsia.input.report.InputDevice`
        /// requests, and an `InputDevice` struct that will receive the FIDL requests
        /// from the `InputDeviceProxy`.
        ///
        /// # Returns
        /// A tuple of the proxy and struct. The struct is `Box`-ed so that the caller
        /// can easily invoke `flush()`.
        pub(super) fn make_input_device_proxy_and_struct() -> (InputDeviceProxy, Box<InputDevice>) {
            let (input_device_proxy, input_device_request_stream) =
                endpoints::create_proxy_and_stream::<InputDeviceMarker>()
                    .expect("creating InputDevice proxy and stream");
            let input_device =
                Box::new(InputDevice::new(input_device_request_stream, DeviceDescriptor::EMPTY));
            (input_device_proxy, input_device)
        }

        /// Creates an `InputReportsReaderProxy`, for sending
        /// `fuchsia.input.report.InputReportsReader` requests, and registers that
        /// `InputReportsReader` with the `InputDevice` bound to `InputDeviceProxy`.
        ///
        /// # Returns
        /// The newly created `InputReportsReaderProxy`.
        pub(super) fn make_input_reports_reader_proxy(
            input_device_proxy: &InputDeviceProxy,
        ) -> InputReportsReaderProxy {
            let (input_reports_reader_proxy, input_reports_reader_server_end) =
                endpoints::create_proxy::<InputReportsReaderMarker>()
                    .expect("internal error creating InputReportsReader proxy and server end");
            input_device_proxy
                .get_input_reports_reader(input_reports_reader_server_end)
                .expect("sending get_input_reports_reader request");
            input_reports_reader_proxy
        }
    }
}
