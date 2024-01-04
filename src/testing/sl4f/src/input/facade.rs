// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::input::types::{
    self, ActionResult, KeyPressRequest, MultiFingerSwipeRequest, MultiFingerTapRequest,
    SwipeRequest, TapRequest, TextRequest,
};
use anyhow::{Context, Error};
use serde_json::{from_value, Value};
use std::{convert::TryFrom, time::Duration};
use tracing::info;

const DEFAULT_DIMENSION: u32 = 1000;
const DEFAULT_DURATION: Duration = Duration::from_millis(300);
const DEFAULT_KEY_EVENT_DURATION: Duration = Duration::from_millis(0);
const DEFAULT_KEY_PRESS_DURATION: Duration = Duration::from_millis(0);
const DEFAULT_TAP_EVENT_COUNT: usize = 1;

macro_rules! validate_fingers {
    ( $fingers:expr, $field:ident $comparator:tt $limit:expr ) => {
        match $fingers.iter().enumerate().find(|(_, finger)| !(finger.$field $comparator $limit)) {
            None => Ok(()),
            Some((finger_num, finger)) => Err(anyhow!(
                "finger {}: expected {} {} {}, but {} is not {} {}",
                finger_num,
                stringify!($field),
                stringify!($comparator),
                stringify!($limit),
                finger.$field,
                stringify!($comparator),
                $limit
            )),
        }
    };
}

/// Perform Input fidl operations.
///
/// Note this object is shared among all threads created by server.
///
#[derive(Debug)]
pub struct InputFacade {}

impl InputFacade {
    pub fn new() -> InputFacade {
        InputFacade {}
    }

    /// Tap at coordinates (x, y) for a touchscreen with default or custom
    /// width, height, duration, and tap event counts
    ///
    /// # Arguments
    /// * `value`: will be parsed to TapRequest
    ///   * must include:
    ///     * `x`: X axis coordinate
    ///     * `y`: Y axis coordinate
    ///   * optionally includes any of:
    ///     * `width`: Horizontal resolution of the touch panel, defaults to 1000
    ///     * `height`: Vertical resolution of the touch panel, defaults to 1000
    ///     * `tap_event_count`: Number of tap events to send (`duration` is divided over the tap
    ///                          events), defaults to 1
    ///     * `duration`: Duration of the event(s) in milliseconds, defaults to 300
    pub async fn tap(&self, args: Value) -> Result<ActionResult, Error> {
        info!("Executing Tap in Input Facade.");
        let req: TapRequest = from_value(args)?;
        let width = req.width.unwrap_or(DEFAULT_DIMENSION);
        let height = req.height.unwrap_or(DEFAULT_DIMENSION);
        let tap_event_count = req.tap_event_count.unwrap_or(DEFAULT_TAP_EVENT_COUNT);
        let duration = req.duration.map_or(DEFAULT_DURATION, Duration::from_millis);

        input_synthesis::tap_event_command(req.x, req.y, width, height, tap_event_count, duration)
            .await?;
        Ok(ActionResult::Success)
    }

    /// Multi-Finger Taps for a touchscreen with default or custom
    /// width, height, duration, and tap event counts.
    ///
    /// # Arguments
    /// * `value`: will be parsed by MultiFingerTapRequest
    ///   * must include:
    ///     * `fingers`: List of FIDL struct `Touch` defined at
    ///                  sdk/fidl/fuchsia.ui.input/input_reports.fidl.
    ///   * optionally includes any of:
    ///     * `width`: Horizontal resolution of the touch panel, defaults to 1000
    ///     * `height`: Vertical resolution of the touch panel, defaults to 1000
    ///     * `tap_event_count`: Number of multi-finger tap events to send
    ///                          (`duration` is divided over the events), defaults to 1
    ///     * `duration`: Duration of the event(s) in milliseconds, defaults to 0
    ///
    /// Example:
    /// To send a 2-finger triple tap over 3s.
    /// multi_finger_tap(MultiFingerTap {
    ///   tap_event_count: 3,
    ///   duration: 3000,
    ///   fingers: [
    ///     Touch { finger_id: 1, x: 0, y: 0, width: 0, height: 0 },
    ///     Touch { finger_id: 2, x: 20, y: 20, width: 0, height: 0 },
    ///  ]
    /// });
    ///
    pub async fn multi_finger_tap(&self, args: Value) -> Result<ActionResult, Error> {
        info!("Executing MultiFingerTap in Input Facade.");
        let req: MultiFingerTapRequest = from_value(args)?;
        let width = req.width.unwrap_or(DEFAULT_DIMENSION);
        let height = req.height.unwrap_or(DEFAULT_DIMENSION);
        let tap_event_count = req.tap_event_count.unwrap_or(DEFAULT_TAP_EVENT_COUNT);
        let duration = req.duration.map_or(DEFAULT_DURATION, Duration::from_millis);

        input_synthesis::multi_finger_tap_event_command(
            req.fingers,
            width,
            height,
            tap_event_count,
            duration,
        )
        .await?;
        Ok(ActionResult::Success)
    }

