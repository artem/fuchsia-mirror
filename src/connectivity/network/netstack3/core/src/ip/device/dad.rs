// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Duplicate Address Detection.

use core::num::NonZeroU16;

use net_types::{
    ip::{Ipv4, Ipv6, Ipv6Addr},
    MulticastAddr, UnicastAddr, Witness as _,
};
use packet_formats::{icmp::ndp::NeighborSolicitation, utils::NonZeroDuration};
use tracing::debug;

use crate::{
    context::{
        CoreEventContext, CoreTimerContext, EventContext, HandleableTimer, TimerBindingsTypes,
        TimerContext,
    },
    device::{self, AnyDevice, DeviceIdContext, StrongId as _, WeakId as _},
    ip::device::{
        state::Ipv6DadState, IpAddressId as _, IpAddressState, IpDeviceAddressIdContext,
        IpDeviceIpExt, WeakIpAddressId,
    },
};

/// A timer ID for duplicate address detection.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct DadTimerId<D: device::WeakId, A: WeakIpAddressId<Ipv6Addr>> {
    pub(crate) device_id: D,
    pub(crate) addr: A,
}

impl<D: device::WeakId, A: WeakIpAddressId<Ipv6Addr>> DadTimerId<D, A> {
    pub(super) fn device_id(&self) -> &D {
        let Self { device_id, addr: _ } = self;
        device_id
    }
}

pub(super) struct DadAddressStateRef<'a, CC, BT: DadBindingsTypes> {
    /// A mutable reference to an address' state.
    ///
    /// `None` if the address is not recognized.
    pub(super) dad_state: &'a mut Ipv6DadState<BT>,
    /// The execution context available with the address's DAD state.
    pub(super) core_ctx: &'a mut CC,
}

/// Holds references to state associated with duplicate address detection.
pub(super) struct DadStateRef<'a, CC, BT: DadBindingsTypes> {
    pub(super) state: DadAddressStateRef<'a, CC, BT>,
    /// The time between DAD message retransmissions.
    pub(super) retrans_timer: &'a NonZeroDuration,
    /// The maximum number of DAD messages to send.
    pub(super) max_dad_transmits: &'a Option<NonZeroU16>,
}

/// The execution context while performing DAD.
pub(super) trait DadAddressContext<BC>: IpDeviceAddressIdContext<Ipv6> {
    /// Calls the function with a mutable reference to the address's assigned
    /// flag.
    fn with_address_assigned<O, F: FnOnce(&mut bool) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
        cb: F,
    ) -> O;

    /// Joins the multicast group on the device.
    fn join_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    );

    /// Leaves the multicast group on the device.
    fn leave_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    );
}

/// The execution context for DAD.
pub(super) trait DadContext<BC: DadBindingsTypes>:
    IpDeviceAddressIdContext<Ipv6>
    + DeviceIdContext<AnyDevice>
    + CoreTimerContext<DadTimerId<Self::WeakDeviceId, Self::WeakAddressId>, BC>
    + CoreEventContext<DadEvent<Self::DeviceId>>
{
    type DadAddressCtx<'a>: DadAddressContext<
        BC,
        DeviceId = Self::DeviceId,
        AddressId = Self::AddressId,
    >;

    /// Calls the function with the DAD state associated with the address.
    fn with_dad_state<O, F: FnOnce(DadStateRef<'_, Self::DadAddressCtx<'_>, BC>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
        cb: F,
    ) -> O;

    /// Sends an NDP Neighbor Solicitation message for DAD to the local-link.
    ///
    /// The message will be sent with the unspecified (all-zeroes) source
    /// address.
    fn send_dad_packet(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        dst_ip: MulticastAddr<Ipv6Addr>,
        message: NeighborSolicitation,
    ) -> Result<(), ()>;
}

#[derive(Debug, Eq, Hash, PartialEq)]
/// Events generated by duplicate address detection.
pub enum DadEvent<DeviceId> {
    /// Duplicate address detection completed and the address is assigned.
    AddressAssigned {
        /// Device the address belongs to.
        device: DeviceId,
        /// The address that moved to the assigned state.
        addr: UnicastAddr<Ipv6Addr>,
    },
}

/// The bindings types for DAD.
pub trait DadBindingsTypes: TimerBindingsTypes {}
impl<BT> DadBindingsTypes for BT where BT: TimerBindingsTypes {}

