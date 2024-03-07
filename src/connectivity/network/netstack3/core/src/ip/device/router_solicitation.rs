// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IPv6 Router Solicitation as defined by [RFC 4861 section 6.3.7].
//!
//! [RFC 4861 section 6.3.7]: https://datatracker.ietf.org/doc/html/rfc4861#section-6.3.7

use core::{num::NonZeroU8, time::Duration};

use net_types::{ip::Ipv6Addr, UnicastAddr};
use packet::{EitherSerializer, EmptyBuf, InnerPacketBuilder as _, Serializer};
use packet_formats::icmp::ndp::{
    options::NdpOptionBuilder, OptionSequenceBuilder, RouterSolicitation,
};
use rand::Rng as _;

use crate::{
    context::{RngContext, TimerContext, TimerHandler},
    device::{AnyDevice, DeviceIdContext},
};

/// Amount of time to wait after sending `MAX_RTR_SOLICITATIONS` Router
/// Solicitation messages before determining that there are no routers on the
/// link for the purpose of IPv6 Stateless Address Autoconfiguration if no
/// Router Advertisement messages have been received as defined in [RFC 4861
/// section 10].
///
/// This parameter is also used when a host sends its initial Router
/// Solicitation message, as per [RFC 4861 section 6.3.7]. Before a node sends
/// an initial solicitation, it SHOULD delay the transmission for a random
/// amount of time between 0 and `MAX_RTR_SOLICITATION_DELAY`. This serves to
/// alleviate congestion when many hosts start up on a link at the same time.
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
/// [RFC 4861 section 6.3.7]: https://tools.ietf.org/html/rfc4861#section-6.3.7
pub(crate) const MAX_RTR_SOLICITATION_DELAY: Duration = Duration::from_secs(1);

/// Minimum duration between router solicitation messages as defined in [RFC
/// 4861 section 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
pub(crate) const RTR_SOLICITATION_INTERVAL: Duration = Duration::from_secs(4);

#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct RsTimerId<DeviceId> {
    pub(crate) device_id: DeviceId,
}

impl<DeviceId> RsTimerId<DeviceId> {
    pub(super) fn device_id(&self) -> &DeviceId {
        let Self { device_id } = self;
        device_id
    }
}

/// The execution context for router solicitation.
pub(super) trait RsContext<BC>: DeviceIdContext<AnyDevice> {
    /// A link-layer address.
    type LinkLayerAddr: AsRef<[u8]>;

    /// Calls the callback with a mutable reference to the remaining number of
    /// router soliciations to send and the maximum number of router solications
    /// to send.
    fn with_rs_remaining_mut_and_max<O, F: FnOnce(&mut Option<NonZeroU8>, Option<NonZeroU8>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the callback with a mutable reference to the remaining number of
    /// router soliciations to send.
    fn with_rs_remaining_mut<F: FnOnce(&mut Option<NonZeroU8>)>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) {
        self.with_rs_remaining_mut_and_max(device_id, |remaining, _max| cb(remaining))
    }

    /// Gets the device's link-layer address bytes, if the device supports
    /// link-layer addressing.
    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Self::LinkLayerAddr>;

    /// Sends an NDP Router Solicitation to the local-link.
    ///
    /// The callback is called with a source address suitable for an outgoing
    /// router solicitation message and returns the message body.
    fn send_rs_packet<
        S: Serializer<Buffer = EmptyBuf>,
        F: FnOnce(Option<UnicastAddr<Ipv6Addr>>) -> S,
    >(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        message: RouterSolicitation,
        body: F,
    ) -> Result<(), S>;
}

/// The bindings execution context for router solicitation.
pub(super) trait RsBindingsContext<DeviceId>:
    RngContext + TimerContext<RsTimerId<DeviceId>>
{
}
impl<DeviceId, BC: RngContext + TimerContext<RsTimerId<DeviceId>>> RsBindingsContext<DeviceId>
    for BC
{
}

/// An implementation of Router Solicitation.
pub trait RsHandler<BC>:
    DeviceIdContext<AnyDevice> + TimerHandler<BC, RsTimerId<Self::DeviceId>>
{
    /// Starts router solicitation.
    fn start_router_solicitation(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId);

    /// Stops router solicitation.
    ///
    /// Does nothing if router solicitaiton is not being performed
    fn stop_router_solicitation(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId);
}

impl<BC: RsBindingsContext<CC::DeviceId>, CC: RsContext<BC>> RsHandler<BC> for CC {
    fn start_router_solicitation(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId) {
        let remaining = self.with_rs_remaining_mut_and_max(device_id, |remaining, max| {
            *remaining = max;
            max
        });

        match remaining {
            None => {}
            Some(_) => {
                // As per RFC 4861 section 6.3.7, delay the first transmission for a
                // random amount of time between 0 and `MAX_RTR_SOLICITATION_DELAY` to
                // alleviate congestion when many hosts start up on a link at the same
                // time.
                let delay =
                    bindings_ctx.rng().gen_range(Duration::new(0, 0)..MAX_RTR_SOLICITATION_DELAY);
                assert_eq!(
                    bindings_ctx.schedule_timer(delay, RsTimerId { device_id: device_id.clone() },),
                    None
                );
            }
        }
    }

