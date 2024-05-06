// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt::Debug;

use net_types::ip::{Ipv4, Ipv6};
use netstack3_base::{InstantBindingsTypes, TimerBindingsTypes, TimerContext};
use packet_formats::ip::IpExt;

use crate::state::State;

/// Trait defining required types for filtering provided by bindings.
///
/// Allows rules that match on device class to be installed, storing the
/// [`FilterBindingsTypes::DeviceClass`] type at rest, while allowing Netstack3
/// Core to have Bindings provide the type since it is platform-specific.
pub trait FilterBindingsTypes: InstantBindingsTypes + TimerBindingsTypes {
    /// The device class type for devices installed in the netstack.
    type DeviceClass: Clone + Debug;
}

/// Trait aggregating functionality required from bindings.
pub trait FilterBindingsContext: TimerContext + FilterBindingsTypes {}
impl<BC: TimerContext + FilterBindingsTypes> FilterBindingsContext for BC {}

/// The IP version-specific execution context for packet filtering.
///
/// This trait exists to abstract over access to the filtering state. It is
/// useful to implement filtering logic in terms of this trait, as opposed to,
/// for example, [`crate::logic::FilterHandler`] methods taking the state
/// directly as an argument, because it allows Netstack3 Core to use lock
/// ordering types to enforce that filtering state is only acquired at or before
/// a given lock level, while keeping test code free of locking concerns.
pub trait FilterIpContext<I: IpExt, BT: FilterBindingsTypes> {
    /// Calls the function with a reference to filtering state.
    fn with_filter_state<O, F: FnOnce(&State<I, BT>) -> O>(&mut self, cb: F) -> O;
}

/// A context for mutably accessing all filtering state at once, to allow IPv4
/// and IPv6 filtering state to be modified atomically.
pub trait FilterContext<BT: FilterBindingsTypes> {
    /// Calls the function with a mutable reference to all filtering state.
    fn with_all_filter_state_mut<O, F: FnOnce(&mut State<Ipv4, BT>, &mut State<Ipv6, BT>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

#[cfg(feature = "testutils")]
impl<TimerId: Debug + PartialEq + Clone + Send + Sync, Event: Debug, State, FrameMeta>
    FilterBindingsTypes
    for netstack3_base::testutil::FakeBindingsCtx<TimerId, Event, State, FrameMeta>
{
    type DeviceClass = ();
}

#[cfg(test)]
pub(crate) mod testutil {
    use core::time::Duration;

    use net_types::ip::Ip;
    use netstack3_base::{
        testutil::{FakeInstant, FakeTimerCtx, WithFakeTimerContext},
        InstantContext, IntoCoreTimerCtx,
    };

    use super::*;
    use crate::{
        conntrack,
        logic::FilterTimerId,
        state::{validation::ValidRoutines, ConntrackExternalData, IpRoutines, Routines},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum FakeDeviceClass {
        Ethernet,
        Wlan,
    }

    pub struct FakeCtx<I: IpExt>(State<I, FakeBindingsCtx<I>>);

    impl<I: IpExt> FakeCtx<I> {
        pub fn with_ip_routines(
            bindings_ctx: &mut FakeBindingsCtx<I>,
            routines: IpRoutines<I, FakeDeviceClass, ()>,
        ) -> Self {
            let (installed_routines, uninstalled_routines) =
                ValidRoutines::new(Routines { ip: routines, ..Default::default() })
                    .expect("invalid state");
            Self(State {
                installed_routines,
                uninstalled_routines,
                conntrack: conntrack::Table::new::<IntoCoreTimerCtx>(bindings_ctx),
            })
        }

        pub fn conntrack(
            &mut self,
        ) -> &conntrack::Table<I, FakeBindingsCtx<I>, ConntrackExternalData> {
            &self.0.conntrack
        }
    }

    impl<I: IpExt> FilterIpContext<I, FakeBindingsCtx<I>> for FakeCtx<I> {
        fn with_filter_state<O, F: FnOnce(&State<I, FakeBindingsCtx<I>>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self(state) = self;
            cb(&*state)
        }
    }

    pub struct FakeBindingsCtx<I: Ip> {
        pub timer_ctx: FakeTimerCtx<FilterTimerId<I>>,
    }

    impl<I: Ip> FakeBindingsCtx<I> {
        pub(crate) fn new() -> Self {
            Self { timer_ctx: Default::default() }
        }

        pub(crate) fn sleep(&mut self, time_elapsed: Duration) {
            self.timer_ctx.instant.sleep(time_elapsed)
        }
    }

    impl<I: Ip> InstantBindingsTypes for FakeBindingsCtx<I> {
        type Instant = FakeInstant;
    }

    impl<I: Ip> FilterBindingsTypes for FakeBindingsCtx<I> {
        type DeviceClass = FakeDeviceClass;
    }

    impl<I: Ip> InstantContext for FakeBindingsCtx<I> {
        fn now(&self) -> Self::Instant {
            self.timer_ctx.now()
        }
    }

    impl<I: Ip> TimerBindingsTypes for FakeBindingsCtx<I> {
        type Timer = <FakeTimerCtx<FilterTimerId<I>> as TimerBindingsTypes>::Timer;

        type DispatchId = <FakeTimerCtx<FilterTimerId<I>> as TimerBindingsTypes>::DispatchId;
    }

    impl<I: Ip> TimerContext for FakeBindingsCtx<I> {
        fn new_timer(&mut self, id: Self::DispatchId) -> Self::Timer {
            self.timer_ctx.new_timer(id)
        }

        fn schedule_timer_instant(
            &mut self,
            time: Self::Instant,
            timer: &mut Self::Timer,
        ) -> Option<Self::Instant> {
            self.timer_ctx.schedule_timer_instant(time, timer)
        }

        fn cancel_timer(&mut self, timer: &mut Self::Timer) -> Option<Self::Instant> {
            self.timer_ctx.cancel_timer(timer)
        }

        fn scheduled_instant(&self, timer: &mut Self::Timer) -> Option<Self::Instant> {
            self.timer_ctx.scheduled_instant(timer)
        }
    }

    impl<I: Ip> WithFakeTimerContext<FilterTimerId<I>> for FakeBindingsCtx<I> {
        fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<FilterTimerId<I>>) -> O>(
            &self,
            f: F,
        ) -> O {
            let Self { timer_ctx } = self;
            f(timer_ctx)
        }

        fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<FilterTimerId<I>>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            let Self { timer_ctx } = self;
            f(timer_ctx)
        }
    }
}