/// The bindings execution context for DAD.
///
/// The type parameter `E` is tied by [`DadContext`] so that [`DadEvent`] can be
/// transformed into an event that is more meaningful to bindings.
pub trait DadBindingsContext<E>: DadBindingsTypes + TimerContext + EventContext<E> {}
impl<E, BC> DadBindingsContext<E> for BC where BC: DadBindingsTypes + TimerContext + EventContext<E> {}

/// An implementation for Duplicate Address Detection.
pub trait DadHandler<I: IpDeviceIpExt, BC>:
    DeviceIdContext<AnyDevice> + IpDeviceAddressIdContext<I>
{
    // TODO(https://fxbug.dev/42077260): This can probably be removed when we
    // can do DAD over IPv4.
    const INITIAL_ADDRESS_STATE: IpAddressState;

    /// Starts duplicate address detection.
    ///
    /// # Panics
    ///
    /// Panics if tentative state for the address is not found.
    fn start_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    );

    /// Stops duplicate address detection.
    ///
    /// Does nothing if DAD is not being performed on the address.
    fn stop_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    );
}

enum DoDadVariation {
    Start,
    Continue,
}

fn do_duplicate_address_detection<BC: DadBindingsContext<CC::OuterEvent>, CC: DadContext<BC>>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: &CC::AddressId,
    variation: DoDadVariation,
) {
    let send_msg = core_ctx.with_dad_state(
        device_id,
        addr,
        |DadStateRef { state, retrans_timer, max_dad_transmits }| {
            let DadAddressStateRef { dad_state, core_ctx } = state;

            match variation {
                DoDadVariation::Start => {
                    // Mark the address as tentative/unassigned before joining
                    // the group so that the address is not used as the source
                    // for any outgoing MLD message.
                    *dad_state = Ipv6DadState::Tentative {
                        dad_transmits_remaining: *max_dad_transmits,
                        timer: CC::new_timer(
                            bindings_ctx,
                            DadTimerId { device_id: device_id.downgrade(), addr: addr.downgrade() },
                        ),
                    };
                    core_ctx.with_address_assigned(device_id, addr, |assigned| *assigned = false);

                    // As per RFC 4861 section 5.6.2,
                    //
                    //   Before sending a Neighbor Solicitation, an interface MUST join
                    //   the all-nodes multicast address and the solicited-node
                    //   multicast address of the tentative address.
                    //
                    // Note that we join the all-nodes multicast address on interface
                    // enable.
                    core_ctx.join_multicast_group(
                        bindings_ctx,
                        device_id,
                        addr.addr().addr().to_solicited_node_address(),
                    );
                }
                DoDadVariation::Continue => {}
            }

            let (remaining, timer) = match dad_state {
                Ipv6DadState::Tentative { dad_transmits_remaining, timer } => {
                    (dad_transmits_remaining, timer)
                }
                Ipv6DadState::Uninitialized | Ipv6DadState::Assigned => {
                    panic!("expected address to be tentative; addr={addr:?}")
                }
            };

            match remaining {
                None => {
                    *dad_state = Ipv6DadState::Assigned;
                    core_ctx.with_address_assigned(device_id, addr, |assigned| *assigned = true);
                    CC::on_event(
                        bindings_ctx,
                        DadEvent::AddressAssigned {
                            device: device_id.clone(),
                            addr: addr.addr_sub().addr().get(),
                        },
                    );
                    false
                }
                Some(non_zero_remaining) => {
                    *remaining = NonZeroU16::new(non_zero_remaining.get() - 1);

                    // Per RFC 4862 section 5.1,
                    //
                    //   DupAddrDetectTransmits ...
                    //      Autoconfiguration also assumes the presence of the variable
                    //      RetransTimer as defined in [RFC4861]. For autoconfiguration
                    //      purposes, RetransTimer specifies the delay between
                    //      consecutive Neighbor Solicitation transmissions performed
                    //      during Duplicate Address Detection (if
                    //      DupAddrDetectTransmits is greater than 1), as well as the
                    //      time a node waits after sending the last Neighbor
                    //      Solicitation before ending the Duplicate Address Detection
                    //      process.
                    assert_eq!(
                        bindings_ctx.schedule_timer(retrans_timer.get(), timer),
                        None,
                        "Unexpected DAD timer; addr={}, device_id={:?}",
                        addr.addr(),
                        device_id
                    );
                    debug!(
                        "performing DAD for {}; {} tries left",
                        addr.addr(),
                        remaining.map_or(0, NonZeroU16::get)
                    );
                    true
                }
            }
        },
    );

    if !send_msg {
        return;
    }

    // Do not include the source link-layer option when the NS
    // message as DAD messages are sent with the unspecified source
    // address which must not hold a source link-layer option.
    //
    // As per RFC 4861 section 4.3,
    //
    //   Possible options:
    //
    //      Source link-layer address
    //           The link-layer address for the sender. MUST NOT be
    //           included when the source IP address is the
    //           unspecified address. Otherwise, on link layers
    //           that have addresses this option MUST be included in
    //           multicast solicitations and SHOULD be included in
    //           unicast solicitations.
    //
    // TODO(https://fxbug.dev/42165912): Either panic or guarantee that this error
    // can't happen statically.
    let dst_ip = addr.addr().addr().to_solicited_node_address();
    let _: Result<(), _> = core_ctx.send_dad_packet(
        bindings_ctx,
        device_id,
        dst_ip,
        NeighborSolicitation::new(addr.addr().addr()),
    );
}