    /// Swipe from coordinates (x0, y0) to (x1, y1) for a touchscreen with default
    /// or custom width, height, duration, and tap event counts
    ///
    /// # Arguments
    /// * `value`: will be parsed to SwipeRequest
    ///   * must include:
    ///     * `x0`: X axis start coordinate
    ///     * `y0`: Y axis start coordinate
    ///     * `x1`: X axis end coordinate
    ///     * `y1`: Y axis end coordinate
    ///   * optionally includes any of:
    ///     * `width`: Horizontal resolution of the touch panel, defaults to 1000
    ///     * `height`: Vertical resolution of the touch panel, defaults to 1000
    ///     * `tap_event_count`: Number of move events to send in between the down and up events of
    ///                          the swipe, defaults to `duration / 17` (to emulate a 60 HZ sensor)
    ///     * `duration`: Duration of the event(s) in milliseconds, default to 300
    pub async fn swipe(&self, args: Value) -> Result<ActionResult, Error> {
        info!("Executing Swipe in Input Facade.");
        let req: SwipeRequest = from_value(args)?;
        let width = req.width.unwrap_or(DEFAULT_DIMENSION);
        let height = req.height.unwrap_or(DEFAULT_DIMENSION);
        let duration = req.duration.map_or(DEFAULT_DURATION, Duration::from_millis);
        let tap_event_count = req.tap_event_count.unwrap_or_else(|| {
            // 17 msec per move event, to emulate a ~60Hz sensor.
            duration.as_millis() as usize / 17
        });

        input_synthesis::swipe_command(
            req.x0,
            req.y0,
            req.x1,
            req.y1,
            height,
            width,
            tap_event_count,
            duration,
        )
        .await?;
        Ok(ActionResult::Success)
    }

    /// Swipes multiple fingers from start positions to end positions for a touchscreen.
    ///
    /// # Arguments
    /// * `value`: will be parsed to `MultiFingerSwipeRequest`
    ///   * must include:
    ///     * `fingers`: List of `FingerSwipe`s.
    ///       * All `x0` and `x1` values must be in the range (0, width), regardless of
    ///         whether the width is defaulted or explicitly specified.
    ///       * All `y0` and `y1` values must be in the range (0, height), regardless of
    ///         whether the height is defaulted or explicitly specified.
    ///   * optionally includes any of:
    ///     * `width`: Horizontal resolution of the touch panel, defaults to 1000
    ///     * `height`: Vertical resolution of the touch panel, defaults to 1000
    ///     * `move_event_count`: Number of move events to send in between the down and up events of
    ///        the swipe.
    ///        * Defaults to `duration / 17` (to emulate a 60 HZ sensor).
    ///        * If 0, only the down and up events will be sent.
    ///     * `duration`: Duration of the event(s) in milliseconds
    ///        * Defaults to 300 milliseconds.
    ///        * Must be large enough to allow for at least one nanosecond per move event.
    ///
    /// # Returns
    /// * `Ok(ActionResult::Success)` if the arguments were successfully parsed and events
    ///    successfully injected.
    /// * `Err(Error)` otherwise.
    ///
    /// # Example
    /// To send a two-finger swipe, with four events over two seconds:
    ///
    /// ```
    /// multi_finger_swipe(MultiFingerSwipeRequest {
    ///   fingers: [
    ///     FingerSwipe { x0: 0, y0:   0, x1: 100, y1:   0 },
    ///     FingerSwipe { x0: 0, y0: 100, x1: 100, y1: 100 },
    ///   ],
    ///   move_event_count: 4
    ///   duration: 2000,
    /// });
    /// ```
    pub async fn multi_finger_swipe(&self, args: Value) -> Result<ActionResult, Error> {
        info!("Executing MultiFingerSwipe in Input Facade.");
        let req: MultiFingerSwipeRequest = from_value(args)?;
        let width = req.width.unwrap_or(DEFAULT_DIMENSION);
        let height = req.height.unwrap_or(DEFAULT_DIMENSION);
        let duration = req.duration.map_or(DEFAULT_DURATION, Duration::from_millis);
        let move_event_count = req.move_event_count.unwrap_or_else(|| {
            // 17 msec per move event, to emulate a ~60Hz sensor.
            duration.as_millis() as usize / 17
        });
        ensure!(
            duration.as_nanos()
                >= u128::try_from(move_event_count)
                    .context("internal error while validating `duration`")?,
            "`duration` of {} nsec is too short for `move_event_count` of {}; \
            all events would have same timestamp",
            duration.as_nanos(),
            move_event_count
        );
        validate_fingers!(req.fingers, x0 <= width)?;
        validate_fingers!(req.fingers, x1 <= width)?;
        validate_fingers!(req.fingers, y0 <= height)?;
        validate_fingers!(req.fingers, y1 <= height)?;

        let start_fingers =
            req.fingers.iter().map(|finger| (finger.x0, finger.y0)).collect::<Vec<_>>();
        let end_fingers =
            req.fingers.iter().map(|finger| (finger.x1, finger.y1)).collect::<Vec<_>>();

        input_synthesis::multi_finger_swipe_command(
            start_fingers,
            end_fingers,
            width,
            height,
            move_event_count,
            duration,
        )
        .await?;
        Ok(ActionResult::Success)
    }