    fn stop_router_solicitation(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId) {
        let _: Option<BC::Instant> =
            bindings_ctx.cancel_timer(RsTimerId { device_id: device_id.clone() });
    }
}

impl<BC: RsBindingsContext<CC::DeviceId>, CC: RsContext<BC>>
    TimerHandler<BC, RsTimerId<CC::DeviceId>> for CC
{
    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        RsTimerId { device_id }: RsTimerId<CC::DeviceId>,
    ) {
        do_router_solicitation(self, bindings_ctx, &device_id)
    }
}

/// Solicit routers once and schedule next message.
fn do_router_solicitation<BC: RsBindingsContext<CC::DeviceId>, CC: RsContext<BC>>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
) {
    let src_ll = core_ctx.get_link_layer_addr_bytes(device_id);

    // TODO(https://fxbug.dev/42165912): Either panic or guarantee that this error
    // can't happen statically.
    let _: Result<(), _> =
        core_ctx.send_rs_packet(bindings_ctx, device_id, RouterSolicitation::default(), |src_ip| {
            // As per RFC 4861 section 4.1,
            //
            //   Valid Options:
            //
            //      Source link-layer address The link-layer address of the
            //                     sender, if known. MUST NOT be included if the
            //                     Source Address is the unspecified address.
            //                     Otherwise, it SHOULD be included on link
            //                     layers that have addresses.
            src_ip.map_or(EitherSerializer::A(EmptyBuf), |UnicastAddr { .. }| {
                EitherSerializer::B(
                    OptionSequenceBuilder::new(
                        src_ll
                            .as_ref()
                            .map(AsRef::as_ref)
                            .into_iter()
                            .map(NdpOptionBuilder::SourceLinkLayerAddress),
                    )
                    .into_serializer(),
                )
            })
        });

    core_ctx.with_rs_remaining_mut(device_id, |remaining| {
        *remaining = NonZeroU8::new(
            remaining
                .expect("should only send a router solicitations when at least one is remaining")
                .get()
                - 1,
        );

        match *remaining {
            None => {}
            Some(NonZeroU8 { .. }) => {
                assert_eq!(
                    bindings_ctx.schedule_timer(
                        RTR_SOLICITATION_INTERVAL,
                        RsTimerId { device_id: device_id.clone() },
                    ),
                    None
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use net_declare::net_ip_v6;
    use packet_formats::icmp::ndp::{options::NdpOption, Options};
    use test_case::test_case;

    use super::*;
    use crate::{
        context::{
            testutil::{FakeBindingsCtx, FakeCoreCtx, FakeCtx, FakeTimerCtxExt as _},
            InstantContext as _, SendFrameContext as _,
        },
        device::testutil::FakeDeviceId,
        ip::testutil::FakeIpDeviceIdCtx,
    };

    struct FakeRsContext {
        max_router_solicitations: Option<NonZeroU8>,
        router_soliciations_remaining: Option<NonZeroU8>,
        source_address: Option<UnicastAddr<Ipv6Addr>>,
        link_layer_bytes: Option<Vec<u8>>,
        ip_device_id_ctx: FakeIpDeviceIdCtx<FakeDeviceId>,
    }

    impl AsRef<FakeIpDeviceIdCtx<FakeDeviceId>> for FakeRsContext {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<FakeDeviceId> {
            &self.ip_device_id_ctx
        }
    }

    #[derive(Debug, PartialEq)]
    struct RsMessageMeta {
        message: RouterSolicitation,
    }

    type FakeCoreCtxImpl = FakeCoreCtx<FakeRsContext, RsMessageMeta, FakeDeviceId>;
    type FakeBindingsCtxImpl = FakeBindingsCtx<RsTimerId<FakeDeviceId>, (), (), ()>;

    impl RsContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type LinkLayerAddr = Vec<u8>;

        fn with_rs_remaining_mut_and_max<
            O,
            F: FnOnce(&mut Option<NonZeroU8>, Option<NonZeroU8>) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            let FakeRsContext {
                max_router_solicitations,
                router_soliciations_remaining,
                source_address: _,
                link_layer_bytes: _,
                ip_device_id_ctx: _,
            } = self.get_mut();
            cb(router_soliciations_remaining, *max_router_solicitations)
        }

        fn get_link_layer_addr_bytes(&mut self, &FakeDeviceId: &FakeDeviceId) -> Option<Vec<u8>> {
            let FakeRsContext {
                max_router_solicitations: _,
                router_soliciations_remaining: _,
                source_address: _,
                link_layer_bytes,
                ip_device_id_ctx: _,
            } = self.get_ref();
            link_layer_bytes.clone()
        }

        fn send_rs_packet<
            S: Serializer<Buffer = EmptyBuf>,
            F: FnOnce(Option<UnicastAddr<Ipv6Addr>>) -> S,
        >(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeDeviceId: &FakeDeviceId,
            message: RouterSolicitation,
            body: F,
        ) -> Result<(), S> {
            let FakeRsContext {
                max_router_solicitations: _,
                router_soliciations_remaining: _,
                source_address,
                link_layer_bytes: _,
                ip_device_id_ctx: _,
            } = self.get_ref();
            self.send_frame(bindings_ctx, RsMessageMeta { message }, body(*source_address))
        }
    }

    const RS_TIMER_ID: RsTimerId<FakeDeviceId> = RsTimerId { device_id: FakeDeviceId };

    #[test]
    fn stop_router_solicitation() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::with_state(FakeRsContext {
                max_router_solicitations: NonZeroU8::new(1),
                router_soliciations_remaining: None,
                source_address: None,
                link_layer_bytes: None,
                ip_device_id_ctx: Default::default(),
            }));
        RsHandler::start_router_solicitation(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId);

        let now = bindings_ctx.now();
        bindings_ctx
            .timer_ctx()
            .assert_timers_installed_range([(RS_TIMER_ID, now..=now + MAX_RTR_SOLICITATION_DELAY)]);

        RsHandler::stop_router_solicitation(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId);
        bindings_ctx.timer_ctx().assert_no_timers_installed();

        assert_eq!(core_ctx.frames(), &[][..]);
    }

    const SOURCE_ADDRESS: UnicastAddr<Ipv6Addr> =
        unsafe { UnicastAddr::new_unchecked(net_ip_v6!("fe80::1")) };

    #[test_case(0, None, None, None; "disabled")]
    #[test_case(1, None, None, None; "once_without_source_address_or_link_layer_option")]
    #[test_case(
        1,
        Some(SOURCE_ADDRESS),
        None,
        None; "once_with_source_address_and_without_link_layer_option")]
    #[test_case(
        1,
        None,
        Some(vec![1, 2, 3, 4, 5, 6]),
        None; "once_without_source_address_and_with_mac_address_source_link_layer_option")]
    #[test_case(
        1,
        Some(SOURCE_ADDRESS),
        Some(vec![1, 2, 3, 4, 5, 6]),
        Some(&[1, 2, 3, 4, 5, 6]); "once_with_source_address_and_mac_address_source_link_layer_option")]
    #[test_case(
        1,
        Some(SOURCE_ADDRESS),
        Some(vec![1, 2, 3, 4, 5]),
        Some(&[1, 2, 3, 4, 5, 0]); "once_with_source_address_and_short_address_source_link_layer_option")]
    #[test_case(
        1,
        Some(SOURCE_ADDRESS),
        Some(vec![1, 2, 3, 4, 5, 6, 7]),
        Some(&[
            1, 2, 3, 4, 5, 6, 7,
            0, 0, 0, 0, 0, 0, 0,
        ]); "once_with_source_address_and_long_address_source_link_layer_option")]
    fn perform_router_solicitation(
        max_router_solicitations: u8,
        source_address: Option<UnicastAddr<Ipv6Addr>>,
        link_layer_bytes: Option<Vec<u8>>,
        expected_sll_bytes: Option<&[u8]>,
    ) {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::with_state(FakeRsContext {
                max_router_solicitations: NonZeroU8::new(max_router_solicitations),
                router_soliciations_remaining: None,
                source_address,
                link_layer_bytes,
                ip_device_id_ctx: Default::default(),
            }));
        RsHandler::start_router_solicitation(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId);

        assert_eq!(core_ctx.frames(), &[][..]);

        let mut duration = MAX_RTR_SOLICITATION_DELAY;
        for i in 0..max_router_solicitations {
            assert_eq!(
                core_ctx.get_ref().router_soliciations_remaining,
                NonZeroU8::new(max_router_solicitations - i)
            );
            let now = bindings_ctx.now();
            bindings_ctx
                .timer_ctx()
                .assert_timers_installed_range([(RS_TIMER_ID, now..=now + duration)]);

            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(RS_TIMER_ID));
            let frames = core_ctx.frames();
            assert_eq!(frames.len(), usize::from(i + 1), "frames = {:?}", frames);
            let (RsMessageMeta { message }, frame) =
                frames.last().expect("should have transmitted a frame");
            assert_eq!(*message, RouterSolicitation::default());
            let options = Options::parse(&frame[..]).expect("parse NDP options");
            let sll_bytes = options.iter().find_map(|o| match o {
                NdpOption::SourceLinkLayerAddress(a) => Some(a),
                o => panic!("unexpected NDP option = {:?}", o),
            });

            assert_eq!(sll_bytes, expected_sll_bytes);
            duration = RTR_SOLICITATION_INTERVAL;
        }

        bindings_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(core_ctx.get_ref().router_soliciations_remaining, None);
        let frames = core_ctx.frames();
        assert_eq!(frames.len(), usize::from(max_router_solicitations), "frames = {:?}", frames);
    }
}