// TODO(https://fxbug.dev/42077260): Actually support DAD for IPv4.
impl<BC, CC> DadHandler<Ipv4, BC> for CC
where
    CC: IpDeviceAddressIdContext<Ipv4> + DeviceIdContext<AnyDevice>,
{
    const INITIAL_ADDRESS_STATE: IpAddressState = IpAddressState::Assigned;
    fn start_duplicate_address_detection(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _addr: &Self::AddressId,
    ) {
    }

    fn stop_duplicate_address_detection(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _addr: &Self::AddressId,
    ) {
    }
}

impl<BC: DadBindingsContext<CC::OuterEvent>, CC: DadContext<BC>> DadHandler<Ipv6, BC> for CC {
    const INITIAL_ADDRESS_STATE: IpAddressState = IpAddressState::Tentative;

    fn start_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    ) {
        do_duplicate_address_detection(self, bindings_ctx, device_id, addr, DoDadVariation::Start)
    }

    fn stop_duplicate_address_detection(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: &Self::AddressId,
    ) {
        self.with_dad_state(
            device_id,
            addr,
            |DadStateRef { state, retrans_timer: _, max_dad_transmits: _ }| {
                let DadAddressStateRef { dad_state, core_ctx } = state;

                let leave_group = match dad_state {
                    Ipv6DadState::Assigned => true,
                    Ipv6DadState::Tentative { dad_transmits_remaining: _, timer } => {
                        assert_ne!(bindings_ctx.cancel_timer(timer), None);

                        true
                    }
                    Ipv6DadState::Uninitialized => false,
                };

                // Undo the work we did when starting/performing DAD by putting
                // the address back into a tentative/unassigned state and
                // leaving the solicited node multicast group. We mark the
                // address as tentative/unassigned before leaving the group so
                // that the address is not used as the source for any outgoing
                // MLD message.
                *dad_state = Ipv6DadState::Uninitialized;
                core_ctx.with_address_assigned(device_id, addr, |assigned| *assigned = false);
                if leave_group {
                    core_ctx.leave_multicast_group(
                        bindings_ctx,
                        device_id,
                        addr.addr().addr().to_solicited_node_address(),
                    );
                }
            },
        )
    }
}

