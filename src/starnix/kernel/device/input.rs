// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{framebuffer::Framebuffer, DeviceMode},
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable,
        kobject::KObjectDeviceAttribute,
        FdEvents, FileObject, FileOps, FsNode,
    },
    logging::{log_info, log_warn, not_implemented},
    mm::MemoryAccessorExt,
    syscalls::{SyscallArg, SyscallResult, SUCCESS},
    task::{CurrentTask, EventHandler, Kernel, WaitCanceler, WaitQueue, Waiter},
    types::time::timeval_from_time,
    types::user_address::{UserAddress, UserRef},
    types::{
        errno::{error, Errno},
        timeval, uapi, DeviceType, OpenFlags, ABS_CNT, ABS_X, ABS_Y, BTN_MISC, BTN_TOOL_FINGER,
        BTN_TOUCH, BUS_VIRTUAL, FF_CNT, INPUT_MAJOR, INPUT_PROP_CNT, INPUT_PROP_DIRECT,
        KEYBOARD_INPUT_MINOR, KEY_CNT, KEY_POWER, LED_CNT, MSC_CNT, REL_CNT, SW_CNT,
        TOUCH_INPUT_MINOR,
    },
};

use fidl::endpoints::Proxy as _; // for `on_closed()`
use fidl::handle::fuchsia_handles::Signals;
use fidl_fuchsia_ui_input::MediaButtonsEvent;
use fidl_fuchsia_ui_input3 as fuiinput;
use fidl_fuchsia_ui_pointer::{
    self as fuipointer, EventPhase as FidlEventPhase, TouchEvent as FidlTouchEvent,
    TouchResponse as FidlTouchResponse, TouchResponseType,
};
use fidl_fuchsia_ui_policy as fuipolicy;
use fidl_fuchsia_ui_views as fuiviews;
use fuchsia_async as fasync;
use fuchsia_inspect::{self, health::Reporter, NumericProperty};
use fuchsia_zircon as zx;
use futures::{
    future::{self, Either},
    StreamExt as _,
};
use starnix_lock::Mutex;
use std::{collections::VecDeque, sync::Arc};
use zerocopy::AsBytes as _; // for `as_bytes()`

pub struct InputDevice {
    // Right now the input device assumes that there is only one input file, and that it represents
    // a touch input device. This structure will soon change to support multiple input files.
    pub touch_input_file: Arc<InputFile>,

    pub keyboard_input_file: Arc<InputFile>,
}

impl InputDevice {
    pub fn new(framebuffer: Arc<Framebuffer>, kernel_node: &fuchsia_inspect::Node) -> Arc<Self> {
        let fb_state = framebuffer.info.read();
        let input_files_node = kernel_node.create_child("input_files");
        let touch_input_file = InputFile::new_touch(
            fb_state.xres.try_into().expect("Failed to convert framebuffer width to i32."),
            fb_state.yres.try_into().expect("Failed to convert framebuffer height to i32."),
            input_files_node.create_child("touch_input_file"),
        );
        let keyboard_input_file = InputFile::new_keyboard();
        kernel_node.record(input_files_node);
        std::mem::drop(fb_state);
        Arc::new(InputDevice { touch_input_file, keyboard_input_file })
    }

    pub fn start_relay(
        &self,
        touch_source_proxy: fuipointer::TouchSourceProxy,
        keyboard: fuiinput::KeyboardProxy,
        registry_proxy: fuipolicy::DeviceListenerRegistryProxy,
        view_ref: fuiviews::ViewRef,
    ) {
        self.touch_input_file.start_touch_relay(touch_source_proxy);
        self.keyboard_input_file.clone().start_keyboard_relay(keyboard, view_ref);
        self.keyboard_input_file.clone().start_buttons_relay(registry_proxy);
    }
}

fn create_touch_device(
    current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(current_task.kernel().input_device.touch_input_file.clone()))
}

fn create_keyboard_device(
    current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(current_task.kernel().input_device.keyboard_input_file.clone()))
}

pub struct InspectStatus {
    /// A node that contains the state below.
    _inspect_node: fuchsia_inspect::Node,

    /// The number of FIDL events received from Fuchsia input system.
    fidl_events_received_count: fuchsia_inspect::UintProperty,

    /// The number of FIDL events converted to this module’s representation of TouchEvent.
    fidl_events_converted_count: fuchsia_inspect::UintProperty,

    /// The number of uapi::input_events generated from TouchEvents.
    uapi_events_generated_count: fuchsia_inspect::UintProperty,

    /// The number of uapi::input_events read from this touch file by external process.
    uapi_events_read_count: fuchsia_inspect::UintProperty,

    // Health status of this InputFile. UNHEALTHY if InputFile loses connection to TouchSource protocol
    pub health_node: fuchsia_inspect::health::Node,
}

impl InspectStatus {
    fn new(node: fuchsia_inspect::Node) -> Self {
        let mut health_node = fuchsia_inspect::health::Node::new(&node);
        health_node.set_starting_up();
        let fidl_events_received_count = node.create_uint("fidl_events_received_count", 0);
        let fidl_events_converted_count = node.create_uint("fidl_events_converted_count", 0);
        let uapi_events_generated_count = node.create_uint("uapi_events_generated_count", 0);
        let uapi_events_read_count = node.create_uint("uapi_events_read_count", 0);
        Self {
            _inspect_node: node,
            fidl_events_received_count,
            fidl_events_converted_count,
            uapi_events_generated_count,
            uapi_events_read_count,
            health_node,
        }
    }

    fn count_received_events(&self, count: u64) {
        self.fidl_events_received_count.add(count);
    }

    fn count_converted_events(&self, count: u64) {
        self.fidl_events_converted_count.add(count);
    }

    fn count_generated_events(&self, count: u64) {
        self.uapi_events_generated_count.add(count);
    }

    fn count_read_events(&self, count: u64) {
        self.uapi_events_read_count.add(count);
    }
}

pub struct InputFile {
    driver_version: u32,
    input_id: uapi::input_id,
    supported_keys: BitSet<{ min_bytes(KEY_CNT) }>,
    supported_position_attributes: BitSet<{ min_bytes(ABS_CNT) }>, // ABSolute position
    supported_motion_attributes: BitSet<{ min_bytes(REL_CNT) }>,   // RELative motion
    supported_switches: BitSet<{ min_bytes(SW_CNT) }>,
    supported_leds: BitSet<{ min_bytes(LED_CNT) }>,
    supported_haptics: BitSet<{ min_bytes(FF_CNT) }>, // 'F'orce 'F'eedback
    supported_misc_features: BitSet<{ min_bytes(MSC_CNT) }>,
    properties: BitSet<{ min_bytes(INPUT_PROP_CNT) }>,
    x_axis_info: uapi::input_absinfo,
    y_axis_info: uapi::input_absinfo,
    inner: Mutex<InputFileMutableState>,
}

