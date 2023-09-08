// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod cobalt;
mod composite;
#[cfg(test)]
mod fake;
mod inspect;

pub use self::cobalt::CobaltDiagnostics;
pub use self::composite::CompositeDiagnostics;
#[cfg(test)]
pub use self::fake::FakeDiagnostics;
pub use self::inspect::{InspectDiagnostics, INSPECTOR};

use {
    crate::enums::{
        ClockCorrectionStrategy, ClockUpdateReason, FrequencyDiscardReason, InitialClockState,
        InitializeRtcOutcome, Role, SampleValidationError, StartClockSource, TimeSourceError,
        Track, WriteRtcOutcome,
    },
    fidl_fuchsia_time_external::Status,
    fuchsia_zircon as zx,
};

/// A special `Duration` that will match any value during an `eq_with_any` operation.
#[cfg(test)]
pub const ANY_DURATION: zx::Duration = zx::Duration::from_nanos(i64::MIN);

/// A special time that will match any value during an `eq_with_any` operation.
#[cfg(test)]
pub const ANY_TIME: zx::Time = zx::Time::from_nanos(i64::MIN);

/// An event that is potentially worth recording in one or more diagnostics systems.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Timekeeper has completed initialization.
    Initialized { clock_state: InitialClockState },
    /// An attempt was made to initialize and read from the real time clock.
    InitializeRtc { outcome: InitializeRtcOutcome, time: Option<zx::Time> },
    /// A time source failed, relaunch will be attempted.
    TimeSourceFailed { role: Role, error: TimeSourceError },
    /// A time source changed its state.
    TimeSourceStatus { role: Role, status: Status },
    /// A sample received from a time source was rejected during validation.
    SampleRejected { role: Role, error: SampleValidationError },
    /// The state of a Kalman filter was updated.
    KalmanFilterUpdated {
        /// The `Track` of the estimate.
        track: Track,
        /// The monotonic time at which the state applies.
        monotonic: zx::Time,
        /// The estimated UTC corresponding to monotonic.
        utc: zx::Time,
        /// Square root of element [0,0] of the covariance matrix.
        sqrt_covariance: zx::Duration,
    },
    /// A partially completed frequency window was discarded without being used.
    FrequencyWindowDiscarded { track: Track, reason: FrequencyDiscardReason },
    /// An estimated frequency was updated.
    FrequencyUpdated {
        /// The `Track` of the estimate.
        track: Track,
        /// The monotonic time at which the state applies.
        monotonic: zx::Time,
        /// The estimated frequency as a PPM deviation from nominal. A positive number means UTC is
        /// running faster than monotonic, i.e. the oscillator is slow.
        rate_adjust_ppm: i32,
        /// The number of frequency windows that contributed to this estimate.
        window_count: u32,
    },
    /// A strategy has been determined to align the userspace clock with the estimated UTC.
    /// This will be followed by zero or more `UpdateClock` events to implement the strategy.
    ClockCorrection { track: Track, correction: zx::Duration, strategy: ClockCorrectionStrategy },
    /// An attempt was made to write to the real time clock.
    WriteRtc { outcome: WriteRtcOutcome },
    /// The userspace clock has been started for the first time.
    StartClock { track: Track, source: StartClockSource },
    /// The userspace clock has been updated.
    UpdateClock { track: Track, reason: ClockUpdateReason },
}

/// A standard interface for systems that record events for diagnostic purposes.
pub trait Diagnostics: Send + Sync {
    /// Records the supplied event if relevant.
    fn record(&self, event: Event);
}