impl<BC: DadBindingsContext<CC::OuterEvent>, CC: DadContext<BC>> HandleableTimer<CC, BC>
    for DadTimerId<CC::WeakDeviceId, CC::WeakAddressId>
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC) {
        let Self { device_id, addr } = self;
        let Some(device_id) = device_id.upgrade() else {
            return;
        };
        let Some(addr_id) = addr.upgrade() else {
            return;
        };
        do_duplicate_address_detection(
            core_ctx,
            bindings_ctx,
            &device_id,
            &addr_id,
            DoDadVariation::Continue,
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::hash_map::{Entry, HashMap};
    use core::time::Duration;

    use assert_matches::assert_matches;
    use net_types::{
        ip::{AddrSubnet, IpAddress as _},
        Witness as _,
    };
    use packet::EmptyBuf;
    use packet_formats::icmp::ndp::Options;

    use super::*;
    use crate::{
        context::{
            testutil::{FakeBindingsCtx, FakeCoreCtx, FakeTimerCtxExt as _},
            CtxPair, InstantContext as _, SendFrameContext as _, TimerHandler,
        },
        device::testutil::{FakeDeviceId, FakeWeakDeviceId},
        ip::{
            device::{testutil::FakeWeakAddressId, Ipv6DeviceAddr},
            testutil::FakeIpDeviceIdCtx,
        },
    };

    struct FakeDadAddressContext {
        addr: UnicastAddr<Ipv6Addr>,
        assigned: bool,
        groups: HashMap<MulticastAddr<Ipv6Addr>, usize>,
        ip_device_id_ctx: FakeIpDeviceIdCtx<FakeDeviceId>,
    }

    type FakeAddressCtxImpl = FakeCoreCtx<FakeDadAddressContext, (), FakeDeviceId>;

    impl AsRef<FakeIpDeviceIdCtx<FakeDeviceId>> for FakeDadAddressContext {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<FakeDeviceId> {
            &self.ip_device_id_ctx
        }
    }

    impl IpDeviceAddressIdContext<Ipv6> for FakeAddressCtxImpl {
        type AddressId = AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>;
        type WeakAddressId = FakeWeakAddressId<AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>>;
    }

    impl DadAddressContext<FakeBindingsCtxImpl> for FakeAddressCtxImpl {
        fn with_address_assigned<O, F: FnOnce(&mut bool) -> O>(
            &mut self,
            &FakeDeviceId: &Self::DeviceId,
            request_addr: &Self::AddressId,
            cb: F,
        ) -> O {
            let FakeDadAddressContext { addr, assigned, .. } = &mut self.state;
            assert_eq!(*request_addr.addr(), *addr);
            cb(assigned)
        }

        fn join_multicast_group(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeDeviceId: &Self::DeviceId,
            multicast_addr: MulticastAddr<Ipv6Addr>,
        ) {
            *self.state.groups.entry(multicast_addr).or_default() += 1;
        }

        fn leave_multicast_group(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeDeviceId: &Self::DeviceId,
            multicast_addr: MulticastAddr<Ipv6Addr>,
        ) {
            match self.state.groups.entry(multicast_addr) {
                Entry::Vacant(_) => {}
                Entry::Occupied(mut e) => {
                    let v = e.get_mut();
                    const COUNT_BEFORE_REMOVE: usize = 1;
                    if *v == COUNT_BEFORE_REMOVE {
                        assert_eq!(e.remove(), COUNT_BEFORE_REMOVE);
                    } else {
                        *v -= 1
                    }
                }
            }
        }
    }

    struct FakeDadContext {
        state: Ipv6DadState<FakeBindingsCtxImpl>,
        retrans_timer: NonZeroDuration,
        max_dad_transmits: Option<NonZeroU16>,
        address_ctx: FakeAddressCtxImpl,
    }

    #[derive(Debug)]
    struct DadMessageMeta {
        dst_ip: MulticastAddr<Ipv6Addr>,
        message: NeighborSolicitation,
    }

    impl AsRef<FakeIpDeviceIdCtx<FakeDeviceId>> for FakeDadContext {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<FakeDeviceId> {
            &self.address_ctx.state.ip_device_id_ctx
        }
    }

    type TestDadTimerId = DadTimerId<
        FakeWeakDeviceId<FakeDeviceId>,
        FakeWeakAddressId<AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>>,
    >;

    type FakeBindingsCtxImpl = FakeBindingsCtx<TestDadTimerId, DadEvent<FakeDeviceId>, (), ()>;

    type FakeCoreCtxImpl = FakeCoreCtx<FakeDadContext, DadMessageMeta, FakeDeviceId>;

    fn get_address_id(addr: Ipv6Addr) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        AddrSubnet::new(addr, Ipv6Addr::BYTES * 8).unwrap()
    }

    impl IpDeviceAddressIdContext<Ipv6> for FakeCoreCtxImpl {
        type AddressId = AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>;
        type WeakAddressId = FakeWeakAddressId<AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>>;
    }

    impl CoreTimerContext<TestDadTimerId, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn convert_timer(dispatch_id: TestDadTimerId) -> TestDadTimerId {
            dispatch_id
        }
    }

    impl CoreEventContext<DadEvent<FakeDeviceId>> for FakeCoreCtxImpl {
        type OuterEvent = DadEvent<FakeDeviceId>;
        fn convert_event(event: DadEvent<FakeDeviceId>) -> DadEvent<FakeDeviceId> {
            event
        }
    }

    impl DadContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type DadAddressCtx<'a> = FakeAddressCtxImpl;

        fn with_dad_state<
            O,
            F: FnOnce(DadStateRef<'_, Self::DadAddressCtx<'_>, FakeBindingsCtxImpl>) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            request_addr: &Self::AddressId,
            cb: F,
        ) -> O {
            let FakeDadContext { state, retrans_timer, max_dad_transmits, address_ctx } =
                &mut self.state;
            let ctx_addr = address_ctx.state.addr;
            let requested_addr = request_addr.addr().get();
            assert!(
                ctx_addr == requested_addr,
                "invalid address {requested_addr} expected {ctx_addr}"
            );
            cb(DadStateRef {
                state: DadAddressStateRef { dad_state: state, core_ctx: address_ctx },
                retrans_timer,
                max_dad_transmits,
            })
        }

        fn send_dad_packet(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeDeviceId: &FakeDeviceId,
            dst_ip: MulticastAddr<Ipv6Addr>,
            message: NeighborSolicitation,
        ) -> Result<(), ()> {
            self.send_frame(bindings_ctx, DadMessageMeta { dst_ip, message }, EmptyBuf)
                .map_err(|EmptyBuf| ())
        }
    }

    const RETRANS_TIMER: NonZeroDuration =
        unsafe { NonZeroDuration::new_unchecked(Duration::from_secs(1)) };
    const DAD_ADDRESS: UnicastAddr<Ipv6Addr> =
        unsafe { UnicastAddr::new_unchecked(Ipv6Addr::new([0xa, 0, 0, 0, 0, 0, 0, 1])) };

    type FakeCtx = CtxPair<FakeCoreCtxImpl, FakeBindingsCtxImpl>;

    #[test]
    #[should_panic(expected = "expected address to be tentative")]
    fn panic_non_tentative_address_handle_timer() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::with_state(FakeDadContext {
                state: Ipv6DadState::Assigned,
                retrans_timer: RETRANS_TIMER,
                max_dad_transmits: None,
                address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                    addr: DAD_ADDRESS,
                    assigned: false,
                    groups: HashMap::default(),
                    ip_device_id_ctx: Default::default(),
                }),
            }));
        TimerHandler::handle_timer(&mut core_ctx, &mut bindings_ctx, dad_timer_id());
    }

    #[test]
    fn dad_disabled() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
                FakeCoreCtxImpl::with_state(FakeDadContext {
                    state: Ipv6DadState::Tentative {
                        dad_transmits_remaining: None,
                        timer: bindings_ctx.new_timer(dad_timer_id()),
                    },
                    retrans_timer: RETRANS_TIMER,
                    max_dad_transmits: None,
                    address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                        addr: DAD_ADDRESS,
                        assigned: false,
                        groups: HashMap::default(),
                        ip_device_id_ctx: Default::default(),
                    }),
                })
            });
        DadHandler::start_duplicate_address_detection(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Assigned);
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(*assigned);
        assert_eq!(groups, &HashMap::from([(DAD_ADDRESS.to_solicited_node_address(), 1)]));
        assert_eq!(
            bindings_ctx.take_events(),
            &[DadEvent::AddressAssigned { device: FakeDeviceId, addr: DAD_ADDRESS }][..]
        );
    }

    fn dad_timer_id() -> TestDadTimerId {
        DadTimerId {
            addr: FakeWeakAddressId(get_address_id(DAD_ADDRESS.get())),
            device_id: FakeWeakDeviceId(FakeDeviceId),
        }
    }

    fn check_dad(
        core_ctx: &FakeCoreCtxImpl,
        bindings_ctx: &FakeBindingsCtxImpl,
        frames_len: usize,
        dad_transmits_remaining: Option<NonZeroU16>,
        retrans_timer: NonZeroDuration,
    ) {
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Tentative {
            dad_transmits_remaining: got,
            timer: _
        } => {
            assert_eq!(got, dad_transmits_remaining);
        });
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(!*assigned);
        assert_eq!(groups, &HashMap::from([(DAD_ADDRESS.to_solicited_node_address(), 1)]));
        let frames = core_ctx.frames();
        assert_eq!(frames.len(), frames_len, "frames = {:?}", frames);
        let (DadMessageMeta { dst_ip, message }, frame) =
            frames.last().expect("should have transmitted a frame");

        assert_eq!(*dst_ip, DAD_ADDRESS.to_solicited_node_address());
        assert_eq!(*message, NeighborSolicitation::new(DAD_ADDRESS.get()));

        let options = Options::parse(&frame[..]).expect("parse NDP options");
        assert_eq!(options.iter().count(), 0);
        bindings_ctx
            .timers
            .assert_timers_installed([(dad_timer_id(), bindings_ctx.now() + retrans_timer.get())]);
    }

    #[test]
    fn perform_dad() {
        const DAD_TRANSMITS_REQUIRED: u16 = 5;
        const RETRANS_TIMER: NonZeroDuration =
            unsafe { NonZeroDuration::new_unchecked(Duration::from_secs(1)) };

        let mut ctx = FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
            FakeCoreCtxImpl::with_state(FakeDadContext {
                state: Ipv6DadState::Tentative {
                    dad_transmits_remaining: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                    timer: bindings_ctx.new_timer(dad_timer_id()),
                },
                retrans_timer: RETRANS_TIMER,
                max_dad_transmits: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                    addr: DAD_ADDRESS,
                    assigned: false,
                    groups: HashMap::default(),
                    ip_device_id_ctx: Default::default(),
                }),
            })
        });
        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        DadHandler::start_duplicate_address_detection(
            core_ctx,
            bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );

        for count in 0..=(DAD_TRANSMITS_REQUIRED - 1) {
            check_dad(
                core_ctx,
                bindings_ctx,
                usize::from(count + 1),
                NonZeroU16::new(DAD_TRANSMITS_REQUIRED - count - 1),
                RETRANS_TIMER,
            );
            assert_eq!(bindings_ctx.trigger_next_timer(core_ctx), Some(dad_timer_id()));
        }
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Assigned);
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(*assigned);
        assert_eq!(groups, &HashMap::from([(DAD_ADDRESS.to_solicited_node_address(), 1)]));
        assert_eq!(
            bindings_ctx.take_events(),
            &[DadEvent::AddressAssigned { device: FakeDeviceId, addr: DAD_ADDRESS }][..]
        );
    }

    #[test]
    fn stop_dad() {
        const DAD_TRANSMITS_REQUIRED: u16 = 2;
        const RETRANS_TIMER: NonZeroDuration =
            unsafe { NonZeroDuration::new_unchecked(Duration::from_secs(2)) };

        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_default_bindings_ctx(|bindings_ctx| {
                FakeCoreCtxImpl::with_state(FakeDadContext {
                    state: Ipv6DadState::Tentative {
                        dad_transmits_remaining: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                        timer: bindings_ctx.new_timer(dad_timer_id()),
                    },
                    retrans_timer: RETRANS_TIMER,
                    max_dad_transmits: NonZeroU16::new(DAD_TRANSMITS_REQUIRED),
                    address_ctx: FakeAddressCtxImpl::with_state(FakeDadAddressContext {
                        addr: DAD_ADDRESS,
                        assigned: false,
                        groups: HashMap::default(),
                        ip_device_id_ctx: Default::default(),
                    }),
                })
            });
        DadHandler::start_duplicate_address_detection(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );
        check_dad(
            &core_ctx,
            &bindings_ctx,
            1,
            NonZeroU16::new(DAD_TRANSMITS_REQUIRED - 1),
            RETRANS_TIMER,
        );

        DadHandler::stop_duplicate_address_detection(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &get_address_id(DAD_ADDRESS.get()),
        );
        bindings_ctx.timers.assert_no_timers_installed();
        let FakeDadContext { state, address_ctx, .. } = &core_ctx.state;
        assert_matches!(*state, Ipv6DadState::Uninitialized);
        let FakeDadAddressContext { assigned, groups, .. } = &address_ctx.state;
        assert!(!*assigned);
        assert_eq!(groups, &HashMap::new());
    }
}