// Mutable state of `InputFile`
struct InputFileMutableState {
    events: VecDeque<uapi::input_event>,
    waiters: WaitQueue,
    // TODO: fxb/131759 - remove `Optional` when implementing Inspect for Keyboard InputFiles
    // Touch InputFile will be initialized with a InspectStatus that holds Inspect data
    // `None` for Keyboard InputFile
    inspect_status: Option<InspectStatus>,
}

#[derive(Copy, Clone)]
// This module's representation of the information from `fuipointer::EventPhase`.
enum PhaseChange {
    Added,
    Removed,
}

// This module's representation of the information from `fuipointer::TouchEvent`.
struct TouchEvent {
    time: timeval,
    // Change in the contact state, if any. The phase change for a FIDL event reporting a move,
    // for example, will have `None` here.
    phase_change: Option<PhaseChange>,
    pos_x: f32,
    pos_y: f32,
}

impl InputFile {
    // Per https://www.linuxjournal.com/article/6429, the driver version is 32-bits wide,
    // and interpreted as:
    // * [31-16]: version
    // * [15-08]: minor
    // * [07-00]: patch level
    const DRIVER_VERSION: u32 = 0;

    // Per https://www.linuxjournal.com/article/6429, the bus type should be populated with a
    // sensible value, but other fields may not be.
    const INPUT_ID: uapi::input_id =
        uapi::input_id { bustype: BUS_VIRTUAL as u16, product: 0, vendor: 0, version: 0 };

    /// Creates an `InputFile` instance suitable for emulating a touchscreen.
    fn new_touch(width: i32, height: i32, inspect_node: fuchsia_inspect::Node) -> Arc<Self> {
        // Fuchsia scales the position reported by the touch sensor to fit view coordinates.
        // Hence, the range of touch positions is exactly the same as the range of view
        // coordinates.
        Arc::new(Self {
            driver_version: Self::DRIVER_VERSION,
            input_id: Self::INPUT_ID,
            supported_keys: touch_key_attributes(),
            supported_position_attributes: touch_position_attributes(),
            supported_motion_attributes: BitSet::new(), // None supported, not a mouse.
            supported_switches: BitSet::new(),          // None supported
            supported_leds: BitSet::new(),              // None supported
            supported_haptics: BitSet::new(),           // None supported
            supported_misc_features: BitSet::new(),     // None supported
            properties: touch_properties(),
            x_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: i32::from(width),
                // TODO(https://fxbug.dev/124595): `value` field should contain the most recent
                // X position.
                ..uapi::input_absinfo::default()
            },
            y_axis_info: uapi::input_absinfo {
                minimum: 0,
                maximum: i32::from(height),
                // TODO(https://fxbug.dev/124595): `value` field should contain the most recent
                // Y position.
                ..uapi::input_absinfo::default()
            },
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
                inspect_status: Some(InspectStatus::new(inspect_node)),
            }),
        })
    }

    fn new_keyboard() -> Arc<Self> {
        Arc::new(Self {
            driver_version: Self::DRIVER_VERSION,
            input_id: Self::INPUT_ID,
            supported_keys: keyboard_key_attributes(),
            supported_position_attributes: keyboard_position_attributes(),
            supported_motion_attributes: BitSet::new(), // None supported, not a mouse.
            supported_switches: BitSet::new(),          // None supported
            supported_leds: BitSet::new(),              // None supported
            supported_haptics: BitSet::new(),           // None supported
            supported_misc_features: BitSet::new(),     // None supported
            properties: keyboard_properties(),
            x_axis_info: uapi::input_absinfo::default(),
            y_axis_info: uapi::input_absinfo::default(),
            inner: Mutex::new(InputFileMutableState {
                events: VecDeque::new(),
                waiters: WaitQueue::default(),
                inspect_status: None,
            }),
        })
    }

    /// Starts reading events from the Fuchsia input system, and making those events available
    /// to the guest system.
    ///
    /// This method *should* be called as soon as possible after the `TouchSourceProxy` has been
    /// registered with the `TouchSource` server, as the server expects `TouchSource` clients to
    /// consume events in a timely manner.
    ///
    /// # Parameters
    /// * `touch_source_proxy`: a connection to the Fuchsia input system, which will provide
    ///   touch events associated with the Fuchsia `View` created by Starnix.
    fn start_touch_relay(
        self: &Arc<Self>,
        touch_source_proxy: fuipointer::TouchSourceProxy,
    ) -> std::thread::JoinHandle<()> {
        let slf = self.clone();
        std::thread::Builder::new()
            .name("kthread-touch-relay".to_string())
            .spawn(move || {
                fasync::LocalExecutor::new().run_singlethreaded(async {
                    slf.inner
                        .lock()
                        .inspect_status
                        .as_mut()
                        .map(|status| status.health_node.set_ok());
                    let mut previous_event_disposition = vec![];
                    // TODO(https://fxbug.dev/123718): Remove `close_fut`.
                    let mut close_fut = touch_source_proxy.on_closed();
                    loop {
                        let query_fut = touch_source_proxy.watch(&previous_event_disposition);
                        let query_res = match future::select(close_fut, query_fut).await {
                            Either::Left((Ok(Signals::CHANNEL_PEER_CLOSED), _)) => {
                                log_warn!("TouchSource server closed connection; input is stopped");
                                break;
                            }
                            Either::Left((Ok(signals), _))
                                if signals
                                    == (Signals::CHANNEL_PEER_CLOSED
                                        | Signals::CHANNEL_READABLE) =>
                            {
                                log_warn!(
                                    "{} {}",
                                    "TouchSource server closed connection and channel is readable",
                                    "input dropped event(s) and stopped"
                                );
                                break;
                            }
                            Either::Left((on_closed_res, _)) => {
                                log_warn!(
                                "on_closed() resolved with unexpected value {:?}; input is stopped",
                                on_closed_res
                            );
                                break;
                            }
                            Either::Right((query_res, pending_close_fut)) => {
                                close_fut = pending_close_fut;
                                query_res
                            }
                        };
                        match query_res {
                            Ok(touch_events) => {
                                let num_received_events: u64 =
                                    touch_events.len().try_into().unwrap();
                                previous_event_disposition =
                                    touch_events.iter().map(make_response_for_fidl_event).collect();
                                let converted_events =
                                    touch_events.iter().filter_map(parse_fidl_touch_event);
                                let num_converted_events: u64 =
                                    converted_events.clone().count().try_into().unwrap();
                                // `flat`: each FIDL event yields a `Vec<uapi::input_event>`,
                                // but the end result should be a single vector of UAPI events,
                                // not a `Vec<Vec<uapi::input_event>>`.
                                let new_events = converted_events.flat_map(make_uapi_events);
                                let num_generated_events: u64 =
                                    new_events.clone().count().try_into().unwrap();
                                let mut inner = slf.inner.lock();
                                match &inner.inspect_status {
                                    Some(inspect_status) => {
                                        inspect_status.count_received_events(num_received_events);
                                        inspect_status.count_converted_events(num_converted_events);
                                        inspect_status.count_generated_events(num_generated_events);
                                    }
                                    None => (),
                                }
                                // TODO(https://fxbug.dev/124597): Reading from an `InputFile` should
                                // not provide access to events that occurred before the file was
                                // opened.
                                inner.events.extend(new_events);
                                // TODO(https://fxbug.dev/124598): Skip notify if `inner.events`
                                // is empty.
                                inner.waiters.notify_fd_events(FdEvents::POLLIN);
                            }
                            Err(e) => {
                                log_warn!(
                                    "error {:?} reading from TouchSourceProxy; input is stopped",
                                    e
                                );
                                break;
                            }
                        };
                    }
                    // If we exit the loop this indicates relay is no longer active.
                    slf.inner.lock().inspect_status.as_mut().map(|status| {
                        status.health_node.set_unhealthy("Touch relay terminated unexpectedly")
                    });
                })
            })
            .expect("able to create threads")
    }

    fn start_keyboard_relay(
        self: &Arc<Self>,
        keyboard: fuiinput::KeyboardProxy,
        view_ref: fuiviews::ViewRef,
    ) -> std::thread::JoinHandle<()> {
        let slf = self.clone();
        std::thread::Builder::new()
            .name("kthread-keyboard-relay".to_string())
            .spawn(move || {
                fasync::LocalExecutor::new().run_singlethreaded(async {
                    let (keyboard_listener, mut event_stream) =
                        fidl::endpoints::create_request_stream::<fuiinput::KeyboardListenerMarker>(
                        )
                        .expect("Failed to create keyboard proxy and stream");
                    if keyboard.add_listener(view_ref, keyboard_listener).await.is_err() {
                        log_warn!("Could not register keyboard listener");
                    }
                    while let Some(Ok(request)) = event_stream.next().await {
                        match request {
                            fuiinput::KeyboardListenerRequest::OnKeyEvent { event, responder } => {
                                let new_events = parse_fidl_keyboard_event(&event);
                                let mut inner = slf.inner.lock();

                                inner.events.extend(new_events);
                                inner.waiters.notify_fd_events(FdEvents::POLLIN);

                                responder.send(fuiinput::KeyEventStatus::Handled).expect("");
                            }
                        }
                    }
                })
            })
            .expect("Failed to create thread")
    }

    fn start_buttons_relay(
        self: &Arc<Self>,
        registry_proxy: fuipolicy::DeviceListenerRegistryProxy,
    ) -> std::thread::JoinHandle<()> {
        let slf = self.clone();
        std::thread::Builder::new()
            .name("kthread-button-relay".to_string())
            .spawn(move || {
                fasync::LocalExecutor::new().run_singlethreaded(async {
                    // Create and register a listener to listen for MediaButtonsEvents from Input Pipeline.
                    let (listener, mut listener_stream) = fidl::endpoints::create_request_stream::<
                        fuipolicy::MediaButtonsListenerMarker,
                    >()
                    .unwrap();
                    if let Err(e) = registry_proxy.register_listener(listener).await {
                        log_warn!("Failed to register media buttons listener: {:?}", e);
                    }
                    while let Some(Ok(request)) = listener_stream.next().await {
                        match request {
                            fuipolicy::MediaButtonsListenerRequest::OnEvent {
                                event,
                                responder,
                            } => {
                                let new_events = parse_fidl_button_event(&event);

                                let mut inner = slf.inner.lock();
                                inner.events.extend(new_events);
                                inner.waiters.notify_fd_events(FdEvents::POLLIN);

                                responder
                                    .send()
                                    .expect("media buttons responder failed to respond");
                            }
                            _ => {}
                        }
                    }
                })
            })
            .expect("Failed to create thread")
    }
}