    /// Enters `text`, as if typed on a keyboard, with `key_event_duration` between key events.
    ///
    /// # Arguments
    /// * `value`: will be parsed to `TextRequest`
    ///   * must include:
    ///     * `text`: the characters to be input.
    ///        * Must be non-empty.
    ///        * All characters within `text` must be representable using the current
    ///          keyboard layout and locale. (At present, it is assumed that the current
    ///          layout and locale are `US-QWERTY` and `en-US`, respectively.)
    ///        * If these constraints are violated, returns an `Err`.
    ///   * optionally includes:
    ///     * `key_event_duration`: Duration of each event in milliseconds
    ///        * Serves as a lower bound on the time between key events (actual time may be
    ///          higher due to system load).
    ///        * Defaults to 0 milliseconds (each event is sent as quickly as possible).
    ///        * The number of events is `>= 2 * text.len()`:
    ///          * To account for both key-down and key-up events for every character.
    ///          * To account for modifier keys (e.g. capital letters require pressing the
    ///            shift key).
    ///
    /// # Returns
    /// * `Ok(ActionResult::Success)` if the arguments were successfully parsed and events
    ///    successfully injected.
    /// * `Err(Error)` otherwise.
    ///
    /// # Example
    /// To send "hello world", with 1 millisecond between each key event:
    ///
    /// ```
    /// text(TextRequest {
    ///   text: "hello world",
    ///   key_event_duration: 1,
    /// });
    /// ```
    pub async fn text(&self, args: Value) -> Result<ActionResult, Error> {
        info!("Executing Text in Input Facade.");
        let req: TextRequest = from_value(args)?;
        let text = match req.text.len() {
            0 => Err(format_err!("`text` must be non-empty")),
            _ => Ok(req.text),
        }?;
        let key_event_duration =
            req.key_event_duration.map_or(DEFAULT_KEY_EVENT_DURATION, Duration::from_millis);

        input_synthesis::text_command(text, key_event_duration).await?;
        Ok(ActionResult::Success)
    }