impl FileOps for Arc<InputFile> {
    fileops_impl_nonseekable!();

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            uapi::EVIOCGVERSION => {
                current_task.write_object(UserRef::new(user_addr), &self.driver_version)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGID => {
                current_task.write_object(UserRef::new(user_addr), &self.input_id)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_KEY => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_keys.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_ABS => {
                current_task.write_object(
                    UserRef::new(user_addr),
                    &self.supported_position_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_REL => {
                current_task.write_object(
                    UserRef::new(user_addr),
                    &self.supported_motion_attributes.bytes,
                )?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_SW => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_switches.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_LED => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_leds.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_FF => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_haptics.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGBIT_EV_MSC => {
                current_task
                    .mm
                    .write_object(UserRef::new(user_addr), &self.supported_misc_features.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGPROP => {
                current_task.write_object(UserRef::new(user_addr), &self.properties.bytes)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_X => {
                current_task.write_object(UserRef::new(user_addr), &self.x_axis_info)?;
                Ok(SUCCESS)
            }
            uapi::EVIOCGABS_Y => {
                current_task.write_object(UserRef::new(user_addr), &self.y_axis_info)?;
                Ok(SUCCESS)
            }
            _ => {
                not_implemented!("ioctl() {} on input device", request);
                error!(EOPNOTSUPP)
            }
        }
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        let mut inner = self.inner.lock();
        let event = inner.events.pop_front();
        match event {
            Some(event) => {
                inner.inspect_status.as_ref().map(|status| status.count_read_events(1));
                drop(inner);
                // TODO(https://fxbug.dev/124600): Consider sending as many events as will fit
                // in `data`, instead of sending them one at a time.
                data.write_all(event.as_bytes())
            }
            // TODO(https://fxbug.dev/124602): `EAGAIN` is only permitted if the file is opened
            // with `O_NONBLOCK`. Figure out what to do if the file is opened without that flag.
            None => {
                log_info!("read() returning EAGAIN");
                error!(EAGAIN)
            }
        }
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        not_implemented!("write() on input device");
        error!(EOPNOTSUPP)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.inner.lock().waiters.wait_async_fd_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(if self.inner.lock().events.is_empty() { FdEvents::empty() } else { FdEvents::POLLIN })
    }
}

struct BitSet<const NUM_BYTES: usize> {
    bytes: [u8; NUM_BYTES],
}

impl<const NUM_BYTES: usize> BitSet<{ NUM_BYTES }> {
    const fn new() -> Self {
        Self { bytes: [0; NUM_BYTES] }
    }

    fn set(&mut self, bitnum: u32) {
        let bitnum = bitnum as usize;
        let byte = bitnum / 8;
        let bit = bitnum % 8;
        self.bytes[byte] |= 1 << bit;
    }
}

/// Returns the minimum number of bytes required to store `n_bits` bits.
const fn min_bytes(n_bits: u32) -> usize {
    ((n_bits as usize) + 7) / 8
}

/// Returns appropriate `KEY`-board related flags for a touchscreen device.
fn touch_key_attributes() -> BitSet<{ min_bytes(KEY_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(BTN_TOUCH);
    attrs.set(BTN_TOOL_FINGER);
    attrs
}

/// Returns appropriate `ABS`-olute position related flags for a touchscreen device.
fn touch_position_attributes() -> BitSet<{ min_bytes(ABS_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(ABS_X);
    attrs.set(ABS_Y);
    attrs
}

/// Returns appropriate `INPUT_PROP`-erties for a touchscreen device.
fn touch_properties() -> BitSet<{ min_bytes(INPUT_PROP_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(INPUT_PROP_DIRECT);
    attrs
}

/// Returns appropriate `KEY`-board related flags for a keyboard device.
fn keyboard_key_attributes() -> BitSet<{ min_bytes(KEY_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(BTN_MISC);
    attrs.set(KEY_POWER);
    attrs
}

/// Returns appropriate `ABS`-olute position related flags for a keyboard device.
fn keyboard_position_attributes() -> BitSet<{ min_bytes(ABS_CNT) }> {
    BitSet::new()
}

/// Returns appropriate `INPUT_PROP`-erties for a keyboard device.
fn keyboard_properties() -> BitSet<{ min_bytes(INPUT_PROP_CNT) }> {
    let mut attrs = BitSet::new();
    attrs.set(INPUT_PROP_DIRECT);
    attrs
}

/// Returns a vector of `uapi::input_events` representing the provided `fidl_event`.
///
/// A single `KeyEvent` may translate into multiple `uapi::input_events`.
///
/// If translation fails an empty vector is returned.
fn parse_fidl_keyboard_event(fidl_event: &fuiinput::KeyEvent) -> Vec<uapi::input_event> {
    match fidl_event {
        &fuiinput::KeyEvent {
            timestamp: Some(time_nanos),
            type_: Some(event_type),
            key: Some(fidl_fuchsia_input::Key::Escape),
            ..
        } => {
            let time = timeval_from_time(zx::Time::from_nanos(time_nanos));
            let key_event = uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: uapi::KEY_POWER as u16,
                value: if event_type == fuiinput::KeyEventType::Pressed { 1 } else { 0 },
            };

            let sync_event = uapi::input_event {
                // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
                time,
                type_: uapi::EV_SYN as u16,
                code: uapi::SYN_REPORT as u16,
                value: 0,
            };

            let mut events = vec![];
            events.push(key_event);
            events.push(sync_event);
            events
        }
        _ => vec![],
    }
}

/// Returns `Some` if the FIDL event included a `timestamp`, and a `pointer_sample` which includes
/// both `position_in_viewport` and `phase`. Returns `None` otherwise.
fn parse_fidl_touch_event(fidl_event: &FidlTouchEvent) -> Option<TouchEvent> {
    match fidl_event {
        FidlTouchEvent {
            timestamp: Some(time_nanos),
            pointer_sample:
                Some(fuipointer::TouchPointerSample {
                    position_in_viewport: Some([x, y]),
                    phase: Some(phase),
                    ..
                }),
            ..
        } => Some(TouchEvent {
            time: timeval_from_time(zx::Time::from_nanos(*time_nanos)),
            phase_change: phase_change_from_fidl_phase(phase),
            pos_x: *x,
            pos_y: *y,
        }),
        _ => None, // TODO(https://fxbug.dev/124603): Add some inspect counters of ignored events.
    }
}

fn parse_fidl_button_event(fidl_event: &MediaButtonsEvent) -> Vec<uapi::input_event> {
    let time = timeval_from_time(zx::Time::get_monotonic());
    let mut events = vec![];
    let sync_event = uapi::input_event {
        // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
        time,
        type_: uapi::EV_SYN as u16,
        code: uapi::SYN_REPORT as u16,
        value: 0,
    };
    match fidl_event {
        &MediaButtonsEvent { power: Some(true), .. } => {
            let key_event = uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: uapi::KEY_POWER as u16,
                value: 1,
            };
            events.push(key_event);
            events.push(sync_event);
        }
        // Power button is released
        &MediaButtonsEvent { power: Some(false), .. } => {
            let key_event = uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: uapi::KEY_POWER as u16,
                value: 0,
            };
            events.push(key_event);
            events.push(sync_event);
        }
        _ => {}
    }

    events
}

/// Returns a FIDL response for `fidl_event`.
fn make_response_for_fidl_event(fidl_event: &FidlTouchEvent) -> FidlTouchResponse {
    match fidl_event {
        FidlTouchEvent { pointer_sample: Some(_), .. } => FidlTouchResponse {
            response_type: Some(TouchResponseType::Yes), // Event consumed by Starnix.
            trace_flow_id: fidl_event.trace_flow_id,
            ..Default::default()
        },
        _ => FidlTouchResponse::default(),
    }
}

/// Returns a sequence of UAPI input events which represent the data in `event`.
fn make_uapi_events(event: TouchEvent) -> Vec<uapi::input_event> {
    let sync_event = uapi::input_event {
        // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
        time: event.time,
        type_: uapi::EV_SYN as u16,
        code: uapi::SYN_REPORT as u16,
        value: 0,
    };
    let mut events = vec![];
    events.extend(make_contact_state_uapi_events(&event).iter().flatten());
    events.extend(make_position_uapi_events(&event));
    events.push(sync_event);
    events
}

/// Returns a sequence of UAPI input events representing `event`'s change in contact state,
/// if any.
fn make_contact_state_uapi_events(event: &TouchEvent) -> Option<[uapi::input_event; 2]> {
    event.phase_change.map(|phase_change| {
        let value = match phase_change {
            PhaseChange::Added => 1,
            PhaseChange::Removed => 0,
        };
        [
            uapi::input_event {
                time: event.time,
                type_: uapi::EV_KEY as u16,
                code: uapi::BTN_TOUCH as u16,
                value,
            },
            // TODO(https://fxbug.dev/124606): Reporting `BTN_TOOL_FINGER` here could cause some
            // programs to interpret this device as a touchpad, rather than a touchscreen. Also,
            // this isn't suitable if `InputFile` (eventually) supports multi-touch mode.
            // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
            uapi::input_event {
                time: event.time,
                type_: uapi::EV_KEY as u16,
                code: uapi::BTN_TOOL_FINGER as u16,
                value,
            },
        ]
    })
}

/// Returns a sequence of UAPI input events representing `event`'s contact location.
fn make_position_uapi_events(event: &TouchEvent) -> [uapi::input_event; 2] {
    [
        // The X coordinate.
        uapi::input_event {
            time: event.time,
            type_: uapi::EV_ABS as u16,
            code: uapi::ABS_X as u16,
            value: event.pos_x as i32,
        },
        // The Y coordinate.
        uapi::input_event {
            time: event.time,
            type_: uapi::EV_ABS as u16,
            code: uapi::ABS_Y as u16,
            value: event.pos_y as i32,
        },
    ]
}

/// Returns `Some` phase change if `fidl_phase` reports a change in contact state.
/// Returns `None` otherwise.
fn phase_change_from_fidl_phase(fidl_phase: &FidlEventPhase) -> Option<PhaseChange> {
    match fidl_phase {
        FidlEventPhase::Add => Some(PhaseChange::Added),
        FidlEventPhase::Change => None, // `Change` indicates position change only
        FidlEventPhase::Remove => Some(PhaseChange::Removed),
        // TODO(https://fxbug.dev/124607): Figure out whether this is correct.
        FidlEventPhase::Cancel => None,
    }
}

pub fn init_input_devices(kernel: &Arc<Kernel>) {
    let input_class =
        kernel.device_registry.add_class(b"input", kernel.device_registry.virtual_bus());
    let touch_attr = KObjectDeviceAttribute::new(
        input_class.clone(),
        b"event0",
        b"input/event0",
        DeviceType::new(INPUT_MAJOR, TOUCH_INPUT_MINOR),
        DeviceMode::Char,
    );
    kernel.add_and_register_device(touch_attr, create_touch_device);

    let keyboard_attr = KObjectDeviceAttribute::new(
        input_class,
        b"event1",
        b"input/event1",
        DeviceType::new(INPUT_MAJOR, KEYBOARD_INPUT_MINOR),
        DeviceMode::Char,
    );
    kernel.add_and_register_device(keyboard_attr, create_keyboard_device);
}

#[cfg(test)]
mod test {
    #![allow(clippy::unused_unit)] // for compatibility with `test_case`

    use super::*;
    use crate::{
        fs::{buffers::VecOutputBuffer, FileHandle},
        task::Kernel,
        testing::{create_kernel_and_task, map_memory, AutoReleasableTask},
        types::errno::EAGAIN,
    };
    use anyhow::anyhow;
    use assert_matches::assert_matches;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fuchsia_zircon as zx;
    use fuipointer::{
        EventPhase, TouchEvent, TouchPointerSample, TouchResponse, TouchSourceMarker,
        TouchSourceRequest,
    };
    use test_case::test_case;
    use test_util::assert_near;
    use zerocopy::FromBytes as _; // for `read_from()`

    const INPUT_EVENT_SIZE: usize = std::mem::size_of::<uapi::input_event>();

    fn make_touch_event() -> fuipointer::TouchEvent {
        // Default to `Change`, because that has the fewest side effects.
        make_touch_event_with_phase(EventPhase::Change)
    }

    fn make_touch_event_with_phase(phase: EventPhase) -> fuipointer::TouchEvent {
        TouchEvent {
            timestamp: Some(0),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([0.0, 0.0]),
                phase: Some(phase),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_touch_event_with_coords(x: f32, y: f32) -> fuipointer::TouchEvent {
        TouchEvent {
            timestamp: Some(0),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([x, y]),
                // Default to `Change`, because that has the fewest side effects.
                phase: Some(EventPhase::Change),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_touch_pointer_sample() -> TouchPointerSample {
        TouchPointerSample {
            position_in_viewport: Some([0.0, 0.0]),
            // Default to `Change`, because that has the fewest side effects.
            phase: Some(EventPhase::Change),
            ..Default::default()
        }
    }

    fn start_touch_input(
    ) -> (Arc<InputFile>, fuipointer::TouchSourceRequestStream, std::thread::JoinHandle<()>) {
        let inspector = fuchsia_inspect::Inspector::default();
        let input_file =
            InputFile::new_touch(720, 1200, inspector.root().create_child("touch_input_file"));
        let (touch_source_proxy, touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_touch_relay(touch_source_proxy);
        (input_file, touch_source_stream, relay_thread)
    }

    fn start_keyboard_input(
    ) -> (Arc<InputFile>, fuiinput::KeyboardRequestStream, std::thread::JoinHandle<()>) {
        let input_file = InputFile::new_keyboard();
        let (keyboard_proxy, keyboard_stream) =
            fidl::endpoints::create_proxy_and_stream::<fuiinput::KeyboardMarker>()
                .expect("failed to create Keyboard channel");
        let view_ref_pair =
            fuchsia_scenic::ViewRefPair::new().expect("Failed to create ViewRefPair");
        let relay_thread = input_file.start_keyboard_relay(keyboard_proxy, view_ref_pair.view_ref);
        (input_file, keyboard_stream, relay_thread)
    }

    fn start_button_input(
    ) -> (Arc<InputFile>, fuipolicy::DeviceListenerRegistryRequestStream, std::thread::JoinHandle<()>)
    {
        let input_file = InputFile::new_keyboard();
        let (device_registry_proxy, device_listener_stream) =
            fidl::endpoints::create_proxy_and_stream::<fuipolicy::DeviceListenerRegistryMarker>()
                .expect("Failed to create DeviceListenerRegistry proxy and stream.");
        let relay_thread = input_file.start_buttons_relay(device_registry_proxy);
        (input_file, device_listener_stream, relay_thread)
    }

    fn make_kernel_objects(file: Arc<InputFile>) -> (Arc<Kernel>, AutoReleasableTask, FileHandle) {
        let (kernel, current_task) = create_kernel_and_task();
        let file_object = FileObject::new(
            Box::new(file),
            // The input node doesn't really live at the root of the filesystem.
            // But the test doesn't need to be 100% representative of production.
            current_task
                .lookup_path_from_root(b".")
                .expect("failed to get namespace node for root"),
            OpenFlags::empty(),
        )
        .expect("FileObject::new failed");
        (kernel, current_task, file_object)
    }

    fn read_uapi_events(
        file: Arc<InputFile>,
        file_object: &FileObject,
        current_task: &CurrentTask,
    ) -> Vec<uapi::input_event> {
        std::iter::from_fn(|| {
            let mut event_bytes = VecOutputBuffer::new(INPUT_EVENT_SIZE);
            match file.read(file_object, current_task, 0, &mut event_bytes) {
                Ok(INPUT_EVENT_SIZE) => Some(
                    uapi::input_event::read_from(Vec::from(event_bytes).as_slice())
                        .ok_or(anyhow!("failed to read input_event from buffer")),
                ),
                Ok(other_size) => {
                    Some(Err(anyhow!("got {} bytes (expected {})", other_size, INPUT_EVENT_SIZE)))
                }
                Err(Errno { code: EAGAIN, .. }) => None,
                Err(other_error) => Some(Err(anyhow!("read failed: {:?}", other_error))),
            }
        })
        .enumerate()
        .map(|(i, read_res)| match read_res {
            Ok(event) => event,
            Err(e) => panic!("unexpected result {:?} on iteration {}", e, i),
        })
        .collect()
    }

    // Waits for a `Watch()` request to arrive on `request_stream`, and responds with
    // `touch_event`. Returns the arguments to the `Watch()` call.
    async fn answer_next_watch_request(
        request_stream: &mut fuipointer::TouchSourceRequestStream,
        touch_event: TouchEvent,
    ) -> Vec<TouchResponse> {
        match request_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                responder.send(&[touch_event]).expect("failure sending Watch reply");
                responses
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }
    }

    #[::fuchsia::test()]
    async fn initial_watch_request_has_empty_responses_arg() {
        // Set up resources.
        let (_input_file, mut touch_source_stream, relay_thread) = start_touch_input();

        // Verify that the watch request has empty `responses`.
        assert_matches!(
            touch_source_stream.next().await,
            Some(Ok(TouchSourceRequest::Watch { responses, .. }))
                => assert_eq!(responses.as_slice(), [])
        );

        // Cleanly tear down the client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("client thread failed"); // Wait for client thread to finish.
    }

    #[::fuchsia::test]
    async fn later_watch_requests_have_responses_arg_matching_earlier_watch_replies() {
        // Set up resources.
        let (_input_file, mut touch_source_stream, relay_thread) = start_touch_input();

        // Reply to first `Watch` with two `TouchEvent`s.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => responder
                .send(&vec![TouchEvent::default(); 2])
                .expect("failure sending Watch reply"),
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Verify second `Watch` has two elements in `responses`.
        // Then reply with five `TouchEvent`s.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                assert_matches!(responses.as_slice(), [_, _]);
                responder
                    .send(&vec![TouchEvent::default(); 5])
                    .expect("failure sending Watch reply")
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Verify third `Watch` has five elements in `responses`.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, .. })) => {
                assert_matches!(responses.as_slice(), [_, _, _, _, _]);
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Cleanly tear down the client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("client thread failed"); // Wait for client thread to finish.
    }

    #[::fuchsia::test]
    async fn notifies_polling_waiters_of_new_data() {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_touch_input();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Ask `input_file` to notify waiters when data is available to read.
        [&waiter1, &waiter2].iter().for_each(|waiter| {
            input_file.wait_async(
                &file_object,
                &current_task,
                waiter,
                FdEvents::POLLIN,
                EventHandler::None,
            );
        });
        assert_matches!(waiter1.wait_until(&current_task, zx::Time::ZERO), Err(_));
        assert_matches!(waiter2.wait_until(&current_task, zx::Time::ZERO), Err(_));

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_eq!(waiter1.wait_until(&current_task, zx::Time::ZERO), Ok(()));
        assert_eq!(waiter2.wait_until(&current_task, zx::Time::ZERO), Ok(()));

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[::fuchsia::test]
    async fn notifies_blocked_waiter_of_new_data() {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_touch_input();
        let waiter = Waiter::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Ask `input_file` to notify `waiter` when data is available to read.
        input_file.wait_async(
            &file_object,
            &current_task,
            &waiter,
            FdEvents::POLLIN,
            EventHandler::None,
        );

        let waiter_thread = std::thread::spawn(move || waiter.wait(&current_task));
        assert!(!waiter_thread.is_finished());

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        //
        // TODO(https://fxbug.dev/124609): Without this, `relay_thread` gets stuck `await`-ing
        // the reply to its first request. Figure out why that happens, and remove this second
        // reply.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Block until `waiter_thread` completes.
        waiter_thread.join().expect("join() failed").expect("wait() failed");

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    // Note: a user program may also want to be woken if events were already ready at the
    // time that the program called `epoll_wait()`. However, there's no test for that case
    // in this module, because:
    //
    // 1. Not all programs will want to be woken in such a case. In particular, some programs
    //    use "edge-triggered" mode instead of "level-tiggered" mode. For details on the
    //    two modes, see https://man7.org/linux/man-pages/man7/epoll.7.html.
    // 2. For programs using "level-triggered" mode, the relevant behavior is implemented in
    //    the `epoll` module, and verified by `epoll::tests::test_epoll_ready_then_wait()`.
    //
    // See also: the documentation for `FileOps::wait_async()`.

    #[::fuchsia::test]
    async fn honors_wait_cancellation() {
        // Set up input resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_touch_input();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Ask `input_file` to notify `waiter` when data is available to read.
        let waitkeys = [&waiter1, &waiter2]
            .iter()
            .map(|waiter| {
                input_file
                    .wait_async(
                        &file_object,
                        &current_task,
                        waiter,
                        FdEvents::POLLIN,
                        EventHandler::None,
                    )
                    .expect("wait_async")
            })
            .collect::<Vec<_>>();

        // Cancel wait for `waiter1`.
        waitkeys.into_iter().next().expect("failed to get first waitkey").cancel();

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // `InputFile` should be done processing the first reply, since it has sent its second
        // request. And, as part of processing the first reply, `InputFile` should have notified
        // the interested waiters.
        assert_matches!(waiter1.wait_until(&current_task, zx::Time::ZERO), Err(_));
        assert_eq!(waiter2.wait_until(&current_task, zx::Time::ZERO), Ok(()));

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[::fuchsia::test]
    async fn query_events() {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_touch_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Check initial expectation.
        assert_eq!(
            input_file.query_events(&file_object, &current_task).expect("query_events"),
            FdEvents::empty(),
            "events should be empty before data arrives"
        );

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Check post-watch expectation.
        assert_eq!(
            input_file.query_events(&file_object, &current_task).expect("query_events"),
            FdEvents::POLLIN,
            "events should be POLLIN after data arrives"
        );

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[test_case((1920, 1080), uapi::EVIOCGABS_X => (0, 1920))]
    #[test_case((1280, 1024), uapi::EVIOCGABS_Y => (0, 1024))]
    #[::fuchsia::test]
    async fn provides_correct_axis_ranges((x_max, y_max): (i32, i32), ioctl_op: u32) -> (i32, i32) {
        // Set up resources.
        let inspector = fuchsia_inspect::Inspector::default();
        let input_file =
            InputFile::new_touch(x_max, y_max, inspector.root().create_child("touch_input_file"));
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());
        let address = map_memory(
            &current_task,
            UserAddress::default(),
            std::mem::size_of::<uapi::input_absinfo>() as u64,
        );

        // Invoke ioctl() for axis details.
        input_file
            .ioctl(&file_object, &current_task, ioctl_op, address.into())
            .expect("ioctl() failed");

        // Extract minimum and maximum fields for validation.
        let axis_info = current_task
            .mm
            .read_object::<uapi::input_absinfo>(UserRef::new(address))
            .expect("failed to read user memory");
        (axis_info.minimum, axis_info.maximum)
    }

    #[test_case(EventPhase::Add, uapi::BTN_TOUCH => Some(1);
        "add_yields_btn_touch_true")]
    #[test_case(EventPhase::Change, uapi::BTN_TOUCH => None;
        "change_yields_no_btn_touch_event")]
    #[test_case(EventPhase::Remove, uapi::BTN_TOUCH => Some(0);
        "remove_yields_btn_touch_false")]
    #[test_case(EventPhase::Add, uapi::BTN_TOOL_FINGER => Some(1);
        "add_yields_btn_tool_finger_true")]
    #[test_case(EventPhase::Change, uapi::BTN_TOOL_FINGER => None;
        "change_yields_no_btn_tool_finger_event")]
    #[test_case(EventPhase::Remove, uapi::BTN_TOOL_FINGER => Some(0);
        "remove_yields_btn_tool_finger_false")]
    #[::fuchsia::test]
    async fn translates_event_phase_to_expected_evkey_events(
        fidl_phase: EventPhase,
        event_code: u32,
    ) -> Option<i32> {
        let event_code = event_code as u16;

        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_touch_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            make_touch_event_with_phase(fidl_phase),
        )
        .await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above. Use an empty `TouchEvent`, to minimize the chance that this event
        // creates unexpected `uapi::input_event`s.
        answer_next_watch_request(&mut touch_source_stream, TouchEvent::default()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(input_file, &file_object, &current_task);

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.

        // Report the `value` of the first `event.code` event, or None if there wasn't one.
        events.into_iter().find_map(|event| {
            if event.type_ == uapi::EV_KEY as u16 && event.code == event_code {
                Some(event.value)
            } else {
                None
            }
        })
    }

    // Per https://www.kernel.org/doc/Documentation/input/event-codes.rst,
    // 1. "BTN_TOUCH must be the first evdev code emitted".
    // 2. "EV_SYN, isused to separate input events"
    #[test_case((uapi::EV_KEY, uapi::BTN_TOUCH), 0; "btn_touch_is_first")]
    #[test_case((uapi::EV_SYN, uapi::SYN_REPORT), -1; "syn_report_is_last")]
    #[::fuchsia::test]
    async fn event_sequence_is_correct((event_type, event_code): (u32, u32), position: isize) {
        let event_type = event_type as u16;
        let event_code = event_code as u16;

        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_touch_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Reply to first `Watch` request.
        answer_next_watch_request(
            &mut touch_source_stream,
            make_touch_event_with_phase(EventPhase::Add),
        )
        .await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(input_file, &file_object, &current_task);

        // Check that the expected event type and code are in the expected position.
        let position = if position >= 0 {
            position.unsigned_abs()
        } else {
            events.iter().len() - position.unsigned_abs()
        };
        assert_matches!(
            events.get(position),
            Some(&uapi::input_event { type_, code, ..}) if type_ == event_type && code == event_code,
            "did not find type={}, code={} at position {}; `events` is {:?}",
            event_type,
            event_code,
            position,
            events
        );

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    #[test_case((0.0, 0.0); "origin")]
    #[test_case((100.7, 200.7); "above midpoint")]
    #[test_case((100.3, 200.3); "below midpoint")]
    #[test_case((100.5, 200.5); "midpoint")]
    #[::fuchsia::test]
    async fn sends_acceptable_coordinates((x, y): (f32, f32)) {
        // Set up resources.
        let (input_file, mut touch_source_stream, relay_thread) = start_touch_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event_with_coords(x, y))
            .await;

        // Wait for another `Watch`, to ensure `relay_thread` has consumed the `Watch` reply
        // from above.
        answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Consume all of the `uapi::input_event`s that are available.
        let events = read_uapi_events(input_file, &file_object, &current_task);

        // Check that the reported positions are within the acceptable error. The acceptable
        // error is chosen to allow either rounding or truncation.
        const ACCEPTABLE_ERROR: f32 = 1.0;
        let actual_x = events
            .iter()
            .find(|event| event.type_ == uapi::EV_ABS as u16 && event.code == uapi::ABS_X as u16)
            .unwrap_or_else(|| panic!("did not find `ABS_X` event in {:?}", events))
            .value;
        let actual_y = events
            .iter()
            .find(|event| event.type_ == uapi::EV_ABS as u16 && event.code == uapi::ABS_Y as u16)
            .unwrap_or_else(|| panic!("did not find `ABS_Y` event in {:?}", events))
            .value;
        assert_near!(x, actual_x as f32, ACCEPTABLE_ERROR);
        assert_near!(y, actual_y as f32, ACCEPTABLE_ERROR);

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
    }