    /// Simulates a single key down + up sequence, for the given `hid_usage_id`.
    ///
    /// # Arguments
    /// * `value`: will be parsed to `KeyPressRequest`
    ///   * must include
    ///     * `hid_usage_id`: desired HID Usage ID, per [HID Usages and Descriptions].
    ///       * The Usage ID will be interpreted in the context of "Usage Page" 0x07, which
    ///         is the "Keyboard/Keypad" page.
    ///       * Because Usage IDs are defined by an external standard, it is impractical
    ///         to validate this parameter. As such, any value can be injected successfully.
    ///         However, the interpretation of unrecognized values is subject to the choices
    ///         of the system under test.
    ///   * optionally includes:
    ///     * `key_press_duration`: time between the down event and the up event, in milliseconds
    ///        * Serves as a lower bound on the time between the down event and the up event
    ///          (actual time may be higher due to system load).
    ///        * Defaults to 0 milliseconds (the up event is sent immediately after the down event)
    ///
    /// # Returns
    /// * `Ok(ActionResult::Success)` if the arguments were successfully parsed and events
    ///    successfully injected.
    /// * `Err(Error)` otherwise.
    ///
    /// # Future directions
    /// Per https://fxbug.dev/63532, this method will be replaced with a method that deals in
    /// `fuchsia.input.Key`s, instead of HID Usage IDs.
    ///
    /// # Example
    /// To simulate a press of the `ENTER` key, with 1 millisecond between the down and
    /// up events:
    ///
    /// ```
    /// key_press(KeyPressRequest {
    ///   hid_usage_id: 40,
    ///   key_press_duration: 1,
    /// });
    /// ```
    ///
    /// [HID Usages and Descriptions]: https://www.usb.org/sites/default/files/documents/hut1_12v2.pdf
    pub async fn key_press(&self, args: Value) -> Result<ActionResult, Error> {
        info!("Executing KeyboardEvent in Input Facade.");
        let req: KeyPressRequest = from_value(args)?;
        let hid_usage_id = req.hid_usage_id;
        let key_press_duration =
            req.key_press_duration.map_or(DEFAULT_KEY_PRESS_DURATION, Duration::from_millis);

        input_synthesis::keyboard_event_command(hid_usage_id.into(), key_press_duration).await?;
        Ok(ActionResult::Success)
    }

    /// Simulates a sequence of key events (presses and releases) on a keyboard.
    ///
    /// Dispatches the supplied `events` into a keyboard device, honoring the timing sequence that is
    /// requested in them, to the extent possible using the current scheduling system.
    ///
    /// Since each individual key press is broken down into constituent pieces (presses, releases,
    /// pauses), it is possible to dispatch a key event sequence corresponding to multiple keys being
    /// pressed and released in an arbitrary sequence.  This sequence is usually understood as a timing
    /// diagram like this:
    ///
    /// ```ignore
    ///           v--- key press   v--- key release
    /// A: _______/^^^^^^^^^^^^^^^^\__________
    ///    |<----->|   <-- duration from start for key press.
    ///    |<--------------------->|   <-- duration from start for key release.
    ///
    /// B: ____________/^^^^^^^^^^^^^^^^\_____
    ///                ^--- key press   ^--- key release
    ///    |<--------->|   <-- duration from start for key press.
    ///    |<-------------------------->|   <-- duration for key release.
    /// ```
    ///
    /// You would from there convert the desired timing diagram into a sequence of [KeyEvent]s
    /// that you would pass into this function. Note that all durations are specified as durations
    /// from the start of the key event sequence.
    ///
    /// Note that due to the way event timing works, it is in practice impossible to have two key
    /// events happen at exactly the same time even if you so specify.  Do not rely on simultaneous
    /// asynchronous event processing: it does not work in this code, and it is not how reality works
    /// either.  Instead, design your key event processing so that it is robust against the inherent
    /// non-determinism in key event delivery.
    ///
    /// # Arguments
    ///
    /// The `args` must be a JSON value that can be parsed as [types::KeyEventsRequest].
    ///
    /// `types::KeyEventsRequest`, in turn, has a sequence of key events that need to be injected.
    /// Each key event is a triple of:
    ///
    /// * The Fuchsia encoding of the USB HID usage code (see [fuchsia_ui_input::Key]).
    /// * The duration in milliseconds since the start of the key event sequence when this
    ///   action must happen.
    /// * The type of the key event (see [fuchsia_ui_input3::KeyEventType]), encoded as a
    ///   numeric value.
    ///
    /// # Example:
    ///
    /// The above diagram would be encoded as the following sequence of events (in pseudocode):
    ///
    /// ```ignore
    /// [
    ///   { "A", 10, Pressed  },
    ///   { "B", 10, Pressed  },
    ///   { "A", 50, Released },
    ///   { "B", 60, Released },
    /// ]
    /// ```
    ///
    /// # Returns
    /// * `Ok(ActionResult::Success)` if the arguments were successfully parsed and events
    ///    successfully injected.
    /// * `Err(Error)` otherwise.
    ///
    pub async fn key_events(&self, args: Value) -> Result<ActionResult, Error> {
        info!(?args, "Executing KeyEvents in Input Facade");
        let req: types::KeyEventsRequest = from_value(args)?;
        input_synthesis::dispatch_key_events(&req.key_events[..]).await?;
        Ok(ActionResult::Success)
    }
}