    // Per the FIDL documentation for `TouchSource::Watch()`:
    //
    // > non-sample events should return an empty |TouchResponse| table to the
    // > server
    #[test_case(TouchEvent {
        timestamp: Some(0),
        pointer_sample: Some(make_touch_pointer_sample()),
        ..Default::default()
      } => matches Some(TouchResponse { response_type: Some(_), ..});
      "event_with_sample_yields_some_response_type")]
    #[test_case(TouchEvent::default() => matches Some(TouchResponse { response_type: None, ..});
      "event_without_sample_yields_no_response_type")]
    #[::fuchsia::test]
    async fn sends_appropriate_reply_to_touch_source_server(
        event: TouchEvent,
    ) -> Option<TouchResponse> {
        // Set up resources.
        let (_input_file, mut touch_source_stream, relay_thread) = start_touch_input();

        // Reply to first `Watch` request.
        answer_next_watch_request(&mut touch_source_stream, event).await;

        // Get response to `event`.
        let responses =
            answer_next_watch_request(&mut touch_source_stream, make_touch_event()).await;

        // Cleanly tear down the `TouchSource` client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.

        // Return the value for `test_case` to match on.
        responses.get(0).cloned()
    }

    #[::fuchsia::test]
    async fn sends_keyboard_events() {
        let (keyboard_file, mut keyboard_stream, relay_thread) = start_keyboard_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(keyboard_file.clone());

        let keyboard_listener = match keyboard_stream.next().await {
            Some(Ok(fuiinput::KeyboardRequest::AddListener {
                view_ref: _,
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let key_event = fuiinput::KeyEvent {
            timestamp: Some(0),
            type_: Some(fuiinput::KeyEventType::Pressed),
            key: Some(fidl_fuchsia_input::Key::Escape),
            ..Default::default()
        };

        let _ = keyboard_listener.on_key_event(&key_event).await;
        std::mem::drop(keyboard_stream); // Close Zircon channel.
        std::mem::drop(keyboard_listener); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
        let events = read_uapi_events(keyboard_file, &file_object, &current_task);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, uapi::KEY_POWER as u16);
    }

    #[::fuchsia::test]
    async fn skips_non_esc_keyboard_events() {
        let (keyboard_file, mut keyboard_stream, relay_thread) = start_keyboard_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(keyboard_file.clone());

        let keyboard_listener = match keyboard_stream.next().await {
            Some(Ok(fuiinput::KeyboardRequest::AddListener {
                view_ref: _,
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let key_event = fuiinput::KeyEvent {
            timestamp: Some(0),
            type_: Some(fuiinput::KeyEventType::Pressed),
            key: Some(fidl_fuchsia_input::Key::A),
            ..Default::default()
        };

        let _ = keyboard_listener.on_key_event(&key_event).await;
        std::mem::drop(keyboard_stream); // Close Zircon channel.
        std::mem::drop(keyboard_listener); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
        let events = read_uapi_events(keyboard_file, &file_object, &current_task);
        assert_eq!(events.len(), 0);
    }

    #[::fuchsia::test]
    async fn sends_button_events() {
        let (input_file, mut device_listener_stream, relay_thread) = start_button_input();
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        let buttons_listener = match device_listener_stream.next().await {
            Some(Ok(fuipolicy::DeviceListenerRegistryRequest::RegisterListener {
                listener,
                responder,
            })) => {
                let _ = responder.send();
                listener.into_proxy().expect("Failed to create proxy")
            }
            _ => {
                panic!("Failed to get event");
            }
        };

        let power_event = MediaButtonsEvent {
            volume: Some(0),
            mic_mute: Some(false),
            pause: Some(false),
            camera_disable: Some(false),
            power: Some(true),
            ..Default::default()
        };

        let _ = buttons_listener.on_event(&power_event).await;
        std::mem::drop(device_listener_stream); // Close Zircon channel.
        std::mem::drop(buttons_listener); // Close Zircon channel.
        relay_thread.join().expect("relay thread failed"); // Wait for relay thread to finish.
        let events = read_uapi_events(input_file, &file_object, &current_task);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, uapi::KEY_POWER as u16);
        assert_eq!(events[0].value, 1);
    }

    #[::fuchsia::test]
    fn touch_input_file_initialized_with_inspect_node() {
        let inspector = fuchsia_inspect::Inspector::default();
        let _touch_file =
            InputFile::new_touch(720, 1200, inspector.root().create_child("touch_input_file"));

        assert_data_tree!(inspector, root: {
            touch_input_file: {
                fidl_events_received_count: 0u64,
                fidl_events_converted_count: 0u64,
                uapi_events_generated_count: 0u64,
                uapi_events_read_count: 0u64,
                "fuchsia.inspect.Health": {
                    status: "STARTING_UP",
                    // Timestamp value is unpredictable and not relevant in this context,
                    // so we only assert that the property is present.
                    start_timestamp_nanos: AnyProperty
                },
            }
        });
    }

    #[::fuchsia::test]
    async fn touch_relay_updates_touch_inspect_status() {
        let inspector = fuchsia_inspect::Inspector::default();
        let input_file =
            InputFile::new_touch(720, 1200, inspector.root().create_child("touch_input_file"));
        let (touch_source_proxy, mut touch_source_stream) =
            fidl::endpoints::create_proxy_and_stream::<TouchSourceMarker>()
                .expect("failed to create TouchSource channel");
        let relay_thread = input_file.start_touch_relay(touch_source_proxy);
        let (_kernel, current_task, file_object) = make_kernel_objects(input_file.clone());

        // Send 2 TouchEvents to proxy that should be counted as `received` by InputFile
        // A TouchEvent::default() has no pointer sample so these events should be discarded.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responder, .. })) => responder
                .send(&vec![TouchEvent::default(); 2])
                .expect("failure sending Watch reply"),
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Send 5 TouchEvents with pointer sample to proxy, these should be received and converted
        // Add/Remove events generate 5 uapi events each. Change events generate 3 uapi events each.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, responder })) => {
                assert_matches!(responses.as_slice(), [_, _]);
                responder
                    .send(&vec![
                        make_touch_event_with_phase(EventPhase::Add),
                        make_touch_event(),
                        make_touch_event(),
                        make_touch_event(),
                        make_touch_event_with_phase(EventPhase::Remove),
                    ])
                    .expect("failure sending Watch reply");
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        // Wait for next `Watch` call and verify it has five elements in `responses`.
        match touch_source_stream.next().await {
            Some(Ok(TouchSourceRequest::Watch { responses, .. })) => {
                assert_matches!(responses.as_slice(), [_, _, _, _, _])
            }
            unexpected_request => panic!("unexpected request {:?}", unexpected_request),
        }

        let _events = read_uapi_events(input_file, &file_object, &current_task);

        assert_data_tree!(inspector, root: {
            touch_input_file: {
                fidl_events_received_count: 7u64,
                fidl_events_converted_count: 5u64,
                uapi_events_generated_count: 19u64,
                uapi_events_read_count: 19u64,
                "fuchsia.inspect.Health": {
                    status: "OK",
                    // Timestamp value is unpredictable and not relevant in this context,
                    // so we only assert that the property is present.
                    start_timestamp_nanos: AnyProperty
                },
            }
        });

        // Cleanly tear down the client.
        std::mem::drop(touch_source_stream); // Close Zircon channel.
        relay_thread.join().expect("client thread failed"); // Wait for client thread to finish.

        // Inspect should reflect InputFile is in UNHEALTHY state after touch stream connection is
        // closed and touch relay terminates.
        assert_data_tree!(inspector, root: {
            touch_input_file: {
                fidl_events_received_count: 7u64,
                fidl_events_converted_count: 5u64,
                uapi_events_generated_count: 19u64,
                uapi_events_read_count: 19u64,
                "fuchsia.inspect.Health": {
                    status: "UNHEALTHY",
                    message: "Touch relay terminated unexpectedly",
                    // Timestamp value is unpredictable and not relevant in this context,
                    // so we only assert that the property is present.
                    start_timestamp_nanos: AnyProperty
                },
            }
        });
    }
}
