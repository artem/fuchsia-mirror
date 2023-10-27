// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Neighbor unreachability detection.

use alloc::{
    collections::{
        hash_map::{Entry, HashMap},
        BinaryHeap, VecDeque,
    },
    vec::Vec,
};
use core::{
    fmt::Debug,
    hash::Hash,
    iter::Iterator,
    marker::PhantomData,
    num::{NonZeroU32, NonZeroU8},
};

use assert_matches::assert_matches;
use derivative::Derivative;
use net_types::{
    ip::{Ip, IpAddr, Ipv4Addr, Ipv6Addr},
    SpecifiedAddr,
};
use packet::{Buf, BufferMut, Serializer};
use packet_formats::utils::NonZeroDuration;

use crate::{
    context::{TimerContext, TimerHandler},
    device::{
        link::{LinkAddress, LinkDevice},
        AnyDevice, DeviceIdContext, StrongId,
    },
    error::{AddressResolutionFailed, NotFoundError},
    Instant,
};

/// The maximum number of multicast solicitations as defined in [RFC 4861
/// section 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
const MAX_MULTICAST_SOLICIT: u8 = 3;

/// The maximum number of unicast solicitations as defined in [RFC 4861 section
/// 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
const MAX_UNICAST_SOLICIT: u8 = 3;

/// The maximum amount of time between retransmissions of neighbor probe
/// messages as defined in [RFC 7048 section 4].
///
/// [RFC 7048 section 4]: https://tools.ietf.org/html/rfc7048#section-4
const MAX_RETRANS_TIMER: NonZeroDuration =
    const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(60));

/// The exponential backoff factor for retransmissions of multicast neighbor
/// probe messages as defined in [RFC 7048 section 4].
///
/// [RFC 7048 section 4]: https://tools.ietf.org/html/rfc7048#section-4
const BACKOFF_MULTIPLE: NonZeroU32 = const_unwrap::const_unwrap_option(NonZeroU32::new(3));

const MAX_PENDING_FRAMES: usize = 10;

/// The time a neighbor is considered reachable after receiving a reachability
/// confirmation, as defined in [RFC 4861 section 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
const REACHABLE_TIME: NonZeroDuration =
    const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(30));

/// The time after which a neighbor in the DELAY state transitions to PROBE, as
/// defined in [RFC 4861 section 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
const DELAY_FIRST_PROBE_TIME: NonZeroDuration =
    const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(5));

/// The maximum number of neighbor entries in the neighbor table for a given
/// device. When the number of entries is above this number and an entry
/// transitions into a discardable state, a garbage collection task will be
/// scheduled to remove any entries that are not in use.
const MAX_ENTRIES: usize = 512;

/// The minimum amount of time between garbage collection passes when the
/// neighbor table grows beyond `MAX_SIZE`.
const MIN_GARBAGE_COLLECTION_INTERVAL: NonZeroDuration =
    const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(30));

#[derive(Debug, Copy, Clone)]
pub(crate) struct ConfirmationFlags {
    pub(crate) solicited_flag: bool,
    pub(crate) override_flag: bool,
}

/// The type of message with a dynamic neighbor update.
#[derive(Debug, Copy, Clone)]
pub(crate) enum DynamicNeighborUpdateSource {
    /// Indicates an update from a neighbor probe message.
    ///
    /// E.g. NDP Neighbor Solicitation.
    Probe,

    /// Indicates an update from a neighbor confirmation message.
    ///
    /// E.g. NDP Neighbor Advertisement.
    Confirmation(ConfirmationFlags),
}

#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(Clone, PartialEq(bound = ""), Eq))]
enum NeighborState<D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>> {
    Dynamic(DynamicNeighborState<D, I, N>),
    Static(D::Address),
}

/// The state of a dynamic entry in the neighbor cache within the Neighbor
/// Unreachability Detection state machine, defined in [RFC 4861 section 7.3.2]
/// and [RFC 7048 section 3].
///
/// [RFC 4861 section 7.3.2]: https://tools.ietf.org/html/rfc4861#section-7.3.2
/// [RFC 7048 section 3]: https://tools.ietf.org/html/rfc7048#section-3
#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(Clone(bound = ""), PartialEq(bound = ""), Eq))]
pub(crate) enum DynamicNeighborState<D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>> {
    /// Address resolution is being performed on the entry.
    ///
    /// Specifically, a probe has been sent to the solicited-node multicast
    /// address of the target, but the corresponding confirmation has not yet
    /// been received.
    Incomplete(Incomplete<D, N>),

    /// Positive confirmation was received within the last ReachableTime
    /// milliseconds that the forward path to the neighbor was functioning
    /// properly. While `Reachable`, no special action takes place as packets
    /// are sent.
    Reachable(Reachable<D, I>),

    /// More than ReachableTime milliseconds have elapsed since the last
    /// positive confirmation was received that the forward path was functioning
    /// properly. While stale, no action takes place until a packet is sent.
    ///
    /// The `Stale` state is entered upon receiving an unsolicited neighbor
    /// message that updates the cached link-layer address. Receipt of such a
    /// message does not confirm reachability, and entering the `Stale` state
    /// ensures reachability is verified quickly if the entry is actually being
    /// used. However, reachability is not actually verified until the entry is
    /// actually used.
    Stale(Stale<D>),

    /// A packet has been recently sent to the neighbor, which has stale
    /// reachability information (i.e. we have not received recent positive
    /// confirmation that the forward path is functioning properly).
    ///
    /// The `Delay` state is an optimization that gives upper-layer protocols
    /// additional time to provide reachability confirmation in those cases
    /// where ReachableTime milliseconds have passed since the last confirmation
    /// due to lack of recent traffic. Without this optimization, the opening of
    /// a TCP connection after a traffic lull would initiate probes even though
    /// the subsequent three-way handshake would provide a reachability
    /// confirmation almost immediately.
    Delay(Delay<D>),

    /// A reachability confirmation is actively sought by retransmitting probes
    /// every RetransTimer milliseconds until a reachability confirmation is
    /// received.
    Probe(Probe<D>),

    /// Similarly to the `Probe` state, a reachability confirmation is actively
    /// sought by retransmitting probes; however, probes are multicast to the
    /// solicited-node multicast address, using a timeout with exponential
    /// backoff, rather than unicast to the cached link address. Also, probes
    /// are only transmitted as long as packets continue to be sent to the
    /// neighbor.
    Unreachable(Unreachable<D>),
}

fn schedule_timer_if_should_retransmit<I, D, DeviceId, SC, C>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &DeviceId,
    neighbor: SpecifiedAddr<I::Addr>,
    event: NudEvent,
    counter: &mut Option<NonZeroU8>,
) -> bool
where
    I: Ip,
    D: LinkDevice,
    DeviceId: StrongId,
    C: NonSyncContext<I, D, DeviceId>,
    SC: NudConfigContext<I>,
{
    match counter {
        Some(c) => {
            *counter = NonZeroU8::new(c.get() - 1);
            let retransmit_timeout = sync_ctx.retransmit_timeout();
            assert_eq!(
                ctx.schedule_timer(
                    retransmit_timeout.get(),
                    NudTimerId::neighbor(device_id.clone(), neighbor, event)
                ),
                None
            );
            true
        }
        None => false,
    }
}

#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(PartialEq, Eq))]
pub(crate) struct Incomplete<D: LinkDevice, N: LinkResolutionNotifier<D>> {
    transmit_counter: Option<NonZeroU8>,
    pending_frames: VecDeque<Buf<Vec<u8>>>,
    #[derivative(PartialEq = "ignore")]
    notifiers: Vec<N>,
    _marker: PhantomData<D>,
}

#[cfg(test)]
impl<D: LinkDevice, N: LinkResolutionNotifier<D>> Clone for Incomplete<D, N> {
    fn clone(&self) -> Self {
        // Do not clone `notifiers` since the LinkResolutionNotifier type is not
        // required to implement `Clone` and notifiers are not used in equality
        // checks in tests.
        let Self { transmit_counter, pending_frames, notifiers: _, _marker } = self;
        Self {
            transmit_counter: transmit_counter.clone(),
            pending_frames: pending_frames.clone(),
            notifiers: Vec::new(),
            _marker: PhantomData,
        }
    }
}

impl<D: LinkDevice, N: LinkResolutionNotifier<D>> Drop for Incomplete<D, N> {
    fn drop(&mut self) {
        let Self { transmit_counter: _, pending_frames: _, notifiers, _marker } = self;
        for notifier in notifiers.drain(..) {
            notifier.notify(Err(AddressResolutionFailed));
        }
    }
}

impl<D: LinkDevice, N: LinkResolutionNotifier<D>> Incomplete<D, N> {
    fn new_with_pending_frame<I, SC, C, DeviceId>(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        frame: Buf<Vec<u8>>,
    ) -> Self
    where
        I: Ip,
        D: LinkDevice,
        C: NonSyncContext<I, D, DeviceId>,
        SC: NudConfigContext<I>,
        DeviceId: StrongId,
    {
        let mut this = Incomplete {
            transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT),
            pending_frames: [frame].into(),
            notifiers: Vec::new(),
            _marker: PhantomData,
        };
        // NB: transmission of a neighbor probe on entering INCOMPLETE (and subsequent
        // retransmissions) is done by `handle_timer`, as it need not be done with the
        // neighbor table lock held.
        assert!(this.schedule_timer_if_should_retransmit(sync_ctx, ctx, device_id, neighbor));
        this
    }

    fn new_with_notifier<I, SC, C, DeviceId>(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        notifier: C::Notifier,
    ) -> Self
    where
        I: Ip,
        D: LinkDevice,
        C: NonSyncContext<I, D, DeviceId, Notifier = N>,
        SC: NudConfigContext<I>,
        DeviceId: StrongId,
    {
        let mut this = Incomplete {
            transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT),
            pending_frames: VecDeque::new(),
            notifiers: [notifier].into(),
            _marker: PhantomData,
        };
        // NB: transmission of a neighbor probe on entering INCOMPLETE (and subsequent
        // retransmissions) is done by `handle_timer`, as it need not be done with the
        // neighbor table lock held.
        assert!(this.schedule_timer_if_should_retransmit(sync_ctx, ctx, device_id, neighbor));
        this
    }

    fn schedule_timer_if_should_retransmit<I, DeviceId, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> bool
    where
        I: Ip,
        D: LinkDevice,
        DeviceId: StrongId,
        C: NonSyncContext<I, D, DeviceId>,
        SC: NudConfigContext<I>,
    {
        let Self { transmit_counter, pending_frames: _, notifiers: _, _marker } = self;
        schedule_timer_if_should_retransmit(
            sync_ctx,
            ctx,
            device_id,
            neighbor,
            NudEvent::RetransmitMulticastProbe,
            transmit_counter,
        )
    }

    fn queue_packet<B, S>(&mut self, body: S) -> Result<(), S>
    where
        B: BufferMut,
        S: Serializer<Buffer = B>,
    {
        let Self { pending_frames, transmit_counter: _, notifiers: _, _marker } = self;

        // We don't accept new packets when the queue is full because earlier packets
        // are more likely to initiate connections whereas later packets are more likely
        // to carry data. E.g. A TCP SYN/SYN-ACK is likely to appear before a TCP
        // segment with data and dropping the SYN/SYN-ACK may result in the TCP peer not
        // processing the segment with data since the segment completing the handshake
        // has not been received and handled yet.
        if pending_frames.len() < MAX_PENDING_FRAMES {
            pending_frames.push_back(
                body.serialize_vec_outer()
                    .map_err(|(_err, s)| s)?
                    .map_a(|b| Buf::new(b.as_ref().to_vec(), ..))
                    .into_inner(),
            );
        }
        Ok(())
    }

    /// Flush pending packets to the resolved link address and notify any observers
    /// that link address resolution is complete.
    fn complete_resolution<I, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        link_address: D::Address,
    ) where
        I: Ip,
        D: LinkDevice,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: BufferNudSenderContext<Buf<Vec<u8>>, I, D, C>,
    {
        let Self { pending_frames, notifiers, transmit_counter: _, _marker } = self;

        // Send out pending packets while holding the NUD lock to prevent a potential
        // ordering violation.
        //
        // If we drop the NUD lock before sending out these queued packets, another
        // thread could take the NUD lock, observe that neighbor resolution is complete,
        // and send a packet *before* these pending packets are sent out, resulting in
        // out-of-order transmission to the device.
        for body in pending_frames.drain(..) {
            // Ignore any errors on sending the IP packet, because a failure at this point
            // is not actionable for the caller: failing to send a previously-queued packet
            // doesn't mean that updating the neighbor entry should fail.
            sync_ctx.send_ip_packet_to_neighbor_link_addr(ctx, link_address, body).unwrap_or_else(
                |_: Buf<Vec<u8>>| {
                    tracing::error!("failed to send pending IP packet to neighbor {link_address:?}")
                },
            )
        }
        for notifier in notifiers.drain(..) {
            notifier.notify(Ok(link_address));
        }
    }
}

#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(Clone, PartialEq, Eq))]
pub(crate) struct Reachable<D: LinkDevice, I: Instant> {
    pub(crate) link_address: D::Address,
    pub(crate) last_confirmed_at: I,
}

#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(Clone, PartialEq, Eq))]
pub(crate) struct Stale<D: LinkDevice> {
    pub(crate) link_address: D::Address,
}

impl<D: LinkDevice> Stale<D> {
    fn enter_delay<I, C, DeviceId: Clone>(
        &mut self,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> Delay<D>
    where
        I: Ip,
        C: NonSyncContext<I, D, DeviceId>,
    {
        let Self { link_address } = *self;

        // Start a timer to transition into PROBE after DELAY_FIRST_PROBE seconds if no
        // packets are sent to this neighbor.
        assert_eq!(
            ctx.schedule_timer(
                DELAY_FIRST_PROBE_TIME.get(),
                NudTimerId::neighbor(device_id.clone(), neighbor, NudEvent::DelayFirstProbe),
            ),
            None
        );

        Delay { link_address }
    }
}

#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(Clone, PartialEq, Eq))]
pub(crate) struct Delay<D: LinkDevice> {
    link_address: D::Address,
}

impl<D: LinkDevice> Delay<D> {
    fn enter_probe<I, DeviceId, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> Probe<D>
    where
        I: Ip,
        DeviceId: StrongId,
        C: NonSyncContext<I, D, DeviceId>,
        SC: NudConfigContext<I>,
    {
        let Self { link_address } = *self;

        // NB: transmission of a neighbor probe on entering PROBE (and subsequent
        // retransmissions) is done by `handle_timer`, as it need not be done with the
        // neighbor table lock held.
        let retransmit_timeout = sync_ctx.retransmit_timeout();
        assert_eq!(
            ctx.schedule_timer(
                retransmit_timeout.get(),
                NudTimerId::neighbor(device_id.clone(), neighbor, NudEvent::RetransmitUnicastProbe)
            ),
            None
        );

        Probe { link_address, transmit_counter: NonZeroU8::new(MAX_UNICAST_SOLICIT - 1) }
    }
}

#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(Clone, PartialEq, Eq))]
pub(crate) struct Probe<D: LinkDevice> {
    link_address: D::Address,
    transmit_counter: Option<NonZeroU8>,
}

impl<D: LinkDevice> Probe<D> {
    fn schedule_timer_if_should_retransmit<I, DeviceId, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> bool
    where
        I: Ip,
        DeviceId: StrongId,
        C: NonSyncContext<I, D, DeviceId>,
        SC: NudConfigContext<I>,
    {
        let Self { link_address: _, transmit_counter } = self;
        schedule_timer_if_should_retransmit(
            sync_ctx,
            ctx,
            device_id,
            neighbor,
            NudEvent::RetransmitUnicastProbe,
            transmit_counter,
        )
    }

    fn enter_unreachable<I, C, DeviceId>(
        &mut self,
        ctx: &mut C,
        device_id: &DeviceId,
        num_entries: usize,
        last_gc: &mut Option<C::Instant>,
    ) -> Unreachable<D>
    where
        I: Ip,
        C: NonSyncContext<I, D, DeviceId>,
        DeviceId: Clone,
    {
        // This entry is deemed discardable now that it is not in active use; schedule
        // garbage collection for the neighbor table if we are currently over the
        // maximum amount of entries.
        maybe_schedule_gc(ctx, device_id, num_entries, last_gc);

        let Self { link_address, transmit_counter: _ } = self;
        Unreachable { link_address: *link_address, mode: UnreachableMode::WaitingForPacketSend }
    }
}

#[derive(Debug, Derivative)]
#[cfg_attr(test, derivative(Clone, PartialEq, Eq))]
pub(crate) struct Unreachable<D: LinkDevice> {
    link_address: D::Address,
    mode: UnreachableMode,
}

/// The dynamic neighbor state specific to the UNREACHABLE state as defined in
/// [RFC 7048].
///
/// When a neighbor entry transitions to UNREACHABLE, the netstack will stop
/// actively retransmitting probes if no packets are being sent to the neighbor.
///
/// If packets are sent through the neighbor, the netstack will continue to
/// retransmit multicast probes, but with exponential backoff on the timer,
/// based on the `BACKOFF_MULTIPLE` and clamped at `MAX_RETRANS_TIMER`.
///
/// [RFC 7048]: https://tools.ietf.org/html/rfc7048
#[derive(Debug, Clone, Copy, Derivative)]
#[cfg_attr(test, derivative(PartialEq, Eq))]
pub(crate) enum UnreachableMode {
    WaitingForPacketSend,
    Backoff { probes_sent: NonZeroU32, packet_sent: bool },
}

impl UnreachableMode {
    /// The amount of time to wait before transmitting another multicast probe
    /// to the cached link address, based on how many probes we have transmitted
    /// so far, as defined in [RFC 7048 section 4].
    ///
    /// [RFC 7048 section 4]: https://tools.ietf.org/html/rfc7048#section-4
    fn next_backoff_retransmit_timeout<I, SC>(&self, sync_ctx: &mut SC) -> NonZeroDuration
    where
        I: Ip,
        SC: NudConfigContext<I>,
    {
        let probes_sent = match self {
            UnreachableMode::Backoff { probes_sent, packet_sent: _ } => probes_sent,
            UnreachableMode::WaitingForPacketSend => {
                panic!("cannot calculate exponential backoff in state {self:?}")
            }
        };
        // TODO(https://fxbug.dev/133437): vary this retransmit timeout by some random
        // "jitter factor" to avoid synchronization of transmissions from different
        // hosts.
        (sync_ctx.retransmit_timeout() * BACKOFF_MULTIPLE.saturating_pow(probes_sent.get()))
            .min(MAX_RETRANS_TIMER)
    }
}

impl<D: LinkDevice> Unreachable<D> {
    fn handle_timer<I, DeviceId, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> Option<TransmitProbe<D::Address>>
    where
        I: Ip,
        DeviceId: StrongId,
        C: NonSyncContext<I, D, DeviceId>,
        SC: NudConfigContext<I>,
    {
        let Self { link_address: _, mode } = self;
        match mode {
            UnreachableMode::WaitingForPacketSend => {
                panic!(
                    "timer should not have fired in UNREACHABLE while waiting for packet send; got \
                    a retransmit multicast probe event for {neighbor} on {device_id:?}",
                );
            }
            UnreachableMode::Backoff { probes_sent, packet_sent } => {
                if *packet_sent {
                    // It is all but guaranteed that we will never end up transmitting u32::MAX
                    // probes, given the retransmit timeout backs off to MAX_RETRANS_TIMER (1 minute
                    // by default), and u32::MAX minutes is over 8,000 years. By then we almost
                    // certainly would have garbage-collected the neighbor entry.
                    //
                    // But we do a saturating add just to be safe.
                    *probes_sent = probes_sent.saturating_add(1);
                    *packet_sent = false;

                    let duration = mode.next_backoff_retransmit_timeout(sync_ctx);
                    assert_eq!(
                        ctx.schedule_timer(
                            duration.into(),
                            NudTimerId::neighbor(
                                device_id.clone(),
                                neighbor,
                                NudEvent::RetransmitMulticastProbe,
                            )
                        ),
                        None
                    );

                    Some(TransmitProbe::Multicast)
                } else {
                    *mode = UnreachableMode::WaitingForPacketSend;

                    None
                }
            }
        }
    }

    /// Advance the UNREACHABLE state machine based on a packet being queued for
    /// transmission.
    ///
    /// Returns whether a multicast neighbor probe should be sent as a result.
    fn handle_packet_queued_to_send<I, DeviceId, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> bool
    where
        I: Ip,
        DeviceId: StrongId,
        C: NonSyncContext<I, D, DeviceId>,
        SC: NudConfigContext<I>,
    {
        let Self { link_address: _, mode } = self;
        match mode {
            UnreachableMode::WaitingForPacketSend => {
                // We already transmitted MAX_MULTICAST_SOLICIT probes to the neighbor
                // without confirmation, but now a packet is being sent to that neighbor, so
                // we are resuming transmission of probes for as long as packets continue to
                // be sent to the neighbor. Instead of retransmitting on a fixed timeout,
                // use exponential backoff per [RFC 7048 section 4]:
                //
                //   If an implementation transmits more than MAX_UNICAST_SOLICIT/
                //   MAX_MULTICAST_SOLICIT packets, then it SHOULD use the exponential
                //   backoff of the retransmit timer.  This is to avoid any significant
                //   load due to a steady background level of retransmissions from
                //   implementations that retransmit a large number of Neighbor
                //   Solicitations (NS) before discarding the NCE.
                //
                // [RFC 7048 section 4]: https://tools.ietf.org/html/rfc7048#section-4
                let probes_sent = NonZeroU32::new(1).unwrap();
                *mode = UnreachableMode::Backoff { probes_sent, packet_sent: false };

                let duration = mode.next_backoff_retransmit_timeout(sync_ctx);
                assert_eq!(
                    ctx.schedule_timer(
                        duration.into(),
                        NudTimerId::neighbor(
                            device_id.clone(),
                            neighbor,
                            NudEvent::RetransmitMulticastProbe,
                        )
                    ),
                    None
                );

                // Transmit a multicast probe.
                true
            }
            UnreachableMode::Backoff { probes_sent: _, packet_sent } => {
                // We are in the exponential backoff phase of sending probes. Make a note
                // that a packet was sent since the last transmission so that we will send
                // another when the timer fires.
                *packet_sent = true;

                false
            }
        }
    }
}

impl<D: LinkDevice, Time: Instant, N: LinkResolutionNotifier<D>> DynamicNeighborState<D, Time, N> {
    fn cancel_timer<I, C, DeviceId>(
        &mut self,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) where
        I: Ip,
        DeviceId: StrongId,
        C: NonSyncContext<I, D, DeviceId>,
    {
        match self {
            DynamicNeighborState::Incomplete(Incomplete {
                transmit_counter: _,
                pending_frames: _,
                notifiers: _,
                _marker,
            }) => {
                assert_ne!(
                    ctx.cancel_timer(NudTimerId::neighbor(
                        device_id.clone(),
                        neighbor,
                        NudEvent::RetransmitMulticastProbe,
                    )),
                    None,
                    "incomplete entry for {neighbor} should have had a timer",
                );
            }
            DynamicNeighborState::Reachable(Reachable {
                link_address: _,
                last_confirmed_at: _,
            }) => {
                assert_ne!(
                    ctx.cancel_timer(NudTimerId::neighbor(
                        device_id.clone(),
                        neighbor,
                        NudEvent::ReachableTime,
                    )),
                    None,
                    "reachable entry for {neighbor} should have had a timer",
                );
            }
            DynamicNeighborState::Stale(Stale { link_address: _ }) => {}
            DynamicNeighborState::Delay(Delay { link_address: _ }) => {
                assert_ne!(
                    ctx.cancel_timer(NudTimerId::neighbor(
                        device_id.clone(),
                        neighbor,
                        NudEvent::DelayFirstProbe,
                    )),
                    None,
                    "delay entry for {neighbor} should have had a timer",
                );
            }
            DynamicNeighborState::Probe(Probe { link_address: _, transmit_counter: _ }) => {
                assert_ne!(
                    ctx.cancel_timer(NudTimerId::neighbor(
                        device_id.clone(),
                        neighbor,
                        NudEvent::RetransmitUnicastProbe,
                    )),
                    None,
                    "probe entry for {neighbor} should have had a timer",
                );
            }
            DynamicNeighborState::Unreachable(Unreachable { link_address: _, mode }) => {
                // A timer should be scheduled iff a packet was recently sent to the neighbor
                // and we are retransmitting probes with exponential backoff.
                match mode {
                    UnreachableMode::WaitingForPacketSend => {}
                    UnreachableMode::Backoff { probes_sent: _, packet_sent: _ } => {
                        assert_ne!(
                            ctx.cancel_timer(NudTimerId::neighbor(
                                device_id.clone(),
                                neighbor,
                                NudEvent::RetransmitMulticastProbe,
                            )),
                            None,
                            "unreachable entry for {neighbor} in {mode:?} should have had a timer",
                        );
                    }
                }
            }
        }
    }

    fn cancel_timer_and_complete_resolution<I, SC, C>(
        mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_address: D::Address,
    ) where
        I: Ip,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: BufferNudSenderContext<Buf<Vec<u8>>, I, D, C>,
    {
        self.cancel_timer(ctx, device_id, neighbor);

        match self {
            DynamicNeighborState::Incomplete(mut incomplete) => {
                incomplete.complete_resolution(sync_ctx, ctx, link_address);
            }
            DynamicNeighborState::Reachable(_)
            | DynamicNeighborState::Stale(_)
            | DynamicNeighborState::Delay(_)
            | DynamicNeighborState::Probe(_)
            | DynamicNeighborState::Unreachable(_) => {}
        }
    }

    fn enter_reachable<I, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_address: D::Address,
    ) where
        I: Ip,
        C: NonSyncContext<I, D, SC::DeviceId, Instant = Time>,
        SC: BufferNudSenderContext<Buf<Vec<u8>>, I, D, C>,
    {
        // TODO(https://fxbug.dev/124960): if the new state matches the current state,
        // update the link address as necessary, but do not cancel + reschedule timers.
        let now = ctx.now();
        match self {
            // If this neighbor entry is already in REACHABLE, rather than proactively
            // rescheduling the timer (which can be a relatively expensive operation
            // especially in the hot path), simply update `last_confirmed_at` so that when
            // the timer does eventually fire, we can reschedule it accordingly.
            DynamicNeighborState::Reachable(Reachable {
                link_address: current,
                last_confirmed_at,
            }) if *current == link_address => {
                *last_confirmed_at = now;
                return;
            }
            DynamicNeighborState::Incomplete(_)
            | DynamicNeighborState::Reachable(_)
            | DynamicNeighborState::Stale(_)
            | DynamicNeighborState::Delay(_)
            | DynamicNeighborState::Probe(_)
            | DynamicNeighborState::Unreachable(_) => {}
        }
        let previous = core::mem::replace(
            self,
            DynamicNeighborState::Reachable(Reachable { link_address, last_confirmed_at: now }),
        );
        previous.cancel_timer_and_complete_resolution(
            sync_ctx,
            ctx,
            device_id,
            neighbor,
            link_address,
        );
        assert_eq!(
            ctx.schedule_timer(
                REACHABLE_TIME.get(),
                NudTimerId::neighbor(device_id.clone(), neighbor, NudEvent::ReachableTime),
            ),
            None
        );
    }

    fn enter_stale<I, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_address: D::Address,
        num_entries: usize,
        last_gc: &mut Option<C::Instant>,
    ) where
        I: Ip,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: BufferNudSenderContext<Buf<Vec<u8>>, I, D, C>,
    {
        // TODO(https://fxbug.dev/124960): if the new state matches the current state,
        // update the link address as necessary, but do not cancel + reschedule timers.
        let previous =
            core::mem::replace(self, DynamicNeighborState::Stale(Stale { link_address }));
        previous.cancel_timer_and_complete_resolution(
            sync_ctx,
            ctx,
            device_id,
            neighbor,
            link_address,
        );

        // This entry is deemed discardable now that it is not in active use; schedule
        // garbage collection for the neighbor table if we are currently over the
        // maximum amount of entries.
        maybe_schedule_gc(ctx, device_id, num_entries, last_gc);

        // Stale entries don't do anything until an outgoing packet is queued for
        // transmission.
    }

    /// Resolve the cached link address for this neighbor entry, or return an
    /// observer for an unresolved neighbor, and advance the NUD state machine
    /// accordingly (as if a packet had been sent to the neighbor).
    ///
    /// Also returns whether a multicast neighbor probe should be sent as a result.
    fn resolve_link_addr<I, DeviceId, C, SC>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> (
        LinkResolutionResult<
            D::Address,
            <<C as LinkResolutionContext<D>>::Notifier as LinkResolutionNotifier<D>>::Observer,
        >,
        bool,
    )
    where
        I: Ip,
        DeviceId: StrongId,
        C: NonSyncContext<I, D, DeviceId, Notifier = N>,
        SC: NudConfigContext<I>,
    {
        match self {
            DynamicNeighborState::Incomplete(Incomplete {
                notifiers,
                transmit_counter: _,
                pending_frames: _,
                _marker,
            }) => {
                let (notifier, observer) = C::Notifier::new();
                notifiers.push(notifier);

                (LinkResolutionResult::Pending(observer), false)
            }
            DynamicNeighborState::Stale(entry) => {
                // Advance the state machine as if a packet had been sent to this neighbor.
                //
                // This is not required by the RFC, and it may result in neighbor probes going
                // out for this neighbor that would not have otherwise (the only other way a
                // STALE entry moves to DELAY is due to a packet being sent to it). However,
                // sending neighbor probes to confirm reachability is likely to be useful given
                // a client is attempting to resolve this neighbor. Additionally, this maintains
                // consistency with Netstack2's behavior.
                let delay @ Delay { link_address } = entry.enter_delay(ctx, device_id, neighbor);
                *self = DynamicNeighborState::Delay(delay);

                (LinkResolutionResult::Resolved(link_address), false)
            }
            DynamicNeighborState::Reachable(Reachable { link_address, last_confirmed_at: _ })
            | DynamicNeighborState::Delay(Delay { link_address })
            | DynamicNeighborState::Probe(Probe { link_address, transmit_counter: _ }) => {
                (LinkResolutionResult::Resolved(*link_address), false)
            }
            DynamicNeighborState::Unreachable(unreachable) => {
                let Unreachable { link_address, mode: _ } = unreachable;
                let link_address = *link_address;

                // Advance the state machine as if a packet had been sent to this neighbor.
                let do_multicast_solicit =
                    unreachable.handle_packet_queued_to_send(sync_ctx, ctx, device_id, neighbor);
                (LinkResolutionResult::Resolved(link_address), do_multicast_solicit)
            }
        }
    }

    /// Handle a packet being queued for transmission: either queue it as a
    /// pending packet for an unresolved neighbor, or send it to the cached link
    /// address, and advance the NUD state machine accordingly.
    ///
    /// Returns whether a multicast neighbor probe should be sent as a result.
    fn handle_packet_queued_to_send<B, I, C, SC, S>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        body: S,
    ) -> Result<bool, S>
    where
        B: BufferMut,
        I: Ip,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: BufferNudSenderContext<B, I, D, C>,
        S: Serializer<Buffer = B>,
    {
        match self {
            DynamicNeighborState::Incomplete(incomplete) => {
                incomplete.queue_packet(body)?;

                Ok(false)
            }
            // Send the IP packet while holding the NUD lock to prevent a potential
            // ordering violation.
            //
            // If we drop the NUD lock before sending out this packet, another thread
            // could take the NUD lock and send a packet *before* this packet is sent
            // out, resulting in out-of-order transmission to the device.
            DynamicNeighborState::Stale(entry) => {
                // Per [RFC 4861 section 7.3.3]:
                //
                //   The first time a node sends a packet to a neighbor whose entry is
                //   STALE, the sender changes the state to DELAY and sets a timer to
                //   expire in DELAY_FIRST_PROBE_TIME seconds.
                //
                // [RFC 4861 section 7.3.3]: https://tools.ietf.org/html/rfc4861#section-7.3.3
                let delay @ Delay { link_address } = entry.enter_delay(ctx, device_id, neighbor);
                *self = DynamicNeighborState::Delay(delay);

                sync_ctx.send_ip_packet_to_neighbor_link_addr(ctx, link_address, body)?;

                Ok(false)
            }
            DynamicNeighborState::Reachable(Reachable { link_address, last_confirmed_at: _ })
            | DynamicNeighborState::Delay(Delay { link_address })
            | DynamicNeighborState::Probe(Probe { link_address, transmit_counter: _ }) => {
                sync_ctx.send_ip_packet_to_neighbor_link_addr(ctx, *link_address, body)?;

                Ok(false)
            }
            DynamicNeighborState::Unreachable(unreachable) => {
                let Unreachable { link_address, mode: _ } = unreachable;
                sync_ctx.send_ip_packet_to_neighbor_link_addr(ctx, *link_address, body)?;

                let do_multicast_solicit =
                    unreachable.handle_packet_queued_to_send(sync_ctx, ctx, device_id, neighbor);
                Ok(do_multicast_solicit)
            }
        }
    }

    fn handle_probe<I, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_address: D::Address,
        num_entries: usize,
        last_gc: &mut Option<C::Instant>,
    ) where
        I: Ip,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: BufferNudSenderContext<Buf<Vec<u8>>, I, D, C>,
    {
        // Per [RFC 4861 section 7.2.3] ("Receipt of Neighbor Solicitations"):
        //
        //   If an entry already exists, and the cached link-layer address
        //   differs from the one in the received Source Link-Layer option, the
        //   cached address should be replaced by the received address, and the
        //   entry's reachability state MUST be set to STALE.
        //
        // [RFC 4861 section 7.2.3]: https://tools.ietf.org/html/rfc4861#section-7.2.3
        let transition_to_stale = match self {
            DynamicNeighborState::Incomplete(_) => true,
            DynamicNeighborState::Reachable(Reachable {
                link_address: current,
                last_confirmed_at: _,
            })
            | DynamicNeighborState::Stale(Stale { link_address: current })
            | DynamicNeighborState::Delay(Delay { link_address: current })
            | DynamicNeighborState::Probe(Probe { link_address: current, transmit_counter: _ })
            | DynamicNeighborState::Unreachable(Unreachable { link_address: current, mode: _ }) => {
                current != &link_address
            }
        };
        if transition_to_stale {
            self.enter_stale(
                sync_ctx,
                ctx,
                device_id,
                neighbor,
                link_address,
                num_entries,
                last_gc,
            );
        }
    }

    fn handle_confirmation<I, SC, C>(
        &mut self,
        sync_ctx: &mut SC,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_address: D::Address,
        flags: ConfirmationFlags,
        num_entries: usize,
        last_gc: &mut Option<C::Instant>,
    ) where
        I: Ip,
        C: NonSyncContext<I, D, SC::DeviceId, Instant = Time>,
        SC: BufferNudSenderContext<Buf<Vec<u8>>, I, D, C>,
    {
        let ConfirmationFlags { solicited_flag, override_flag } = flags;
        enum NewState<A> {
            Reachable { link_address: A },
            Stale { link_address: A },
        }

        let new_state = match self {
            DynamicNeighborState::Incomplete(Incomplete {
                transmit_counter: _,
                pending_frames: _,
                notifiers: _,
                _marker,
            }) => {
                // Per RFC 4861 section 7.2.5:
                //
                //   If the advertisement's Solicited flag is set, the state of the
                //   entry is set to REACHABLE; otherwise, it is set to STALE.
                //
                //   Note that the Override flag is ignored if the entry is in the
                //   INCOMPLETE state.
                if solicited_flag {
                    Some(NewState::Reachable { link_address })
                } else {
                    Some(NewState::Stale { link_address })
                }
            }
            DynamicNeighborState::Reachable(Reachable {
                link_address: current,
                last_confirmed_at: _,
            })
            | DynamicNeighborState::Stale(Stale { link_address: current })
            | DynamicNeighborState::Delay(Delay { link_address: current })
            | DynamicNeighborState::Probe(Probe { link_address: current, transmit_counter: _ })
            | DynamicNeighborState::Unreachable(Unreachable { link_address: current, mode: _ }) => {
                let updated_link_address = current != &link_address;

                match (solicited_flag, updated_link_address, override_flag) {
                    // Per RFC 4861 section 7.2.5:
                    //
                    //   If [either] the Override flag is set, or the supplied link-layer address is
                    //   the same as that in the cache, [and] ... the Solicited flag is set, the
                    //   entry MUST be set to REACHABLE.
                    (true, _, true) | (true, false, _) => {
                        Some(NewState::Reachable { link_address })
                    }
                    // Per RFC 4861 section 7.2.5:
                    //
                    //   If the Override flag is clear and the supplied link-layer address differs
                    //   from that in the cache, then one of two actions takes place:
                    //
                    //    a. If the state of the entry is REACHABLE, set it to STALE, but do not
                    //       update the entry in any other way.
                    //    b. Otherwise, the received advertisement should be ignored and MUST NOT
                    //       update the cache.
                    (_, true, false) => match self {
                        // NB: do not update the link address.
                        DynamicNeighborState::Reachable(Reachable {
                            link_address,
                            last_confirmed_at: _,
                        }) => Some(NewState::Stale { link_address: *link_address }),
                        // Ignore the advertisement and do not update the cache.
                        DynamicNeighborState::Stale(_)
                        | DynamicNeighborState::Delay(_)
                        | DynamicNeighborState::Probe(_)
                        | DynamicNeighborState::Unreachable(_) => None,
                        // The INCOMPLETE state was already handled in the outer match.
                        DynamicNeighborState::Incomplete(_) => unreachable!(),
                    },
                    // Per RFC 4861 section 7.2.5:
                    //
                    //   If the Override flag is set [and] ... the Solicited flag is zero and the
                    //   link-layer address was updated with a different address, the state MUST be
                    //   set to STALE.
                    (false, true, true) => Some(NewState::Stale { link_address }),
                    // Per RFC 4861 section 7.2.5:
                    //
                    //   There is no need to update the state for unsolicited advertisements that do
                    //   not change the contents of the cache.
                    (false, false, _) => None,
                }
            }
        };
        match new_state {
            Some(NewState::Reachable { link_address }) => {
                self.enter_reachable(sync_ctx, ctx, device_id, neighbor, link_address)
            }
            Some(NewState::Stale { link_address }) => self.enter_stale(
                sync_ctx,
                ctx,
                device_id,
                neighbor,
                link_address,
                num_entries,
                last_gc,
            ),
            None => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    pub(crate) fn assert_dynamic_neighbor_with_addr<
        I: Ip,
        D: LinkDevice,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: NudContext<I, D, C>,
    >(
        sync_ctx: &mut SC,
        device_id: SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        expected_link_addr: D::Address,
    ) {
        sync_ctx.with_nud_state_mut(&device_id, |NudState { neighbors, last_gc: _ }, _config| {
            assert_matches!(
                neighbors.get(&neighbor),
                Some(NeighborState::Dynamic(
                    DynamicNeighborState::Reachable(Reachable{ link_address, last_confirmed_at: _ })
                    | DynamicNeighborState::Stale(Stale{ link_address })
                )) => {
                    assert_eq!(link_address, &expected_link_addr)
                }
            )
        })
    }

    pub(crate) fn assert_dynamic_neighbor_state<I, D, C, SC>(
        sync_ctx: &mut SC,
        device_id: SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        expected_state: DynamicNeighborState<D, C::Instant, C::Notifier>,
    ) where
        I: Ip,
        D: LinkDevice + PartialEq,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: NudContext<I, D, C>,
    {
        sync_ctx.with_nud_state_mut(&device_id, |NudState { neighbors, last_gc: _ }, _config| {
            assert_matches!(
                neighbors.get(&neighbor),
                Some(NeighborState::Dynamic(state)) => {
                    assert_eq!(state, &expected_state)
                }
            )
        })
    }

    pub(crate) fn assert_neighbor_unknown<
        I: Ip,
        D: LinkDevice,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: NudContext<I, D, C>,
    >(
        sync_ctx: &mut SC,
        device_id: SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) {
        sync_ctx.with_nud_state_mut(&device_id, |NudState { neighbors, last_gc: _ }, _config| {
            assert_matches!(neighbors.get(&neighbor), None)
        })
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
enum NudEvent {
    RetransmitMulticastProbe,
    ReachableTime,
    DelayFirstProbe,
    RetransmitUnicastProbe,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct NudTimerId<I: Ip, D: LinkDevice, DeviceId>(DeviceId, NudTimerInner<I, D>);

impl<I: Ip, D: LinkDevice, DeviceId> NudTimerId<I, D, DeviceId> {
    fn garbage_collection(device_id: DeviceId) -> Self {
        NudTimerId(device_id, NudTimerInner::GarbageCollection)
    }

    fn neighbor(device_id: DeviceId, neighbor: SpecifiedAddr<I::Addr>, event: NudEvent) -> Self {
        NudTimerId(
            device_id,
            NudTimerInner::Neighbor { lookup_addr: neighbor, event, _marker: PhantomData },
        )
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
enum NudTimerInner<I: Ip, D: LinkDevice> {
    GarbageCollection,
    Neighbor { lookup_addr: SpecifiedAddr<I::Addr>, event: NudEvent, _marker: PhantomData<D> },
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct NudState<I: Ip, D: LinkDevice, Time: Instant, N: LinkResolutionNotifier<D>> {
    // TODO(https://fxbug.dev/126138): Key neighbors by `UnicastAddr`.
    neighbors: HashMap<SpecifiedAddr<I::Addr>, NeighborState<D, Time, N>>,
    last_gc: Option<Time>,
}

impl<I: Ip, D: LinkDevice, T: Instant, N: LinkResolutionNotifier<D>> NudState<I, D, T, N> {
    pub(crate) fn state_iter(
        &self,
    ) -> impl Iterator<Item = NeighborStateInspect<D::Address, T>> + '_ {
        self.neighbors.iter().map(|(ip_address, state)| {
            let (state, link_address, last_confirmed_at) = match state {
                NeighborState::Static(addr) => ("Static", Some(*addr), None),
                NeighborState::Dynamic(dynamic_state) => match dynamic_state {
                    DynamicNeighborState::Incomplete(Incomplete {
                        transmit_counter: _,
                        pending_frames: _,
                        notifiers: _,
                        _marker,
                    }) => ("Incomplete", None, None),
                    DynamicNeighborState::Reachable(Reachable {
                        link_address,
                        last_confirmed_at,
                    }) => ("Reachable", Some(*link_address), Some(*last_confirmed_at)),
                    DynamicNeighborState::Stale(Stale { link_address }) => {
                        ("Stale", Some(*link_address), None)
                    }
                    DynamicNeighborState::Delay(Delay { link_address }) => {
                        ("Delay", Some(*link_address), None)
                    }
                    DynamicNeighborState::Probe(Probe { link_address, transmit_counter: _ }) => {
                        ("Probe", Some(*link_address), None)
                    }
                    DynamicNeighborState::Unreachable(Unreachable { link_address, mode: _ }) => {
                        ("Unreachable", Some(*link_address), None)
                    }
                },
            };
            NeighborStateInspect {
                state: state,
                ip_address: IpAddr::from(*ip_address),
                link_address,
                last_confirmed_at,
            }
        })
    }
}

/// A snapshot of the state of a neighbor, for exporting to Inspect.
pub struct NeighborStateInspect<LinkAddress: Debug, T: Instant> {
    /// The NUD state of the neighbor.
    pub state: &'static str,
    /// The neighbor's IP address.
    pub ip_address: IpAddr<SpecifiedAddr<Ipv4Addr>, SpecifiedAddr<Ipv6Addr>>,
    /// The neighbor's link address.
    pub link_address: Option<LinkAddress>,
    /// The last instant at which the neighbor's reachability was confirmed.
    pub last_confirmed_at: Option<T>,
}

/// The non-synchronized context for NUD.
pub(crate) trait NonSyncContext<I: Ip, D: LinkDevice, DeviceId>:
    TimerContext<NudTimerId<I, D, DeviceId>> + LinkResolutionContext<D>
{
}

impl<
        I: Ip,
        D: LinkDevice,
        DeviceId,
        C: TimerContext<NudTimerId<I, D, DeviceId>> + LinkResolutionContext<D>,
    > NonSyncContext<I, D, DeviceId> for C
{
}

/// An execution context that allows creating link resolution notifiers.
pub trait LinkResolutionContext<D: LinkDevice> {
    /// A notifier held by core that can be used to inform interested parties of
    /// the result of link address resolution.
    type Notifier: LinkResolutionNotifier<D>;
}

/// A notifier held by core that can be used to inform interested parties of the
/// result of link address resolution.
pub trait LinkResolutionNotifier<D: LinkDevice>: Debug + Sized + Send {
    /// The corresponding observer that can be used to observe the result of
    /// link address resolution.
    type Observer;

    /// Create a connected (notifier, observer) pair.
    fn new() -> (Self, Self::Observer);

    /// Signal to Bindings that link address resolution has completed for a
    /// neighbor.
    fn notify(self, result: Result<D::Address, AddressResolutionFailed>);
}

/// The execution context for NUD for a link device.
pub(crate) trait NudContext<I: Ip, D: LinkDevice, C: NonSyncContext<I, D, Self::DeviceId>>:
    DeviceIdContext<D>
{
    type ConfigCtx<'a>: NudConfigContext<I>;

    /// Calls the function with a mutable reference to the NUD state and NUD
    /// configuration for the device.
    fn with_nud_state_mut<
        O,
        F: FnOnce(&mut NudState<I, D, C::Instant, C::Notifier>, &mut Self::ConfigCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Sends a neighbor probe/solicitation message.
    ///
    /// If `remote_link_addr` is provided, the message will be unicasted to that
    /// address; if it is `None`, the message will be multicast.
    fn send_neighbor_solicitation(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        lookup_addr: SpecifiedAddr<I::Addr>,
        remote_link_addr: Option<D::Address>,
    );
}

/// The execution context for NUD that allows accessing NUD configuration (such
/// as timer durations) for a particular device.
pub(crate) trait NudConfigContext<I: Ip> {
    /// The amount of time between retransmissions of neighbor probe messages.
    ///
    /// This corresponds to the configurable per-interface `RetransTimer` value
    /// used in NUD as defined in [RFC 4861 section 6.3.2].
    ///
    /// [RFC 4861 section 6.3.2]: https://datatracker.ietf.org/doc/html/rfc4861#section-6.3.2
    fn retransmit_timeout(&mut self) -> NonZeroDuration;
}

/// The execution context for NUD for a link device, with a buffer.
pub(crate) trait BufferNudContext<
    B: BufferMut,
    I: Ip,
    D: LinkDevice,
    C: NonSyncContext<I, D, Self::DeviceId>,
>: NudContext<I, D, C>
{
    type BufferSenderCtx<'a>: BufferNudSenderContext<B, I, D, C, DeviceId = Self::DeviceId>;

    /// Calls the function with a mutable reference to the NUD state and the
    /// synchronized context.
    fn with_nud_state_mut_and_buf_ctx<
        O,
        F: FnOnce(&mut NudState<I, D, C::Instant, C::Notifier>, &mut Self::BufferSenderCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// The execution context for NUD for a link device that allows sending IP
/// packets to specific neighbors.
pub(crate) trait BufferNudSenderContext<
    B: BufferMut,
    I: Ip,
    D: LinkDevice,
    C: NonSyncContext<I, D, Self::DeviceId>,
>: NudConfigContext<I> + DeviceIdContext<D>
{
    /// Send an IP frame to the neighbor with the specified link address.
    fn send_ip_packet_to_neighbor_link_addr<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        neighbor_link_addr: D::Address,
        body: S,
    ) -> Result<(), S>;
}

/// An implementation of NUD for the IP layer.
pub(crate) trait NudIpHandler<I: Ip, C>: DeviceIdContext<AnyDevice> {
    /// Handles an incoming neighbor probe message.
    ///
    /// For IPv6, this can be an NDP Neighbor Solicitation or an NDP Router
    /// Advertisement message.
    fn handle_neighbor_probe(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    );

    /// Handles an incoming neighbor confirmation message.
    ///
    /// For IPv6, this can be an NDP Neighbor Advertisement.
    fn handle_neighbor_confirmation(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
        flags: ConfirmationFlags,
    );

    /// Clears the neighbor table.
    fn flush_neighbor_table(&mut self, ctx: &mut C, device_id: &Self::DeviceId);
}

/// Specifies the link-layer address of a neighbor.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LinkResolutionResult<A: LinkAddress, Observer> {
    /// The destination is a known neighbor with the given link-layer address.
    Resolved(A),
    /// The destination is pending neighbor resolution.
    Pending(Observer),
}

/// An implementation of NUD for a link device.
pub(crate) trait NudHandler<I: Ip, D: LinkDevice, C: LinkResolutionContext<D>>:
    DeviceIdContext<D>
{
    /// Sets a dynamic neighbor's entry state to the specified values in
    /// response to the source packet.
    fn handle_neighbor_update(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        // TODO(https://fxbug.dev/126138): Use IPv4 subnet information to
        // disallow the address with all host bits equal to 0, and the
        // subnet broadcast addresses with all host bits equal to 1.
        // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
        neighbor: SpecifiedAddr<I::Addr>,
        // TODO(https://fxbug.dev/134102): Wrap in `UnicastAddr`.
        link_addr: D::Address,
        source: DynamicNeighborUpdateSource,
    );

    /// Sets a static neighbor entry for the neighbor.
    ///
    /// If no entry exists, a new one may be created. If an entry already
    /// exists, it will be updated with the provided link address and set
    /// to be a static entry.
    ///
    /// Dynamic updates for the neighbor will be ignored for static entries.
    fn set_static_neighbor(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        // TODO(https://fxbug.dev/126138): Use IPv4 subnet information to
        // disallow the address with all host bits equal to 0, and the
        // subnet broadcast addresses with all host bits equal to 1.
        // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
        neighbor: SpecifiedAddr<I::Addr>,
        // TODO(https://fxbug.dev/134102): Wrap in `UnicastAddr`.
        link_addr: D::Address,
    );

    /// Deletes a static or dynamic neighbor entry.
    fn delete_neighbor(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        // TODO(https://fxbug.dev/126138): Use IPv4 subnet information to
        // disallow subnet and subnet broadcast addresses.
        // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> Result<(), NotFoundError>;

    /// Clears the neighbor table.
    fn flush(&mut self, ctx: &mut C, device_id: &Self::DeviceId);

    /// Resolve the link-address of a device's neighbor.
    fn resolve_link_addr(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        // TODO(https://fxbug.dev/126138): Use IPv4 subnet information to
        // disallow subnet and subnet broadcast addresses.
        // TODO(https://fxbug.dev/134098): Use NeighborAddr when available.
        dst: &SpecifiedAddr<I::Addr>,
    ) -> LinkResolutionResult<
        D::Address,
        <<C as LinkResolutionContext<D>>::Notifier as LinkResolutionNotifier<D>>::Observer,
    >;
}

/// An implementation of NUD for a link device, with a buffer.
pub(crate) trait BufferNudHandler<B: BufferMut, I: Ip, D: LinkDevice, C>:
    DeviceIdContext<D>
{
    /// Send an IP packet to the neighbor.
    ///
    /// If the neighbor's link address is not known, link address resolution
    /// is performed.
    fn send_ip_packet_to_neighbor<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        body: S,
    ) -> Result<(), S>;
}

enum TransmitProbe<A> {
    Multicast,
    Unicast(A),
}

impl<I: Ip, D: LinkDevice, C: NonSyncContext<I, D, SC::DeviceId>, SC: NudContext<I, D, C>>
    TimerHandler<C, NudTimerId<I, D, SC::DeviceId>> for SC
{
    fn handle_timer(
        &mut self,
        ctx: &mut C,
        NudTimerId(device_id, timer): NudTimerId<I, D, SC::DeviceId>,
    ) {
        match timer {
            NudTimerInner::GarbageCollection => collect_garbage(self, ctx, &device_id),
            NudTimerInner::Neighbor { lookup_addr, event, _marker } => {
                handle_neighbor_timer(self, ctx, &device_id, lookup_addr, event)
            }
        }
    }
}

fn handle_neighbor_timer<I, D, SC, C>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    lookup_addr: SpecifiedAddr<I::Addr>,
    event: NudEvent,
) where
    I: Ip,
    D: LinkDevice,
    C: NonSyncContext<I, D, SC::DeviceId>,
    SC: NudContext<I, D, C>,
{
    let action =
        sync_ctx.with_nud_state_mut(&device_id, |NudState { neighbors, last_gc }, sync_ctx| {
            let num_entries = neighbors.len();
            let mut entry = match neighbors.entry(lookup_addr) {
                Entry::Occupied(entry) => entry,
                Entry::Vacant(_) => panic!("timer fired for invalid entry"),
            };

            match entry.get_mut() {
                NeighborState::Dynamic(DynamicNeighborState::Incomplete(incomplete)) => {
                    assert_eq!(event, NudEvent::RetransmitMulticastProbe);

                    if incomplete.schedule_timer_if_should_retransmit(
                        sync_ctx,
                        ctx,
                        &device_id,
                        lookup_addr,
                    ) {
                        Some(TransmitProbe::Multicast)
                    } else {
                        // Failed to complete neighbor resolution and no more probes to send.
                        // Subsequent traffic to this neighbor will recreate the entry and restart
                        // address resolution.
                        //
                        // TODO(http://fxbug.dev/132349): consider retaining this neighbor entry in
                        // a sentinel `Failed` state, equivalent to its having been discarded except
                        // for debugging/observability purposes.
                        tracing::debug!(
                            "neighbor resolution failed for {lookup_addr}; removing entry"
                        );
                        let _: NeighborState<_, _, _> = entry.remove();
                        None
                    }
                }
                NeighborState::Dynamic(DynamicNeighborState::Probe(probe)) => {
                    assert_eq!(event, NudEvent::RetransmitUnicastProbe);

                    let Probe { link_address, transmit_counter: _ } = probe;
                    let link_address = *link_address;
                    if probe.schedule_timer_if_should_retransmit(
                        sync_ctx,
                        ctx,
                        &device_id,
                        lookup_addr,
                    ) {
                        Some(TransmitProbe::Unicast(link_address))
                    } else {
                        let unreachable =
                            probe.enter_unreachable(ctx, &device_id, num_entries, last_gc);
                        *entry.get_mut() =
                            NeighborState::Dynamic(DynamicNeighborState::Unreachable(unreachable));
                        None
                    }
                }
                NeighborState::Dynamic(DynamicNeighborState::Unreachable(unreachable)) => {
                    assert_eq!(event, NudEvent::RetransmitMulticastProbe);
                    unreachable.handle_timer(sync_ctx, ctx, &device_id, lookup_addr)
                }
                NeighborState::Dynamic(DynamicNeighborState::Reachable(Reachable {
                    link_address,
                    last_confirmed_at,
                })) => {
                    assert_eq!(event, NudEvent::ReachableTime);
                    let link_address = *link_address;

                    let expiration = last_confirmed_at.add(REACHABLE_TIME.get());
                    if expiration > ctx.now() {
                        assert_eq!(
                            ctx.schedule_timer_instant(
                                expiration,
                                NudTimerId::neighbor(
                                    device_id.clone(),
                                    lookup_addr,
                                    NudEvent::ReachableTime,
                                ),
                            ),
                            None
                        );
                    } else {
                        // Per [RFC 4861 section 7.3.3]:
                        //
                        //   When ReachableTime milliseconds have passed since receipt of the last
                        //   reachability confirmation for a neighbor, the Neighbor Cache entry's
                        //   state changes from REACHABLE to STALE.
                        //
                        // [RFC 4861 section 7.3.3]: https://tools.ietf.org/html/rfc4861#section-7.3.3
                        *entry.get_mut() =
                            NeighborState::Dynamic(DynamicNeighborState::Stale(Stale {
                                link_address,
                            }));

                        // This entry is deemed discardable now that it is not in active use;
                        // schedule garbage collection for the neighbor table if we are currently
                        // over the maximum amount of entries.
                        maybe_schedule_gc(ctx, device_id, num_entries, last_gc);
                    }

                    None
                }
                NeighborState::Dynamic(DynamicNeighborState::Delay(delay)) => {
                    assert_eq!(event, NudEvent::DelayFirstProbe);

                    // Per [RFC 4861 section 7.3.3]:
                    //
                    //   If the entry is still in the DELAY state when the timer expires, the
                    //   entry's state changes to PROBE.
                    //
                    // [RFC 4861 section 7.3.3]: https://tools.ietf.org/html/rfc4861#section-7.3.3
                    let probe @ Probe { link_address, transmit_counter: _ } =
                        delay.enter_probe(sync_ctx, ctx, &device_id, lookup_addr);
                    *entry.get_mut() = NeighborState::Dynamic(DynamicNeighborState::Probe(probe));

                    Some(TransmitProbe::Unicast(link_address))
                }
                state @ (NeighborState::Static(_)
                | NeighborState::Dynamic(DynamicNeighborState::Stale(_))) => {
                    panic!("timer unexpectedly fired in state {state:?}")
                }
            }
        });

    match action {
        Some(TransmitProbe::Multicast) => {
            sync_ctx.send_neighbor_solicitation(ctx, &device_id, lookup_addr, None);
        }
        Some(TransmitProbe::Unicast(link_addr)) => {
            sync_ctx.send_neighbor_solicitation(ctx, &device_id, lookup_addr, Some(link_addr));
        }
        None => {}
    }
}

impl<
        I: Ip,
        D: LinkDevice,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: BufferNudContext<Buf<Vec<u8>>, I, D, C>,
    > NudHandler<I, D, C> for SC
{
    fn handle_neighbor_update(
        &mut self,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_address: D::Address,
        source: DynamicNeighborUpdateSource,
    ) {
        tracing::debug!("received neighbor {:?} from {}", source, neighbor);
        self.with_nud_state_mut_and_buf_ctx(
            device_id,
            |NudState { neighbors, last_gc }, sync_ctx| {
                let num_entries = neighbors.len();
                match neighbors.entry(neighbor) {
                    Entry::Vacant(e) => match source {
                        DynamicNeighborUpdateSource::Probe => {
                            // Per [RFC 4861 section 7.2.3] ("Receipt of Neighbor Solicitations"):
                            //
                            //   If an entry does not already exist, the node SHOULD create a new
                            //   one and set its reachability state to STALE as specified in Section
                            //   7.3.3.
                            //
                            // [RFC 4861 section 7.2.3]: https://tools.ietf.org/html/rfc4861#section-7.2.3
                            let _: &mut NeighborState<_, _, _> = e.insert(NeighborState::Dynamic(
                                DynamicNeighborState::Stale(Stale { link_address }),
                            ));

                            // This entry is not currently in active use; if we are currently over
                            // the maximum amount of entries, schedule garbage collection.
                            maybe_schedule_gc(ctx, device_id, neighbors.len(), last_gc);
                        }
                        // Per [RFC 4861 section 7.2.5] ("Receipt of Neighbor Advertisements"):
                        //
                        //   If no entry exists, the advertisement SHOULD be silently discarded.
                        //   There is no need to create an entry if none exists, since the
                        //   recipient has apparently not initiated any communication with the
                        //   target.
                        //
                        // [RFC 4861 section 7.2.5]: https://tools.ietf.org/html/rfc4861#section-7.2.5
                        DynamicNeighborUpdateSource::Confirmation(_) => {}
                    },
                    Entry::Occupied(e) => match e.into_mut() {
                        NeighborState::Dynamic(e) => match source {
                            DynamicNeighborUpdateSource::Probe => e.handle_probe(
                                sync_ctx,
                                ctx,
                                device_id,
                                neighbor,
                                link_address,
                                num_entries,
                                last_gc,
                            ),
                            DynamicNeighborUpdateSource::Confirmation(flags) => e
                                .handle_confirmation(
                                    sync_ctx,
                                    ctx,
                                    device_id,
                                    neighbor,
                                    link_address,
                                    flags,
                                    num_entries,
                                    last_gc,
                                ),
                        },
                        NeighborState::Static(_) => {}
                    },
                }
            },
        );
    }

    fn set_static_neighbor(
        &mut self,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_address: D::Address,
    ) {
        self.with_nud_state_mut_and_buf_ctx(
            device_id,
            |NudState { neighbors, last_gc: _ }, sync_ctx| match neighbors
                .insert(neighbor, NeighborState::Static(link_address))
            {
                Some(NeighborState::Dynamic(entry)) => {
                    entry.cancel_timer_and_complete_resolution(
                        sync_ctx,
                        ctx,
                        device_id,
                        neighbor,
                        link_address,
                    );
                }
                Some(NeighborState::Static(_)) | None => {}
            },
        );
    }

    fn delete_neighbor(
        &mut self,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    ) -> Result<(), NotFoundError> {
        self.with_nud_state_mut(device_id, |NudState { neighbors, last_gc: _ }, _config| {
            match neighbors.remove(&neighbor).ok_or(NotFoundError)? {
                NeighborState::Dynamic(mut entry) => {
                    entry.cancel_timer(ctx, device_id, neighbor);
                }
                NeighborState::Static(_) => {}
            }
            Ok(())
        })
    }

    fn flush(&mut self, ctx: &mut C, device_id: &Self::DeviceId) {
        self.with_nud_state_mut(device_id, |NudState { neighbors, last_gc: _ }, _config| {
            neighbors.retain(|neighbor, state| {
                match state {
                    NeighborState::Dynamic(entry) => {
                        entry.cancel_timer(ctx, device_id, *neighbor);

                        // Only flush dynamic entries.
                        false
                    }
                    NeighborState::Static(_) => true,
                }
            });
        });
    }

    fn resolve_link_addr(
        &mut self,
        ctx: &mut C,
        device: &SC::DeviceId,
        dst: &SpecifiedAddr<I::Addr>,
    ) -> LinkResolutionResult<D::Address, <C::Notifier as LinkResolutionNotifier<D>>::Observer>
    {
        let (result, do_multicast_solicit) =
            self.with_nud_state_mut(device, |NudState { neighbors, last_gc: _ }, sync_ctx| {
                match neighbors.entry(*dst) {
                    Entry::Vacant(entry) => {
                        // Initiate link resolution.
                        let (notifier, observer) = C::Notifier::new();
                        let _: &mut NeighborState<_, _, _> = entry.insert(NeighborState::Dynamic(
                            DynamicNeighborState::Incomplete(Incomplete::new_with_notifier(
                                sync_ctx, ctx, device, *dst, notifier,
                            )),
                        ));
                        (LinkResolutionResult::Pending(observer), true)
                    }
                    Entry::Occupied(e) => match e.into_mut() {
                        NeighborState::Static(link_address) => {
                            (LinkResolutionResult::Resolved(*link_address), false)
                        }
                        NeighborState::Dynamic(e) => {
                            e.resolve_link_addr(sync_ctx, ctx, device, *dst)
                        }
                    },
                }
            });

        if do_multicast_solicit {
            self.send_neighbor_solicitation(ctx, &device, *dst, /* multicast */ None);
        }

        result
    }
}

impl<
        B: BufferMut,
        I: Ip,
        D: LinkDevice,
        C: NonSyncContext<I, D, SC::DeviceId>,
        SC: BufferNudContext<B, I, D, C>,
    > BufferNudHandler<B, I, D, C> for SC
{
    fn send_ip_packet_to_neighbor<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        lookup_addr: SpecifiedAddr<I::Addr>,
        body: S,
    ) -> Result<(), S> {
        let do_multicast_solicit = self.with_nud_state_mut_and_buf_ctx(
            device_id,
            |NudState { neighbors, last_gc: _ }, sync_ctx| {
                match neighbors.entry(lookup_addr) {
                    Entry::Vacant(e) => {
                        let _: &mut NeighborState<_, _, _> = e.insert(NeighborState::Dynamic(
                            DynamicNeighborState::Incomplete(Incomplete::new_with_pending_frame(
                                sync_ctx,
                                ctx,
                                device_id,
                                lookup_addr,
                                body.serialize_vec_outer()
                                    .map_err(|(_err, s)| s)?
                                    .map_a(|b| Buf::new(b.as_ref().to_vec(), ..))
                                    .into_inner(),
                            )),
                        ));

                        Ok(true)
                    }
                    Entry::Occupied(e) => {
                        match e.into_mut() {
                            NeighborState::Static(link_address) => {
                                // Send the IP packet while holding the NUD lock to prevent a
                                // potential ordering violation.
                                //
                                // If we drop the NUD lock before sending out this packet, another
                                // thread could take the NUD lock and send a packet *before* this
                                // packet is sent out, resulting in out-of-order transmission to the
                                // device.
                                sync_ctx.send_ip_packet_to_neighbor_link_addr(
                                    ctx,
                                    *link_address,
                                    body,
                                )?;

                                Ok(false)
                            }
                            NeighborState::Dynamic(e) => {
                                let do_multicast_solicit = e.handle_packet_queued_to_send(
                                    sync_ctx,
                                    ctx,
                                    device_id,
                                    lookup_addr,
                                    body,
                                )?;

                                Ok(do_multicast_solicit)
                            }
                        }
                    }
                }
            },
        )?;

        if do_multicast_solicit {
            self.send_neighbor_solicitation(
                ctx,
                &device_id,
                lookup_addr,
                /* multicast */ None,
            );
        }

        Ok(())
    }
}

/// Confirm upper-layer forward reachability to the specified neighbor through
/// the specified device.
pub(crate) fn confirm_reachable<I, D, SC, C>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    neighbor: SpecifiedAddr<I::Addr>,
) where
    I: Ip,
    D: LinkDevice,
    C: NonSyncContext<I, D, SC::DeviceId>,
    SC: BufferNudContext<Buf<Vec<u8>>, I, D, C>,
{
    sync_ctx.with_nud_state_mut_and_buf_ctx(
        device_id,
        |NudState { neighbors, last_gc: _ }, sync_ctx| {
            match neighbors.entry(neighbor) {
                Entry::Vacant(_) => {
                    tracing::error!(
                        "got an upper-layer confirmation for non-existent neighbor entry {}",
                        neighbor
                    );
                }
                Entry::Occupied(e) => match e.into_mut() {
                    NeighborState::Static(_) => {}
                    NeighborState::Dynamic(e) => {
                        // Per [RFC 4861 section 7.3.3]:
                        //
                        //   When a reachability confirmation is received (either through upper-
                        //   layer advice or a solicited Neighbor Advertisement), an entry's state
                        //   changes to REACHABLE.  The one exception is that upper-layer advice has
                        //   no effect on entries in the INCOMPLETE state (e.g., for which no link-
                        //   layer address is cached).
                        //
                        // [RFC 4861 section 7.3.3]: https://tools.ietf.org/html/rfc4861#section-7.3.3
                        let link_address = match e {
                            DynamicNeighborState::Incomplete(_) => return,
                            DynamicNeighborState::Reachable(Reachable {
                                link_address,
                                last_confirmed_at: _,
                            })
                            | DynamicNeighborState::Stale(Stale { link_address })
                            | DynamicNeighborState::Delay(Delay { link_address })
                            | DynamicNeighborState::Probe(Probe {
                                link_address,
                                transmit_counter: _,
                            })
                            | DynamicNeighborState::Unreachable(Unreachable {
                                link_address,
                                mode: _,
                            }) => *link_address,
                        };
                        e.enter_reachable(sync_ctx, ctx, device_id, neighbor, link_address);
                    }
                },
            }
        },
    );
}

fn maybe_schedule_gc<I, D, C, DeviceId: Clone>(
    ctx: &mut C,
    device_id: &DeviceId,
    num_entries: usize,
    last_gc: &mut Option<C::Instant>,
) where
    I: Ip,
    D: LinkDevice,
    C: NonSyncContext<I, D, DeviceId>,
{
    if num_entries > MAX_ENTRIES
        && ctx.scheduled_instant(NudTimerId::garbage_collection(device_id.clone())).is_none()
    {
        let delay = if let Some(last_gc) = last_gc {
            let next_gc = last_gc.add(MIN_GARBAGE_COLLECTION_INTERVAL.get());
            next_gc.saturating_duration_since(ctx.now())
        } else {
            core::time::Duration::ZERO
        };
        // NB: we can be sure that this assertion will always hold because this function
        // is the only place that schedules the GC timer, and it takes a &mut to
        // `last_gc`, which is protected by the NUD lock. No two threads can acquire
        // that lock at the same time, which means that the following operations will
        // always occur atomically:
        //
        //  (1) checking if the GC timer is scheduled
        //  (2) scheduling a timer if it is not
        assert_eq!(
            ctx.schedule_timer(delay, NudTimerId::garbage_collection(device_id.clone())),
            None
        );
    }
}

/// Performs a linear scan of the neighbor table, discarding enough entries to
/// bring the total size under `MAX_ENTRIES` if possible.
///
/// Static neighbor entries are never discarded, nor are any entries that are
/// considered to be in use, which is defined as an entry in REACHABLE,
/// INCOMPLETE, DELAY, or PROBE. In other words, the only entries eligible to be
/// discarded are those in STALE or UNREACHABLE. This is reasonable because all
/// other states represent entries to which we have either recently sent packets
/// (REACHABLE, DELAY, PROBE), or which we are actively trying to resolve and
/// for which we have recently queued outgoing packets (INCOMPLETE).
fn collect_garbage<I, D, SC, C>(sync_ctx: &mut SC, ctx: &mut C, device_id: &SC::DeviceId)
where
    I: Ip,
    D: LinkDevice,
    C: NonSyncContext<I, D, SC::DeviceId>,
    SC: NudContext<I, D, C>,
{
    sync_ctx.with_nud_state_mut(device_id, |NudState { neighbors, last_gc }, _| {
        let max_to_remove = neighbors.len().saturating_sub(MAX_ENTRIES);
        if max_to_remove == 0 {
            return;
        }

        *last_gc = Some(ctx.now());

        // Define an ordering by priority for garbage collection, such that lower
        // numbers correspond to higher usefulness and therefore lower likelihood of
        // being discarded.
        //
        // TODO(https://fxbug.dev/124960): once neighbor entries hold a timestamp
        // tracking when they were last updated, consider using this timestamp to break
        // ties between entries in the same state, so that we discard less recently
        // updated entries before more recently updated ones.
        fn gc_priority<D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>>(
            state: &DynamicNeighborState<D, I, N>,
        ) -> usize {
            match state {
                DynamicNeighborState::Incomplete(_)
                | DynamicNeighborState::Reachable(_)
                | DynamicNeighborState::Delay(_)
                | DynamicNeighborState::Probe(_) => unreachable!(
                    "the netstack should only ever discard STALE or UNREACHABLE entries; \
                        found {:?}",
                    state,
                ),
                DynamicNeighborState::Stale(_) => 0,
                DynamicNeighborState::Unreachable(Unreachable {
                    link_address: _,
                    mode: UnreachableMode::Backoff { probes_sent: _, packet_sent: _ },
                }) => 1,
                DynamicNeighborState::Unreachable(Unreachable {
                    link_address: _,
                    mode: UnreachableMode::WaitingForPacketSend,
                }) => 2,
            }
        }

        struct SortEntry<'a, K: Eq, D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>> {
            key: K,
            state: &'a mut DynamicNeighborState<D, I, N>,
        }

        impl<K: Eq, D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>> PartialEq
            for SortEntry<'_, K, D, I, N>
        {
            fn eq(&self, other: &Self) -> bool {
                self.key == other.key && gc_priority(self.state) == gc_priority(other.state)
            }
        }
        impl<K: Eq, D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>> Eq
            for SortEntry<'_, K, D, I, N>
        {
        }
        impl<K: Eq, D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>> Ord
            for SortEntry<'_, K, D, I, N>
        {
            fn cmp(&self, other: &Self) -> core::cmp::Ordering {
                // Sort in reverse order so `BinaryHeap` will function as a min-heap rather than
                // a max-heap. This means it will maintain the minimum (i.e. most useful) entry
                // at the top of the heap.
                gc_priority(self.state).cmp(&gc_priority(other.state)).reverse()
            }
        }
        impl<K: Eq, D: LinkDevice, I: Instant, N: LinkResolutionNotifier<D>> PartialOrd
            for SortEntry<'_, K, D, I, N>
        {
            fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
                Some(self.cmp(&other))
            }
        }

        let mut entries_to_remove = BinaryHeap::with_capacity(max_to_remove);
        for (ip, neighbor) in neighbors.iter_mut() {
            match neighbor {
                NeighborState::Static(_) => {
                    // Don't discard static entries.
                    continue;
                }
                NeighborState::Dynamic(state) => {
                    match state {
                        DynamicNeighborState::Incomplete(_)
                        | DynamicNeighborState::Reachable(_)
                        | DynamicNeighborState::Delay(_)
                        | DynamicNeighborState::Probe(_) => {
                            // Don't discard in-use entries.
                            continue;
                        }
                        DynamicNeighborState::Stale(_) | DynamicNeighborState::Unreachable(_) => {
                            // Unconditionally insert the first `max_to_remove` entries.
                            if entries_to_remove.len() < max_to_remove {
                                entries_to_remove.push(SortEntry { key: ip, state });
                                continue;
                            }
                            // Check if this neighbor is greater than (i.e. less useful than) the
                            // minimum (i.e. most useful) entry that is currently set to be removed.
                            // If it is, replace that entry with this one.
                            let minimum = entries_to_remove
                                .peek()
                                .expect("heap should have at least 1 entry");
                            let candidate = SortEntry { key: ip, state };
                            if &candidate > minimum {
                                let _: SortEntry<'_, _, _, _, _> = entries_to_remove.pop().unwrap();
                                entries_to_remove.push(candidate);
                            }
                        }
                    }
                }
            }
        }

        let entries_to_remove = entries_to_remove
            .into_iter()
            .map(|SortEntry { key: neighbor, state }| {
                state.cancel_timer(ctx, device_id, *neighbor);
                *neighbor
            })
            .collect::<Vec<_>>();

        for neighbor in entries_to_remove {
            assert_matches!(neighbors.remove(&neighbor), Some(_));
        }
    });
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};
    use core::num::{NonZeroU16, NonZeroUsize};

    use ip_test_macro::ip_test;
    use lock_order::Locked;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::{
        ip::{AddrSubnet, IpAddress as _, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
        UnicastAddr, Witness as _,
    };
    use packet::{Buf, InnerPacketBuilder as _, Serializer as _};
    use packet_formats::{
        ethernet::{EtherType, EthernetFrameLengthCheck},
        icmp::{
            ndp::{
                options::NdpOptionBuilder, NeighborAdvertisement, NeighborSolicitation,
                OptionSequenceBuilder, RouterAdvertisement,
            },
            IcmpPacketBuilder, IcmpUnusedCode,
        },
        ip::Ipv6Proto,
        ipv6::Ipv6PacketBuilder,
        testutil::{parse_ethernet_frame, parse_icmp_packet_in_ip_packet_in_ethernet_frame},
    };
    use test_case::test_case;

    use super::*;
    use crate::{
        context::{
            testutil::{
                handle_timer_helper_with_sc_ref_mut, FakeCtxWithSyncCtx, FakeInstant,
                FakeLinkResolutionNotifier, FakeNetwork, FakeNetworkLinks, FakeNonSyncCtx,
                FakeSyncCtx, FakeTimerCtxExt as _, WrappedFakeSyncCtx,
            },
            InstantContext, SendFrameContext as _,
        },
        device::{
            ethernet::EthernetLinkDevice,
            link::testutil::{FakeLinkAddress, FakeLinkDevice, FakeLinkDeviceId},
            ndp::testutil::{neighbor_advertisement_ip_packet, neighbor_solicitation_ip_packet},
            testutil::FakeWeakDeviceId,
            update_ipv6_configuration, EthernetDeviceId, EthernetWeakDeviceId, WeakDeviceId,
        },
        ip::{
            device::{
                slaac::SlaacConfiguration,
                testutil::UpdateIpDeviceConfigurationAndFlagsTestIpExt as _,
                Ipv6DeviceConfigurationUpdate,
            },
            icmp::REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
            receive_ip_packet, FrameDestination,
        },
        testutil::{
            self, FakeEventDispatcherConfig, TestIpExt as _, DEFAULT_INTERFACE_METRIC,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        transport::tcp,
        SyncCtx,
    };

    struct FakeNudContext<I: Ip, D: LinkDevice> {
        nud: NudState<I, D, FakeInstant, FakeLinkResolutionNotifier<D>>,
    }

    struct FakeConfigContext {
        retrans_timer: NonZeroDuration,
    }

    type FakeCtxImpl<I> = WrappedFakeSyncCtx<
        FakeNudContext<I, FakeLinkDevice>,
        FakeConfigContext,
        FakeNudMessageMeta<I>,
        FakeLinkDeviceId,
    >;

    type FakeInnerCtxImpl<I> =
        FakeSyncCtx<FakeConfigContext, FakeNudMessageMeta<I>, FakeLinkDeviceId>;

    #[derive(Debug, PartialEq, Eq)]
    enum FakeNudMessageMeta<I: Ip> {
        NeighborSolicitation {
            lookup_addr: SpecifiedAddr<I::Addr>,
            remote_link_addr: Option<FakeLinkAddress>,
        },
        IpFrame {
            dst_link_address: FakeLinkAddress,
        },
    }

    type FakeNonSyncCtxImpl<I> =
        FakeNonSyncCtx<NudTimerId<I, FakeLinkDevice, FakeLinkDeviceId>, (), ()>;

    impl<I: Ip> FakeCtxImpl<I> {
        fn new() -> Self {
            Self::with_inner_and_outer_state(
                FakeConfigContext { retrans_timer: ONE_SECOND },
                FakeNudContext { nud: NudState::default() },
            )
        }
    }

    impl<I: Ip> DeviceIdContext<FakeLinkDevice> for FakeCtxImpl<I> {
        type DeviceId = FakeLinkDeviceId;
        type WeakDeviceId = FakeWeakDeviceId<FakeLinkDeviceId>;

        fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
            FakeWeakDeviceId(device_id.clone())
        }

        fn upgrade_weak_device_id(
            &self,
            weak_device_id: &Self::WeakDeviceId,
        ) -> Option<Self::DeviceId> {
            let FakeWeakDeviceId(id) = weak_device_id;
            Some(id.clone())
        }
    }

    impl<I: Ip> NudContext<I, FakeLinkDevice, FakeNonSyncCtxImpl<I>> for FakeCtxImpl<I> {
        type ConfigCtx<'a> = FakeConfigContext;

        fn with_nud_state_mut<
            O,
            F: FnOnce(
                &mut NudState<
                    I,
                    FakeLinkDevice,
                    FakeInstant,
                    FakeLinkResolutionNotifier<FakeLinkDevice>,
                >,
                &mut Self::ConfigCtx<'_>,
            ) -> O,
        >(
            &mut self,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            cb(&mut self.outer.nud, self.inner.get_mut())
        }

        fn send_neighbor_solicitation(
            &mut self,
            ctx: &mut FakeNonSyncCtxImpl<I>,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            lookup_addr: SpecifiedAddr<I::Addr>,
            remote_link_addr: Option<FakeLinkAddress>,
        ) {
            self.inner
                .send_frame(
                    ctx,
                    FakeNudMessageMeta::NeighborSolicitation { lookup_addr, remote_link_addr },
                    Buf::new(Vec::new(), ..),
                )
                .unwrap()
        }
    }

    impl<I: Ip> NudConfigContext<I> for FakeConfigContext {
        fn retransmit_timeout(&mut self) -> NonZeroDuration {
            self.retrans_timer
        }
    }

    impl<B: BufferMut, I: Ip> BufferNudContext<B, I, FakeLinkDevice, FakeNonSyncCtxImpl<I>>
        for FakeCtxImpl<I>
    {
        type BufferSenderCtx<'a> = FakeInnerCtxImpl<I>;

        fn with_nud_state_mut_and_buf_ctx<
            O,
            F: FnOnce(
                &mut NudState<
                    I,
                    FakeLinkDevice,
                    FakeInstant,
                    FakeLinkResolutionNotifier<FakeLinkDevice>,
                >,
                &mut Self::BufferSenderCtx<'_>,
            ) -> O,
        >(
            &mut self,
            _device_id: &Self::DeviceId,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(&mut outer.nud, inner)
        }
    }

    impl<B: BufferMut, I: Ip> BufferNudSenderContext<B, I, FakeLinkDevice, FakeNonSyncCtxImpl<I>>
        for FakeInnerCtxImpl<I>
    {
        fn send_ip_packet_to_neighbor_link_addr<S: Serializer<Buffer = B>>(
            &mut self,
            ctx: &mut FakeNonSyncCtxImpl<I>,
            dst_link_address: FakeLinkAddress,
            body: S,
        ) -> Result<(), S> {
            self.send_frame(ctx, FakeNudMessageMeta::IpFrame { dst_link_address }, body)
        }
    }

    impl<I: Ip> NudConfigContext<I> for FakeInnerCtxImpl<I> {
        fn retransmit_timeout(&mut self) -> NonZeroDuration {
            <FakeConfigContext as NudConfigContext<I>>::retransmit_timeout(self.get_mut())
        }
    }

    const ONE_SECOND: NonZeroDuration =
        const_unwrap::const_unwrap_option(NonZeroDuration::from_secs(1));

    #[track_caller]
    fn check_lookup_has<I: Ip>(
        sync_ctx: &mut FakeCtxImpl<I>,
        ctx: &mut FakeNonSyncCtxImpl<I>,
        lookup_addr: SpecifiedAddr<I::Addr>,
        expected_link_addr: FakeLinkAddress,
    ) {
        let entry = assert_matches!(
            sync_ctx.outer.nud.neighbors.get(&lookup_addr),
            Some(entry @ (
                NeighborState::Dynamic(
                    DynamicNeighborState::Reachable (Reachable { link_address, last_confirmed_at: _ })
                    | DynamicNeighborState::Stale (Stale { link_address })
                    | DynamicNeighborState::Delay (Delay { link_address })
                    | DynamicNeighborState::Probe (Probe { link_address, transmit_counter: _ })
                    | DynamicNeighborState::Unreachable (Unreachable { link_address, mode: _ })
                )
                | NeighborState::Static(link_address)
            )) => {
                assert_eq!(link_address, &expected_link_addr);
                entry
            }
        );
        match entry {
            NeighborState::Dynamic(DynamicNeighborState::Incomplete { .. }) => {
                unreachable!("entry must be static, REACHABLE, or STALE")
            }
            NeighborState::Dynamic(DynamicNeighborState::Reachable { .. }) => {
                ctx.timer_ctx().assert_timers_installed([(
                    NudTimerId::neighbor(FakeLinkDeviceId, lookup_addr, NudEvent::ReachableTime),
                    ctx.now() + REACHABLE_TIME.get(),
                )])
            }
            NeighborState::Dynamic(DynamicNeighborState::Delay { .. }) => {
                ctx.timer_ctx().assert_timers_installed([(
                    NudTimerId::neighbor(FakeLinkDeviceId, lookup_addr, NudEvent::DelayFirstProbe),
                    ctx.now() + DELAY_FIRST_PROBE_TIME.get(),
                )])
            }
            NeighborState::Dynamic(DynamicNeighborState::Probe { .. }) => {
                ctx.timer_ctx().assert_timers_installed([(
                    NudTimerId::neighbor(
                        FakeLinkDeviceId,
                        lookup_addr,
                        NudEvent::RetransmitUnicastProbe,
                    ),
                    ctx.now() + sync_ctx.inner.get_ref().retrans_timer.get(),
                )])
            }
            NeighborState::Dynamic(DynamicNeighborState::Unreachable(Unreachable {
                link_address: _,
                mode,
            })) => {
                let instant = match mode {
                    UnreachableMode::WaitingForPacketSend => None,
                    mode @ UnreachableMode::Backoff { .. } => {
                        let duration =
                            mode.next_backoff_retransmit_timeout::<I, _>(sync_ctx.inner.get_mut());
                        Some(ctx.now() + duration.get())
                    }
                };
                if let Some(instant) = instant {
                    ctx.timer_ctx().assert_timers_installed([(
                        NudTimerId::neighbor(
                            FakeLinkDeviceId,
                            lookup_addr,
                            NudEvent::RetransmitUnicastProbe,
                        ),
                        instant,
                    )])
                }
            }
            NeighborState::Dynamic(DynamicNeighborState::Stale { .. })
            | NeighborState::Static(_) => ctx.timer_ctx().assert_no_timers_installed(),
        }
    }

    trait TestIpExt: Ip {
        const LOOKUP_ADDR1: SpecifiedAddr<Self::Addr>;
        const LOOKUP_ADDR2: SpecifiedAddr<Self::Addr>;
        const LOOKUP_ADDR3: SpecifiedAddr<Self::Addr>;
    }

    impl TestIpExt for Ipv4 {
        // Safe because the address is non-zero.
        const LOOKUP_ADDR1: SpecifiedAddr<Ipv4Addr> =
            unsafe { SpecifiedAddr::new_unchecked(net_ip_v4!("192.168.0.1")) };
        const LOOKUP_ADDR2: SpecifiedAddr<Ipv4Addr> =
            unsafe { SpecifiedAddr::new_unchecked(net_ip_v4!("192.168.0.2")) };
        const LOOKUP_ADDR3: SpecifiedAddr<Ipv4Addr> =
            unsafe { SpecifiedAddr::new_unchecked(net_ip_v4!("192.168.0.3")) };
    }

    impl TestIpExt for Ipv6 {
        // Safe because the address is non-zero.
        const LOOKUP_ADDR1: SpecifiedAddr<Ipv6Addr> =
            unsafe { SpecifiedAddr::new_unchecked(net_ip_v6!("fe80::1")) };
        const LOOKUP_ADDR2: SpecifiedAddr<Ipv6Addr> =
            unsafe { SpecifiedAddr::new_unchecked(net_ip_v6!("fe80::2")) };
        const LOOKUP_ADDR3: SpecifiedAddr<Ipv6Addr> =
            unsafe { SpecifiedAddr::new_unchecked(net_ip_v6!("fe80::3")) };
    }

    const LINK_ADDR1: FakeLinkAddress = FakeLinkAddress([1]);
    const LINK_ADDR2: FakeLinkAddress = FakeLinkAddress([2]);
    const LINK_ADDR3: FakeLinkAddress = FakeLinkAddress([3]);

    fn queue_ip_packet_to_unresolved_neighbor<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        neighbor: SpecifiedAddr<I::Addr>,
        pending_frames: &mut VecDeque<Buf<Vec<u8>>>,
        body: u8,
    ) {
        let body = [body];
        assert_eq!(
            BufferNudHandler::send_ip_packet_to_neighbor(
                sync_ctx,
                non_sync_ctx,
                &FakeLinkDeviceId,
                neighbor,
                Buf::new(body, ..),
            ),
            Ok(())
        );

        pending_frames.push_back(Buf::new(body.to_vec(), ..));

        assert_neighbor_state_with_ip(
            sync_ctx,
            neighbor,
            DynamicNeighborState::Incomplete(Incomplete {
                transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT - 1),
                pending_frames: pending_frames.clone(),
                notifiers: Vec::new(),
                _marker: PhantomData,
            }),
        );
        non_sync_ctx.timer_ctx().assert_some_timers_installed([(
            NudTimerId::neighbor(FakeLinkDeviceId, neighbor, NudEvent::RetransmitMulticastProbe),
            non_sync_ctx.now() + ONE_SECOND.get(),
        )]);
    }

    fn init_incomplete_neighbor_with_ip<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        ip_address: SpecifiedAddr<I::Addr>,
        take_probe: bool,
    ) -> VecDeque<Buf<Vec<u8>>> {
        let mut pending_frames = VecDeque::new();
        queue_ip_packet_to_unresolved_neighbor(
            sync_ctx,
            non_sync_ctx,
            ip_address,
            &mut pending_frames,
            1,
        );
        assert_neighbor_state_with_ip(
            sync_ctx,
            ip_address,
            DynamicNeighborState::Incomplete(Incomplete {
                transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT - 1),
                pending_frames: pending_frames.clone(),
                notifiers: Vec::new(),
                _marker: PhantomData,
            }),
        );
        if take_probe {
            assert_neighbor_probe_sent_for_ip(sync_ctx, ip_address, None);
        }
        pending_frames
    }

    fn init_incomplete_neighbor<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        take_probe: bool,
    ) -> VecDeque<Buf<Vec<u8>>> {
        init_incomplete_neighbor_with_ip(sync_ctx, non_sync_ctx, I::LOOKUP_ADDR1, take_probe)
    }

    fn init_stale_neighbor_with_ip<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        ip_address: SpecifiedAddr<I::Addr>,
        link_address: FakeLinkAddress,
    ) {
        NudHandler::handle_neighbor_update(
            sync_ctx,
            non_sync_ctx,
            &FakeLinkDeviceId,
            ip_address,
            link_address,
            DynamicNeighborUpdateSource::Probe,
        );
        assert_neighbor_state_with_ip(
            sync_ctx,
            ip_address,
            DynamicNeighborState::Stale(Stale { link_address }),
        );
    }

    fn init_stale_neighbor<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        link_address: FakeLinkAddress,
    ) {
        init_stale_neighbor_with_ip(sync_ctx, non_sync_ctx, I::LOOKUP_ADDR1, link_address);
    }

    fn init_reachable_neighbor_with_ip<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        ip_address: SpecifiedAddr<I::Addr>,
        link_address: FakeLinkAddress,
    ) {
        let queued_frame =
            init_incomplete_neighbor_with_ip(sync_ctx, non_sync_ctx, ip_address, true);
        NudHandler::handle_neighbor_update(
            sync_ctx,
            non_sync_ctx,
            &FakeLinkDeviceId,
            ip_address,
            link_address,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag: true,
                override_flag: false,
            }),
        );
        assert_neighbor_state_with_ip(
            sync_ctx,
            ip_address,
            DynamicNeighborState::Reachable(Reachable {
                link_address,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
        assert_pending_frame_sent(sync_ctx, queued_frame, link_address);
    }

    fn init_reachable_neighbor<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        link_address: FakeLinkAddress,
    ) {
        init_reachable_neighbor_with_ip(sync_ctx, non_sync_ctx, I::LOOKUP_ADDR1, link_address);
    }

    fn init_delay_neighbor_with_ip<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        ip_address: SpecifiedAddr<I::Addr>,
        link_address: FakeLinkAddress,
    ) {
        init_stale_neighbor_with_ip(sync_ctx, non_sync_ctx, ip_address, link_address);
        assert_eq!(
            BufferNudHandler::send_ip_packet_to_neighbor(
                sync_ctx,
                non_sync_ctx,
                &FakeLinkDeviceId,
                ip_address,
                Buf::new([1], ..),
            ),
            Ok(())
        );
        assert_neighbor_state_with_ip(
            sync_ctx,
            ip_address,
            DynamicNeighborState::Delay(Delay { link_address }),
        );
        assert_eq!(
            sync_ctx.inner.take_frames(),
            vec![(FakeNudMessageMeta::IpFrame { dst_link_address: LINK_ADDR1 }, vec![1])],
        );
    }

    fn init_delay_neighbor<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        link_address: FakeLinkAddress,
    ) {
        init_delay_neighbor_with_ip(sync_ctx, non_sync_ctx, I::LOOKUP_ADDR1, link_address);
    }

    fn init_probe_neighbor_with_ip<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        ip_address: SpecifiedAddr<I::Addr>,
        link_address: FakeLinkAddress,
        take_probe: bool,
    ) {
        init_delay_neighbor_with_ip(sync_ctx, non_sync_ctx, ip_address, link_address);
        assert_eq!(
            non_sync_ctx.trigger_timers_for(
                DELAY_FIRST_PROBE_TIME.into(),
                handle_timer_helper_with_sc_ref_mut(sync_ctx, TimerHandler::handle_timer),
            ),
            [NudTimerId::neighbor(FakeLinkDeviceId, ip_address, NudEvent::DelayFirstProbe)]
        );
        assert_neighbor_state_with_ip(
            sync_ctx,
            ip_address,
            DynamicNeighborState::Probe(Probe {
                link_address,
                transmit_counter: NonZeroU8::new(MAX_UNICAST_SOLICIT - 1),
            }),
        );
        if take_probe {
            assert_neighbor_probe_sent_for_ip(sync_ctx, ip_address, Some(LINK_ADDR1));
        }
    }

    fn init_probe_neighbor<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        link_address: FakeLinkAddress,
        take_probe: bool,
    ) {
        init_probe_neighbor_with_ip(
            sync_ctx,
            non_sync_ctx,
            I::LOOKUP_ADDR1,
            link_address,
            take_probe,
        );
    }

    fn init_unreachable_neighbor_with_ip<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        ip_address: SpecifiedAddr<I::Addr>,
        link_address: FakeLinkAddress,
    ) {
        init_probe_neighbor_with_ip(sync_ctx, non_sync_ctx, ip_address, link_address, false);
        let retransmit_timeout = sync_ctx.inner.retransmit_timeout();
        for _ in 0..MAX_UNICAST_SOLICIT {
            assert_neighbor_probe_sent_for_ip(sync_ctx, ip_address, Some(LINK_ADDR1));
            assert_eq!(
                non_sync_ctx.trigger_timers_for(
                    retransmit_timeout.into(),
                    handle_timer_helper_with_sc_ref_mut(sync_ctx, TimerHandler::handle_timer),
                ),
                [NudTimerId::neighbor(
                    FakeLinkDeviceId,
                    ip_address,
                    NudEvent::RetransmitUnicastProbe
                )]
            );
        }
        assert_neighbor_state_with_ip(
            sync_ctx,
            ip_address,
            DynamicNeighborState::Unreachable(Unreachable {
                link_address,
                mode: UnreachableMode::WaitingForPacketSend,
            }),
        );
    }

    fn init_unreachable_neighbor<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        link_address: FakeLinkAddress,
    ) {
        init_unreachable_neighbor_with_ip(sync_ctx, non_sync_ctx, I::LOOKUP_ADDR1, link_address);
    }

    #[derive(Debug, Clone, Copy)]
    enum InitialState {
        Incomplete,
        Stale,
        Reachable,
        Delay,
        Probe,
        Unreachable,
    }

    fn init_neighbor_in_state<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        non_sync_ctx: &mut FakeNonSyncCtxImpl<I>,
        state: InitialState,
    ) -> DynamicNeighborState<FakeLinkDevice, FakeInstant, FakeLinkResolutionNotifier<FakeLinkDevice>>
    {
        match state {
            InitialState::Incomplete => {
                let _: VecDeque<Buf<Vec<u8>>> =
                    init_incomplete_neighbor(sync_ctx, non_sync_ctx, true);
            }
            InitialState::Reachable => {
                init_reachable_neighbor(sync_ctx, non_sync_ctx, LINK_ADDR1);
            }
            InitialState::Stale => {
                init_stale_neighbor(sync_ctx, non_sync_ctx, LINK_ADDR1);
            }
            InitialState::Delay => {
                init_delay_neighbor(sync_ctx, non_sync_ctx, LINK_ADDR1);
            }
            InitialState::Probe => {
                init_probe_neighbor(sync_ctx, non_sync_ctx, LINK_ADDR1, true);
            }
            InitialState::Unreachable => {
                init_unreachable_neighbor(sync_ctx, non_sync_ctx, LINK_ADDR1);
            }
        }
        assert_matches!(sync_ctx.outer.nud.neighbors.get(&I::LOOKUP_ADDR1),
            Some(NeighborState::Dynamic(state)) => state.clone()
        )
    }

    #[track_caller]
    fn assert_neighbor_state<I: Ip + TestIpExt>(
        sync_ctx: &FakeCtxImpl<I>,
        state: DynamicNeighborState<
            FakeLinkDevice,
            FakeInstant,
            FakeLinkResolutionNotifier<FakeLinkDevice>,
        >,
    ) {
        let FakeNudContext { nud } = &sync_ctx.outer;
        assert_eq!(nud.neighbors.get(&I::LOOKUP_ADDR1), Some(&NeighborState::Dynamic(state)));
    }

    #[track_caller]
    fn assert_neighbor_state_with_ip<I: Ip + TestIpExt>(
        sync_ctx: &FakeCtxImpl<I>,
        neighbor: SpecifiedAddr<I::Addr>,
        state: DynamicNeighborState<
            FakeLinkDevice,
            FakeInstant,
            FakeLinkResolutionNotifier<FakeLinkDevice>,
        >,
    ) {
        let FakeNudContext { nud } = &sync_ctx.outer;
        assert_eq!(nud.neighbors.get(&neighbor), Some(&NeighborState::Dynamic(state)));
    }

    #[track_caller]
    fn assert_pending_frame_sent<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        pending_frames: VecDeque<Buf<Vec<u8>>>,
        link_address: FakeLinkAddress,
    ) {
        assert_eq!(
            sync_ctx.inner.take_frames(),
            pending_frames
                .into_iter()
                .map(|f| (
                    FakeNudMessageMeta::IpFrame { dst_link_address: link_address },
                    f.as_ref().to_vec(),
                ))
                .collect::<Vec<_>>()
        );
    }

    #[track_caller]
    fn assert_neighbor_probe_sent_for_ip<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        ip_address: SpecifiedAddr<I::Addr>,
        link_address: Option<FakeLinkAddress>,
    ) {
        assert_eq!(
            sync_ctx.inner.take_frames(),
            [(
                FakeNudMessageMeta::NeighborSolicitation {
                    lookup_addr: ip_address,
                    remote_link_addr: link_address,
                },
                Vec::new()
            )]
        );
    }

    #[track_caller]
    fn assert_neighbor_probe_sent<I: Ip + TestIpExt>(
        sync_ctx: &mut FakeCtxImpl<I>,
        link_address: Option<FakeLinkAddress>,
    ) {
        assert_neighbor_probe_sent_for_ip(sync_ctx, I::LOOKUP_ADDR1, link_address);
    }

    #[ip_test]
    fn incomplete_to_stale_on_probe<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor in INCOMPLETE.
        let queued_frame = init_incomplete_neighbor(&mut sync_ctx, &mut non_sync_ctx, true);

        // Handle an incoming probe from that neighbor.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
            DynamicNeighborUpdateSource::Probe,
        );

        // Neighbor should now be in STALE, per RFC 4861 section 7.2.3.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Stale(Stale { link_address: LINK_ADDR1 }),
        );
        assert_pending_frame_sent(&mut sync_ctx, queued_frame, LINK_ADDR1);
    }

    #[ip_test]
    #[test_case(true, true; "solicited override")]
    #[test_case(true, false; "solicited non-override")]
    #[test_case(false, true; "unsolicited override")]
    #[test_case(false, false; "unsolicited non-override")]
    fn incomplete_on_confirmation<I: Ip + TestIpExt>(solicited_flag: bool, override_flag: bool) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor in INCOMPLETE.
        let queued_frame = init_incomplete_neighbor(&mut sync_ctx, &mut non_sync_ctx, true);

        // Handle an incoming confirmation from that neighbor.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag,
                override_flag,
            }),
        );

        let expected_state = if solicited_flag {
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now(),
            })
        } else {
            DynamicNeighborState::Stale(Stale { link_address: LINK_ADDR1 })
        };
        assert_neighbor_state(&sync_ctx, expected_state);
        assert_pending_frame_sent(&mut sync_ctx, queued_frame, LINK_ADDR1);
    }

    #[ip_test]
    fn reachable_to_stale_on_timeout<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor in REACHABLE.
        init_reachable_neighbor(&mut sync_ctx, &mut non_sync_ctx, LINK_ADDR1);

        // After REACHABLE_TIME, neighbor should transition to STALE.
        assert_eq!(
            non_sync_ctx.trigger_timers_for(
                REACHABLE_TIME.into(),
                handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
            ),
            [NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::ReachableTime)]
        );
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Stale(Stale { link_address: LINK_ADDR1 }),
        );
    }

    #[ip_test]
    #[test_case(InitialState::Reachable, true; "reachable with different address")]
    #[test_case(InitialState::Reachable, false; "reachable with same address")]
    #[test_case(InitialState::Stale, true; "stale with different address")]
    #[test_case(InitialState::Stale, false; "stale with same address")]
    #[test_case(InitialState::Delay, true; "delay with different address")]
    #[test_case(InitialState::Delay, false; "delay with same address")]
    #[test_case(InitialState::Probe, true; "probe with different address")]
    #[test_case(InitialState::Probe, false; "probe with same address")]
    #[test_case(InitialState::Unreachable, true; "unreachable with different address")]
    #[test_case(InitialState::Unreachable, false; "unreachable with same address")]
    fn transition_to_stale_on_probe_with_different_address<I: Ip + TestIpExt>(
        initial_state: InitialState,
        update_link_address: bool,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor.
        let initial_state = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        // Handle an incoming probe, possibly with an updated link address.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            if update_link_address { LINK_ADDR2 } else { LINK_ADDR1 },
            DynamicNeighborUpdateSource::Probe,
        );

        // If the link address was updated, the neighbor should now be in STALE with the
        // new link address, per RFC 4861 section 7.2.3.
        //
        // If the link address is the same, the entry should remain in its initial
        // state.
        let expected_state = if update_link_address {
            DynamicNeighborState::Stale(Stale { link_address: LINK_ADDR2 })
        } else {
            initial_state
        };
        assert_neighbor_state(&sync_ctx, expected_state);
    }

    #[ip_test]
    #[test_case(InitialState::Reachable, true; "reachable with override flag set")]
    #[test_case(InitialState::Reachable, false; "reachable with override flag not set")]
    #[test_case(InitialState::Stale, true; "stale with override flag set")]
    #[test_case(InitialState::Stale, false; "stale with override flag not set")]
    #[test_case(InitialState::Delay, true; "delay with override flag set")]
    #[test_case(InitialState::Delay, false; "delay with override flag not set")]
    #[test_case(InitialState::Probe, true; "probe with override flag set")]
    #[test_case(InitialState::Probe, false; "probe with override flag not set")]
    #[test_case(InitialState::Unreachable, true; "unreachable with override flag set")]
    #[test_case(InitialState::Unreachable, false; "unreachable with override flag not set")]
    fn transition_to_reachable_on_solicited_confirmation_same_address<I: Ip + TestIpExt>(
        initial_state: InitialState,
        override_flag: bool,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor.
        let _ = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        // Handle an incoming solicited confirmation.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag: true,
                override_flag,
            }),
        );

        // Neighbor should now be in REACHABLE, per RFC 4861 section 7.2.5.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
    }

    #[ip_test]
    #[test_case(InitialState::Reachable; "reachable")]
    #[test_case(InitialState::Stale; "stale")]
    #[test_case(InitialState::Delay; "delay")]
    #[test_case(InitialState::Probe; "probe")]
    #[test_case(InitialState::Unreachable; "unreachable")]
    fn transition_to_stale_on_unsolicited_override_confirmation_with_different_address<
        I: Ip + TestIpExt,
    >(
        initial_state: InitialState,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor.
        let _ = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        // Handle an incoming unsolicited override confirmation with a different link address.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR2,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag: false,
                override_flag: true,
            }),
        );

        // Neighbor should now be in STALE, per RFC 4861 section 7.2.5.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Stale(Stale { link_address: LINK_ADDR2 }),
        );
    }

    #[ip_test]
    #[test_case(InitialState::Reachable, true; "reachable with override flag set")]
    #[test_case(InitialState::Reachable, false; "reachable with override flag not set")]
    #[test_case(InitialState::Stale, true; "stale with override flag set")]
    #[test_case(InitialState::Stale, false; "stale with override flag not set")]
    #[test_case(InitialState::Delay, true; "delay with override flag set")]
    #[test_case(InitialState::Delay, false; "delay with override flag not set")]
    #[test_case(InitialState::Probe, true; "probe with override flag set")]
    #[test_case(InitialState::Probe, false; "probe with override flag not set")]
    #[test_case(InitialState::Unreachable, true; "unreachable with override flag set")]
    #[test_case(InitialState::Unreachable, false; "unreachable with override flag not set")]
    fn noop_on_unsolicited_confirmation_with_same_address<I: Ip + TestIpExt>(
        initial_state: InitialState,
        override_flag: bool,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor.
        let expected_state =
            init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        // Handle an incoming unsolicited confirmation with the same link address.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag: false,
                override_flag,
            }),
        );

        // Neighbor should not have been updated.
        assert_neighbor_state(&sync_ctx, expected_state);
    }

    #[ip_test]
    #[test_case(InitialState::Reachable; "reachable")]
    #[test_case(InitialState::Stale; "stale")]
    #[test_case(InitialState::Delay; "delay")]
    #[test_case(InitialState::Probe; "probe")]
    #[test_case(InitialState::Unreachable; "unreachable")]
    fn transition_to_reachable_on_solicited_override_confirmation_with_different_address<
        I: Ip + TestIpExt,
    >(
        initial_state: InitialState,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor.
        let _ = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        // Handle an incoming solicited override confirmation with a different link address.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR2,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag: true,
                override_flag: true,
            }),
        );

        // Neighbor should now be in REACHABLE, per RFC 4861 section 7.2.5.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR2,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
    }

    #[ip_test]
    fn reachable_to_reachable_on_probe_with_same_address<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor in REACHABLE.
        init_reachable_neighbor(&mut sync_ctx, &mut non_sync_ctx, LINK_ADDR1);

        // Handle an incoming probe with the same link address.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
            DynamicNeighborUpdateSource::Probe,
        );

        // Neighbor should still be in REACHABLE with the same link address.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
    }

    #[ip_test]
    #[test_case(true; "solicited")]
    #[test_case(false; "unsolicited")]
    fn reachable_to_stale_on_non_override_confirmation_with_different_address<I: Ip + TestIpExt>(
        solicited_flag: bool,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor in REACHABLE.
        init_reachable_neighbor(&mut sync_ctx, &mut non_sync_ctx, LINK_ADDR1);

        // Handle an incoming non-override confirmation with a different link address.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR2,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                override_flag: false,
                solicited_flag,
            }),
        );

        // Neighbor should now be in STALE, with the *same* link address as was
        // previously cached, per RFC 4861 section 7.2.5.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Stale(Stale { link_address: LINK_ADDR1 }),
        );
    }

    #[ip_test]
    #[test_case(InitialState::Stale, true; "stale solicited")]
    #[test_case(InitialState::Stale, false; "stale unsolicited")]
    #[test_case(InitialState::Delay, true; "delay solicited")]
    #[test_case(InitialState::Delay, false; "delay unsolicited")]
    #[test_case(InitialState::Probe, true; "probe solicited")]
    #[test_case(InitialState::Probe, false; "probe unsolicited")]
    #[test_case(InitialState::Unreachable, true; "unreachable solicited")]
    #[test_case(InitialState::Unreachable, false; "unreachable unsolicited")]
    fn noop_on_non_override_confirmation_with_different_address<I: Ip + TestIpExt>(
        initial_state: InitialState,
        solicited_flag: bool,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor.
        let initial_state = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        // Handle an incoming non-override confirmation with a different link address.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR2,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                override_flag: false,
                solicited_flag,
            }),
        );

        // Neighbor should still be in the original state; the link address should *not*
        // have been updated.
        assert_neighbor_state(&sync_ctx, initial_state);
    }

    #[ip_test]
    fn stale_to_delay_on_packet_sent<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Initialize a neighbor in STALE.
        init_stale_neighbor(&mut sync_ctx, &mut non_sync_ctx, LINK_ADDR1);

        // Send a packet to the neighbor.
        let body = 1;
        assert_eq!(
            BufferNudHandler::send_ip_packet_to_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                I::LOOKUP_ADDR1,
                Buf::new([body], ..),
            ),
            Ok(())
        );

        // Neighbor should be in DELAY.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Delay(Delay { link_address: LINK_ADDR1 }),
        );
        non_sync_ctx.timer_ctx().assert_timers_installed([(
            NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::DelayFirstProbe),
            non_sync_ctx.now() + DELAY_FIRST_PROBE_TIME.get(),
        )]);
        assert_pending_frame_sent(
            &mut sync_ctx,
            VecDeque::from([Buf::new(vec![body], ..)]),
            LINK_ADDR1,
        );
    }

    #[ip_test]
    #[test_case(InitialState::Delay,
                NudEvent::DelayFirstProbe;
                "delay to probe")]
    #[test_case(InitialState::Probe,
                NudEvent::RetransmitUnicastProbe;
                "probe retransmit unicast probe")]
    fn delay_or_probe_to_probe_on_timeout<I: Ip + TestIpExt>(
        initial_state: InitialState,
        expected_initial_event: NudEvent,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::with_inner_and_outer_state(
                FakeConfigContext { retrans_timer: ONE_SECOND },
                FakeNudContext { nud: NudState::default() },
            ));

        // Initialize a neighbor.
        let _ = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        // If the neighbor started in DELAY, then after DELAY_FIRST_PROBE_TIME, the
        // neighbor should transition to PROBE and send out a unicast probe.
        //
        // If the neighbor started in PROBE, then after RetransTimer expires, the
        // neighbor should remain in PROBE and retransmit a unicast probe.
        let (time, transmit_counter) = match initial_state {
            InitialState::Delay => {
                (DELAY_FIRST_PROBE_TIME, NonZeroU8::new(MAX_UNICAST_SOLICIT - 1))
            }
            InitialState::Probe => {
                (sync_ctx.inner.get_ref().retrans_timer, NonZeroU8::new(MAX_UNICAST_SOLICIT - 2))
            }
            other => unreachable!("test only covers DELAY and PROBE, got {:?}", other),
        };
        assert_eq!(
            non_sync_ctx.trigger_timers_for(
                time.into(),
                handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
            ),
            [NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, expected_initial_event)]
        );
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Probe(Probe { link_address: LINK_ADDR1, transmit_counter }),
        );
        non_sync_ctx.timer_ctx().assert_timers_installed([(
            NudTimerId::neighbor(
                FakeLinkDeviceId,
                I::LOOKUP_ADDR1,
                NudEvent::RetransmitUnicastProbe,
            ),
            non_sync_ctx.now() + sync_ctx.inner.get_ref().retrans_timer.get(),
        )]);
        assert_neighbor_probe_sent(&mut sync_ctx, Some(LINK_ADDR1));
    }

    #[ip_test]
    fn unreachable_probes_with_exponential_backoff_while_packets_sent<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        init_unreachable_neighbor(&mut sync_ctx, &mut non_sync_ctx, LINK_ADDR1);

        let retrans_timer = sync_ctx.inner.retransmit_timeout().get();
        let timer_id = NudTimerId::neighbor(
            FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            NudEvent::RetransmitMulticastProbe,
        );

        // No multicast probes should be transmitted even after the retransmit timeout.
        assert_eq!(
            non_sync_ctx.trigger_timers_for(
                retrans_timer,
                handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
            ),
            []
        );
        assert_eq!(sync_ctx.inner.take_frames(), []);

        // Send a packet and ensure that we also transmit a multicast probe.
        const BODY: u8 = 0x33;
        assert_eq!(
            BufferNudHandler::send_ip_packet_to_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                I::LOOKUP_ADDR1,
                Buf::new([BODY], ..),
            ),
            Ok(())
        );
        assert_eq!(
            sync_ctx.inner.take_frames(),
            [
                (FakeNudMessageMeta::IpFrame { dst_link_address: LINK_ADDR1 }, vec![BODY]),
                (
                    FakeNudMessageMeta::NeighborSolicitation {
                        lookup_addr: I::LOOKUP_ADDR1,
                        remote_link_addr: /* multicast */ None,
                    },
                    Vec::new()
                )
            ]
        );

        let next_backoff_timer = |sync_ctx: &mut FakeCtxImpl<I>, probes_sent| {
            UnreachableMode::Backoff {
                probes_sent: NonZeroU32::new(probes_sent).unwrap(),
                packet_sent: /* unused */ false,
            }
            .next_backoff_retransmit_timeout::<I, _>(sync_ctx.inner.get_mut())
            .get()
        };

        const ITERATIONS: u8 = 2;
        for i in 1..ITERATIONS {
            let probes_sent = u32::from(i);

            // Send another packet before the retransmit timer expires: only the packet
            // should be sent (not a probe), and the `packet_sent` flag should be set.
            assert_eq!(
                BufferNudHandler::send_ip_packet_to_neighbor(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    &FakeLinkDeviceId,
                    I::LOOKUP_ADDR1,
                    Buf::new([BODY + i], ..),
                ),
                Ok(())
            );
            assert_eq!(
                sync_ctx.inner.take_frames(),
                [(FakeNudMessageMeta::IpFrame { dst_link_address: LINK_ADDR1 }, vec![BODY + i])]
            );

            // Fast forward until the current retransmit timer should fire, taking
            // exponential backoff into account. Another multicast probe should be
            // transmitted and a new timer should be scheduled (backing off further) because
            // a packet was recently sent.
            assert_eq!(
                non_sync_ctx.trigger_timers_for(
                    next_backoff_timer(&mut sync_ctx, probes_sent),
                    handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
                ),
                [timer_id]
            );
            assert_neighbor_probe_sent(&mut sync_ctx, /* multicast */ None);
            non_sync_ctx.timer_ctx().assert_timers_installed([(
                timer_id,
                non_sync_ctx.now() + next_backoff_timer(&mut sync_ctx, probes_sent + 1),
            )]);
        }

        // If no more packets are sent, no multicast probes should be transmitted even
        // after the next backoff timer expires.
        let current_timer = next_backoff_timer(&mut sync_ctx, u32::from(ITERATIONS));
        assert_eq!(
            non_sync_ctx.trigger_timers_for(
                current_timer,
                handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
            ),
            [timer_id]
        );
        assert_eq!(sync_ctx.inner.take_frames(), []);
        non_sync_ctx.timer_ctx().assert_no_timers_installed();

        // Finally, if another packet is sent, we resume transmitting multicast probes
        // and "reset" the exponential backoff.
        assert_eq!(
            BufferNudHandler::send_ip_packet_to_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                I::LOOKUP_ADDR1,
                Buf::new([BODY], ..),
            ),
            Ok(())
        );
        assert_eq!(
            sync_ctx.inner.take_frames(),
            [
                (FakeNudMessageMeta::IpFrame { dst_link_address: LINK_ADDR1 }, vec![BODY]),
                (
                    FakeNudMessageMeta::NeighborSolicitation {
                        lookup_addr: I::LOOKUP_ADDR1,
                        remote_link_addr: /* multicast */ None,
                    },
                    Vec::new()
                )
            ]
        );
        non_sync_ctx.timer_ctx().assert_timers_installed([(
            timer_id,
            non_sync_ctx.now() + next_backoff_timer(&mut sync_ctx, 1),
        )]);
    }

    #[ip_test]
    #[test_case(true; "solicited confirmation")]
    #[test_case(false; "unsolicited confirmation")]
    fn confirmation_should_not_create_entry<I: Ip + TestIpExt>(solicited_flag: bool) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        let link_addr = FakeLinkAddress([1]);
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            link_addr,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag,
                override_flag: false,
            }),
        );
        assert_eq!(sync_ctx.outer.nud.neighbors, HashMap::new());
    }

    #[ip_test]
    #[test_case(true; "set_with_dynamic")]
    #[test_case(false; "set_with_static")]
    fn pending_frames<I: Ip + TestIpExt>(dynamic: bool) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        assert_eq!(sync_ctx.inner.take_frames(), []);

        // Send up to the maximum number of pending frames to some neighbor
        // which requires resolution. This should cause all frames to be queued
        // pending resolution completion.
        const MAX_PENDING_FRAMES_U8: u8 = MAX_PENDING_FRAMES as u8;
        let expected_pending_frames =
            (0..MAX_PENDING_FRAMES_U8).map(|i| Buf::new(vec![i], ..)).collect::<VecDeque<_>>();

        for body in expected_pending_frames.iter() {
            assert_eq!(
                BufferNudHandler::send_ip_packet_to_neighbor(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    &FakeLinkDeviceId,
                    I::LOOKUP_ADDR1,
                    body.clone()
                ),
                Ok(())
            );
        }
        // Should have only sent out a single neighbor probe message.
        assert_neighbor_probe_sent(&mut sync_ctx, None);
        assert_matches!(
            sync_ctx.outer.nud.neighbors.get(&I::LOOKUP_ADDR1),
            Some(NeighborState::Dynamic(DynamicNeighborState::Incomplete(Incomplete {
                transmit_counter: _,
                pending_frames,
                notifiers: _,
                _marker,
            }))) => {
                assert_eq!(pending_frames, &expected_pending_frames);
            }
        );

        // The next frame should be dropped.
        assert_eq!(
            BufferNudHandler::send_ip_packet_to_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                I::LOOKUP_ADDR1,
                Buf::new([123], ..),
            ),
            Ok(())
        );
        assert_eq!(sync_ctx.inner.take_frames(), []);
        assert_matches!(
            sync_ctx.outer.nud.neighbors.get(&I::LOOKUP_ADDR1),
            Some(NeighborState::Dynamic(DynamicNeighborState::Incomplete(Incomplete{
                transmit_counter: _,
                pending_frames,
                notifiers: _,
                _marker,
            }))) => {
                assert_eq!(pending_frames, &expected_pending_frames);
            }
        );

        // Completing resolution should result in all queued packets being sent.
        if dynamic {
            NudHandler::handle_neighbor_update(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                I::LOOKUP_ADDR1,
                LINK_ADDR1,
                DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                    solicited_flag: true,
                    override_flag: false,
                }),
            );
            non_sync_ctx.timer_ctx().assert_timers_installed([(
                NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::ReachableTime),
                non_sync_ctx.now() + REACHABLE_TIME.get(),
            )]);
        } else {
            NudHandler::set_static_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                I::LOOKUP_ADDR1,
                LINK_ADDR1,
            );
            non_sync_ctx.timer_ctx().assert_no_timers_installed();
        }
        assert_eq!(
            sync_ctx.inner.take_frames(),
            expected_pending_frames
                .into_iter()
                .map(|p| (
                    FakeNudMessageMeta::IpFrame { dst_link_address: LINK_ADDR1 },
                    p.as_ref().to_vec()
                ))
                .collect::<Vec<_>>()
        );
    }

    #[ip_test]
    fn static_neighbor<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        NudHandler::set_static_neighbor(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
        );
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);
        check_lookup_has(&mut sync_ctx, &mut non_sync_ctx, I::LOOKUP_ADDR1, LINK_ADDR1);

        // Dynamic entries should not overwrite static entries.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR2,
            DynamicNeighborUpdateSource::Probe,
        );
        check_lookup_has(&mut sync_ctx, &mut non_sync_ctx, I::LOOKUP_ADDR1, LINK_ADDR1);

        NudHandler::delete_neighbor(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
        )
        .expect("neighbor entry should exist");

        let FakeNudContext { nud: NudState { neighbors, last_gc: _ } } = &sync_ctx.outer;
        assert!(neighbors.is_empty(), "neighbor table should be empty: {neighbors:?}");
    }

    #[ip_test]
    fn dynamic_neighbor<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
            DynamicNeighborUpdateSource::Probe,
        );
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);
        check_lookup_has(&mut sync_ctx, &mut non_sync_ctx, I::LOOKUP_ADDR1, LINK_ADDR1);

        // Dynamic entries may be overwritten by new dynamic entries.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR2,
            DynamicNeighborUpdateSource::Probe,
        );
        check_lookup_has(&mut sync_ctx, &mut non_sync_ctx, I::LOOKUP_ADDR1, LINK_ADDR2);
        assert_eq!(sync_ctx.inner.take_frames(), []);

        // A static entry may overwrite a dynamic entry.
        NudHandler::set_static_neighbor(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR3,
        );
        check_lookup_has(&mut sync_ctx, &mut non_sync_ctx, I::LOOKUP_ADDR1, LINK_ADDR3);
        assert_eq!(sync_ctx.inner.take_frames(), []);
    }

    #[ip_test]
    fn send_solicitation_on_lookup<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);

        let mut pending_frames = VecDeque::new();

        queue_ip_packet_to_unresolved_neighbor(
            &mut sync_ctx,
            &mut non_sync_ctx,
            I::LOOKUP_ADDR1,
            &mut pending_frames,
            1,
        );
        assert_neighbor_probe_sent(&mut sync_ctx, None);

        queue_ip_packet_to_unresolved_neighbor(
            &mut sync_ctx,
            &mut non_sync_ctx,
            I::LOOKUP_ADDR1,
            &mut pending_frames,
            2,
        );
        assert_eq!(sync_ctx.inner.take_frames(), []);

        // Complete link resolution.
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
            DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                solicited_flag: true,
                override_flag: false,
            }),
        );
        check_lookup_has(&mut sync_ctx, &mut non_sync_ctx, I::LOOKUP_ADDR1, LINK_ADDR1);

        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
        assert_eq!(
            sync_ctx.inner.take_frames(),
            pending_frames
                .into_iter()
                .map(|f| (
                    FakeNudMessageMeta::IpFrame { dst_link_address: LINK_ADDR1 },
                    f.as_ref().to_vec(),
                ))
                .collect::<Vec<_>>()
        );
    }

    #[ip_test]
    fn solicitation_failure_in_incomplete<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);

        let pending_frames = init_incomplete_neighbor(&mut sync_ctx, &mut non_sync_ctx, false);

        let timer_id = NudTimerId::neighbor(
            FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            NudEvent::RetransmitMulticastProbe,
        );
        for i in 1..=MAX_MULTICAST_SOLICIT {
            let FakeConfigContext { retrans_timer } = &sync_ctx.inner.get_ref();
            let retrans_timer = retrans_timer.get();

            assert_neighbor_state(
                &sync_ctx,
                DynamicNeighborState::Incomplete(Incomplete {
                    transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT - i),
                    pending_frames: pending_frames.clone(),
                    notifiers: Vec::new(),
                    _marker: PhantomData,
                }),
            );

            non_sync_ctx
                .timer_ctx()
                .assert_timers_installed([(timer_id, non_sync_ctx.now() + ONE_SECOND.get())]);
            assert_neighbor_probe_sent(&mut sync_ctx, /* multicast */ None);

            assert_eq!(
                non_sync_ctx.trigger_timers_for(
                    retrans_timer,
                    handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
                ),
                [timer_id]
            );
        }

        // The neighbor entry should have been removed.
        assert_eq!(sync_ctx.outer.nud.neighbors, HashMap::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);
    }

    #[ip_test]
    fn solicitation_failure_in_probe<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);

        init_probe_neighbor(&mut sync_ctx, &mut non_sync_ctx, LINK_ADDR1, false);

        let timer_id = NudTimerId::neighbor(
            FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            NudEvent::RetransmitUnicastProbe,
        );
        for i in 1..=MAX_UNICAST_SOLICIT {
            let FakeConfigContext { retrans_timer } = &sync_ctx.inner.get_ref();
            let retrans_timer = retrans_timer.get();

            assert_neighbor_state(
                &sync_ctx,
                DynamicNeighborState::Probe(Probe {
                    transmit_counter: NonZeroU8::new(MAX_UNICAST_SOLICIT - i),
                    link_address: LINK_ADDR1,
                }),
            );

            non_sync_ctx
                .timer_ctx()
                .assert_timers_installed([(timer_id, non_sync_ctx.now() + ONE_SECOND.get())]);
            assert_neighbor_probe_sent(&mut sync_ctx, Some(LINK_ADDR1));

            assert_eq!(
                non_sync_ctx.trigger_timers_for(
                    retrans_timer,
                    handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
                ),
                [timer_id]
            );
        }

        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Unreachable(Unreachable {
                link_address: LINK_ADDR1,
                mode: UnreachableMode::WaitingForPacketSend,
            }),
        );
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);
    }

    #[ip_test]
    fn flush_entries<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);

        NudHandler::set_static_neighbor(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
            LINK_ADDR1,
        );
        NudHandler::handle_neighbor_update(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR2,
            LINK_ADDR2,
            DynamicNeighborUpdateSource::Probe,
        );
        let body = [3];
        let pending_frames = VecDeque::from([Buf::new(body.to_vec(), ..)]);
        assert_eq!(
            BufferNudHandler::send_ip_packet_to_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                I::LOOKUP_ADDR3,
                Buf::new(body, ..),
            ),
            Ok(())
        );

        let FakeNudContext { nud } = &sync_ctx.outer;
        assert_eq!(
            nud.neighbors,
            HashMap::from([
                (I::LOOKUP_ADDR1, NeighborState::Static(LINK_ADDR1)),
                (
                    I::LOOKUP_ADDR2,
                    NeighborState::Dynamic(DynamicNeighborState::Stale(Stale {
                        link_address: LINK_ADDR2,
                    })),
                ),
                (
                    I::LOOKUP_ADDR3,
                    NeighborState::Dynamic(DynamicNeighborState::Incomplete(Incomplete {
                        transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT - 1),
                        pending_frames: pending_frames,
                        notifiers: Vec::new(),
                        _marker: PhantomData,
                    })),
                ),
            ]),
        );
        non_sync_ctx.timer_ctx().assert_timers_installed([(
            NudTimerId::neighbor(
                FakeLinkDeviceId,
                I::LOOKUP_ADDR3,
                NudEvent::RetransmitMulticastProbe,
            ),
            non_sync_ctx.now() + ONE_SECOND.get(),
        )]);

        // Flushing the table should clear all dynamic entries and timers.
        NudHandler::flush(&mut sync_ctx, &mut non_sync_ctx, &FakeLinkDeviceId);
        let FakeNudContext { nud } = &sync_ctx.outer;
        assert_eq!(
            nud.neighbors,
            HashMap::from([(I::LOOKUP_ADDR1, NeighborState::Static(LINK_ADDR1))])
        );
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
    }

    #[ip_test]
    fn delete_dynamic_entry<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);

        init_reachable_neighbor(&mut sync_ctx, &mut non_sync_ctx, LINK_ADDR1);
        check_lookup_has(&mut sync_ctx, &mut non_sync_ctx, I::LOOKUP_ADDR1, LINK_ADDR1);

        NudHandler::delete_neighbor(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            I::LOOKUP_ADDR1,
        )
        .expect("neighbor entry should exist");

        // Entry should be removed and timer cancelled.
        let FakeNudContext { nud: NudState { neighbors, last_gc: _ } } = &sync_ctx.outer;
        assert!(neighbors.is_empty(), "neighbor table should be empty: {neighbors:?}");
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
    }

    fn assert_neighbors<
        'a,
        I: Ip,
        C: crate::NonSyncContext + NonSyncContext<I, EthernetLinkDevice, EthernetDeviceId<C>>,
    >(
        sync_ctx: &'a SyncCtx<C>,
        device_id: &EthernetDeviceId<C>,
        expected: HashMap<
            SpecifiedAddr<I::Addr>,
            NeighborState<EthernetLinkDevice, C::Instant, C::Notifier>,
        >,
    ) where
        Locked<&'a SyncCtx<C>, crate::lock_ordering::Unlocked>: NudContext<I, EthernetLinkDevice, C>
            + DeviceIdContext<EthernetLinkDevice, DeviceId = EthernetDeviceId<C>>,
        <C as LinkResolutionContext<EthernetLinkDevice>>::Notifier: PartialEq,
    {
        NudContext::<I, EthernetLinkDevice, _>::with_nud_state_mut(
            &mut Locked::new(sync_ctx),
            device_id,
            |NudState { neighbors, last_gc: _ }, _config| assert_eq!(*neighbors, expected),
        )
    }

    #[ip_test]
    #[test_case(InitialState::Reachable; "reachable neighbor")]
    #[test_case(InitialState::Stale; "stale neighbor")]
    #[test_case(InitialState::Delay; "delay neighbor")]
    #[test_case(InitialState::Probe; "probe neighbor")]
    #[test_case(InitialState::Unreachable; "unreachable neighbor")]
    fn resolve_cached_linked_addr<I: Ip + TestIpExt>(initial_state: InitialState) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(sync_ctx.inner.take_frames(), []);

        let _ = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        let link_addr = assert_matches!(
            NudHandler::resolve_link_addr(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                &I::LOOKUP_ADDR1,
            ),
            LinkResolutionResult::Resolved(addr) => addr
        );
        assert_eq!(link_addr, LINK_ADDR1);
    }

    enum ResolutionSuccess {
        Confirmation,
        StaticEntryAdded,
    }

    #[ip_test]
    #[test_case(ResolutionSuccess::Confirmation; "incomplete entry timed out")]
    #[test_case(ResolutionSuccess::StaticEntryAdded; "incomplete entry removed from table")]
    fn dynamic_neighbor_resolution_success<I: Ip + TestIpExt>(reason: ResolutionSuccess) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        let observers = (0..10)
            .map(|_| {
                let observer = assert_matches!(
                    NudHandler::resolve_link_addr(
                        &mut sync_ctx,
                        &mut non_sync_ctx,
                        &FakeLinkDeviceId,
                        &I::LOOKUP_ADDR1,
                    ),
                    LinkResolutionResult::Pending(observer) => observer
                );
                assert_eq!(*observer.lock(), None);
                observer
            })
            .collect::<Vec<_>>();

        // We should have initialized an incomplete neighbor and sent a neighbor probe
        // to attempt resolution.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Incomplete(Incomplete {
                transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT - 1),
                pending_frames: VecDeque::new(),
                // NB: notifiers is not checked for equality.
                notifiers: Vec::new(),
                _marker: PhantomData,
            }),
        );
        assert_neighbor_probe_sent(&mut sync_ctx, /* multicast */ None);

        match reason {
            ResolutionSuccess::Confirmation => {
                // Complete neighbor resolution with an incoming neighbor confirmation.
                NudHandler::handle_neighbor_update(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    &FakeLinkDeviceId,
                    I::LOOKUP_ADDR1,
                    LINK_ADDR1,
                    DynamicNeighborUpdateSource::Confirmation(ConfirmationFlags {
                        solicited_flag: true,
                        override_flag: false,
                    }),
                );
                assert_neighbor_state(
                    &sync_ctx,
                    DynamicNeighborState::Reachable(Reachable {
                        link_address: LINK_ADDR1,
                        last_confirmed_at: non_sync_ctx.now(),
                    }),
                );
            }
            ResolutionSuccess::StaticEntryAdded => {
                NudHandler::set_static_neighbor(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    &FakeLinkDeviceId,
                    I::LOOKUP_ADDR1,
                    LINK_ADDR1,
                );
                assert_eq!(
                    sync_ctx.outer.nud.neighbors.get(&I::LOOKUP_ADDR1),
                    Some(&NeighborState::Static(LINK_ADDR1))
                );
            }
        }

        // Each observer should have been notified of successful link resolution.
        for observer in observers {
            assert_eq!(*observer.lock(), Some(Ok(LINK_ADDR1)));
        }
    }

    enum ResolutionFailure {
        Timeout,
        Removed,
    }

    #[ip_test]
    #[test_case(ResolutionFailure::Timeout; "incomplete entry timed out")]
    #[test_case(ResolutionFailure::Removed; "incomplete entry removed from table")]
    fn dynamic_neighbor_resolution_failure<I: Ip + TestIpExt>(reason: ResolutionFailure) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        let observers = (0..10)
            .map(|_| {
                let observer = assert_matches!(
                    NudHandler::resolve_link_addr(
                        &mut sync_ctx,
                        &mut non_sync_ctx,
                        &FakeLinkDeviceId,
                        &I::LOOKUP_ADDR1,
                    ),
                    LinkResolutionResult::Pending(observer) => observer
                );
                assert_eq!(*observer.lock(), None);
                observer
            })
            .collect::<Vec<_>>();

        // We should have initialized an incomplete neighbor and sent a neighbor probe
        // to attempt resolution.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Incomplete(Incomplete {
                transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT - 1),
                pending_frames: VecDeque::new(),
                // NB: notifiers is not checked for equality.
                notifiers: Vec::new(),
                _marker: PhantomData,
            }),
        );
        assert_neighbor_probe_sent(&mut sync_ctx, /* multicast */ None);

        match reason {
            ResolutionFailure::Timeout => {
                // Wait until neighbor resolution exceeds its maximum probe retransmits and
                // times out.
                for _ in 1..=MAX_MULTICAST_SOLICIT {
                    let FakeConfigContext { retrans_timer } = &sync_ctx.inner.get_ref();
                    let retrans_timer = retrans_timer.get();
                    assert_eq!(
                        non_sync_ctx.trigger_timers_for(
                            retrans_timer,
                            handle_timer_helper_with_sc_ref_mut(
                                &mut sync_ctx,
                                TimerHandler::handle_timer
                            ),
                        ),
                        [NudTimerId::neighbor(
                            FakeLinkDeviceId,
                            I::LOOKUP_ADDR1,
                            NudEvent::RetransmitMulticastProbe,
                        )]
                    );
                }
            }
            ResolutionFailure::Removed => {
                // Flush the neighbor table so the entry is removed.
                NudHandler::flush(&mut sync_ctx, &mut non_sync_ctx, &FakeLinkDeviceId);
            }
        }

        super::testutil::assert_neighbor_unknown(&mut sync_ctx, FakeLinkDeviceId, I::LOOKUP_ADDR1);
        // Each observer should have been notified of link resolution failure.
        for observer in observers {
            assert_eq!(*observer.lock(), Some(Err(AddressResolutionFailed)));
        }
    }

    #[ip_test]
    #[test_case(InitialState::Incomplete, false; "incomplete neighbor")]
    #[test_case(InitialState::Reachable, true; "reachable neighbor")]
    #[test_case(InitialState::Stale, true; "stale neighbor")]
    #[test_case(InitialState::Delay, true; "delay neighbor")]
    #[test_case(InitialState::Probe, true; "probe neighbor")]
    #[test_case(InitialState::Unreachable, true; "unreachable neighbor")]
    fn upper_layer_confirmation<I: Ip + TestIpExt>(
        initial_state: InitialState,
        should_transition_to_reachable: bool,
    ) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        let initial = init_neighbor_in_state(&mut sync_ctx, &mut non_sync_ctx, initial_state);

        confirm_reachable(&mut sync_ctx, &mut non_sync_ctx, &FakeLinkDeviceId, I::LOOKUP_ADDR1);

        if !should_transition_to_reachable {
            assert_neighbor_state(&sync_ctx, initial);
            return;
        }

        // Neighbor should have transitioned to REACHABLE and scheduled a timer.
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
        non_sync_ctx.timer_ctx().assert_timers_installed([(
            NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::ReachableTime),
            non_sync_ctx.now() + REACHABLE_TIME.get(),
        )]);

        // Advance the clock by less than REACHABLE_TIME and confirm reachability again.
        // The existing timer should not have been rescheduled; only the entry's
        // `last_confirmed_at` timestamp should have been updated.
        non_sync_ctx.timer_ctx_mut().instant.sleep(REACHABLE_TIME.get() / 2);
        confirm_reachable(&mut sync_ctx, &mut non_sync_ctx, &FakeLinkDeviceId, I::LOOKUP_ADDR1);
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
        non_sync_ctx.timer_ctx().assert_timers_installed([(
            NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::ReachableTime),
            non_sync_ctx.now() + REACHABLE_TIME.get() / 2,
        )]);

        // When the original timer eventually does expire, a new timer should be
        // scheduled based on when the entry was last confirmed.
        assert_eq!(
            non_sync_ctx.trigger_timers_for(
                REACHABLE_TIME.get() / 2,
                handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
            ),
            [NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::ReachableTime)]
        );
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now() - REACHABLE_TIME.get() / 2,
            }),
        );
        non_sync_ctx.timer_ctx().assert_timers_installed([(
            NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::ReachableTime),
            non_sync_ctx.now() + REACHABLE_TIME.get() / 2,
        )]);

        // When *that* timer fires, if the entry has not been confirmed since it was
        // scheduled, it should move into STALE.
        assert_eq!(
            non_sync_ctx.trigger_timers_for(
                REACHABLE_TIME.get() / 2,
                handle_timer_helper_with_sc_ref_mut(&mut sync_ctx, TimerHandler::handle_timer),
            ),
            [NudTimerId::neighbor(FakeLinkDeviceId, I::LOOKUP_ADDR1, NudEvent::ReachableTime)]
        );
        assert_neighbor_state(
            &sync_ctx,
            DynamicNeighborState::Stale(Stale { link_address: LINK_ADDR1 }),
        );
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
    }

    fn generate_ip_addr<I: Ip>(i: usize) -> SpecifiedAddr<I::Addr> {
        I::map_ip(
            IpInvariant(i),
            |IpInvariant(i)| {
                let start = u32::from_be_bytes(net_ip_v4!("192.168.0.1").ipv4_bytes());
                let bytes = (start + u32::try_from(i).unwrap()).to_be_bytes();
                SpecifiedAddr::new(Ipv4Addr::new(bytes)).unwrap()
            },
            |IpInvariant(i)| {
                let start = u128::from_be_bytes(net_ip_v6!("fe80::1").ipv6_bytes());
                let bytes = (start + u128::try_from(i).unwrap()).to_be_bytes();
                SpecifiedAddr::new(Ipv6Addr::from_bytes(bytes)).unwrap()
            },
        )
    }

    #[ip_test]
    fn garbage_collection_retains_static_entries<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Add `MAX_ENTRIES` STALE dynamic neighbors and `MAX_ENTRIES` static
        // neighbors to the neighbor table, interleaved to avoid accidental
        // behavior re: insertion order.
        for i in 0..MAX_ENTRIES * 2 {
            if i % 2 == 0 {
                init_stale_neighbor_with_ip(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    generate_ip_addr::<I>(i),
                    LINK_ADDR1,
                );
            } else {
                NudHandler::set_static_neighbor(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    &FakeLinkDeviceId,
                    generate_ip_addr::<I>(i),
                    LINK_ADDR1,
                );
            }
        }
        assert_eq!(sync_ctx.outer.nud.neighbors.len(), MAX_ENTRIES * 2);

        // Perform GC, and ensure that only the dynamic entries are discarded.
        collect_garbage(&mut sync_ctx, &mut non_sync_ctx, &FakeLinkDeviceId);
        assert_eq!(sync_ctx.outer.nud.neighbors.len(), MAX_ENTRIES);
        for (_, neighbor) in sync_ctx.outer.nud.neighbors {
            assert_matches!(neighbor, NeighborState::Static(_));
        }
    }

    #[ip_test]
    fn garbage_collection_retains_in_use_entries<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Add enough static entries that the NUD table is near maximum capacity.
        for i in 0..MAX_ENTRIES - 1 {
            NudHandler::set_static_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                generate_ip_addr::<I>(i),
                LINK_ADDR1,
            );
        }

        // Add a STALE entry...
        let stale_entry = generate_ip_addr::<I>(MAX_ENTRIES - 1);
        init_stale_neighbor_with_ip(&mut sync_ctx, &mut non_sync_ctx, stale_entry, LINK_ADDR1);
        // ...and a REACHABLE entry.
        let reachable_entry = generate_ip_addr::<I>(MAX_ENTRIES);
        init_reachable_neighbor_with_ip(
            &mut sync_ctx,
            &mut non_sync_ctx,
            reachable_entry,
            LINK_ADDR1,
        );

        // Perform GC, and ensure that the REACHABLE entry was retained.
        collect_garbage(&mut sync_ctx, &mut non_sync_ctx, &FakeLinkDeviceId);
        super::testutil::assert_dynamic_neighbor_state(
            &mut sync_ctx,
            FakeLinkDeviceId,
            reachable_entry,
            DynamicNeighborState::Reachable(Reachable {
                link_address: LINK_ADDR1,
                last_confirmed_at: non_sync_ctx.now(),
            }),
        );
        super::testutil::assert_neighbor_unknown(&mut sync_ctx, FakeLinkDeviceId, stale_entry);
    }

    #[ip_test]
    fn garbage_collection_triggered_on_new_stale_entry<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        // Pretend we just ran GC so the next pass will be scheduled after a delay.
        sync_ctx.outer.nud.last_gc = Some(non_sync_ctx.now());

        // Fill the neighbor table to maximum capacity with static entries.
        for i in 0..MAX_ENTRIES {
            NudHandler::set_static_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                generate_ip_addr::<I>(i),
                LINK_ADDR1,
            );
        }

        // Add a STALE neighbor entry to the table, which should trigger a GC run
        // because it pushes the size of the table over the max.
        init_stale_neighbor_with_ip(
            &mut sync_ctx,
            &mut non_sync_ctx,
            generate_ip_addr::<I>(MAX_ENTRIES + 1),
            LINK_ADDR1,
        );
        let expected_gc_time = non_sync_ctx.now() + MIN_GARBAGE_COLLECTION_INTERVAL.get();
        non_sync_ctx.timer_ctx().assert_some_timers_installed([(
            NudTimerId::garbage_collection(FakeLinkDeviceId),
            expected_gc_time,
        )]);

        // Advance the clock by less than the GC interval and add another STALE entry to
        // trigger GC again. The existing GC timer should not have been rescheduled
        // given a GC pass is already pending.
        non_sync_ctx.timer_ctx_mut().instant.sleep(ONE_SECOND.get());
        init_stale_neighbor_with_ip(
            &mut sync_ctx,
            &mut non_sync_ctx,
            generate_ip_addr::<I>(MAX_ENTRIES + 2),
            LINK_ADDR1,
        );
        non_sync_ctx.timer_ctx().assert_some_timers_installed([(
            NudTimerId::garbage_collection(FakeLinkDeviceId),
            expected_gc_time,
        )]);
    }

    #[ip_test]
    fn garbage_collection_triggered_on_transition_to_unreachable<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());
        // Pretend we just ran GC so the next pass will be scheduled after a delay.
        sync_ctx.outer.nud.last_gc = Some(non_sync_ctx.now());

        // Fill the neighbor table to maximum capacity.
        for i in 0..MAX_ENTRIES {
            NudHandler::set_static_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                generate_ip_addr::<I>(i),
                LINK_ADDR1,
            );
        }
        assert_eq!(sync_ctx.outer.nud.neighbors.len(), MAX_ENTRIES);

        // Add a dynamic neighbor entry to the table and transition it to the
        // UNREACHABLE state. This should trigger a GC run.
        init_unreachable_neighbor_with_ip(
            &mut sync_ctx,
            &mut non_sync_ctx,
            generate_ip_addr::<I>(MAX_ENTRIES),
            LINK_ADDR1,
        );
        let expected_gc_time =
            sync_ctx.outer.nud.last_gc.unwrap() + MIN_GARBAGE_COLLECTION_INTERVAL.get();
        non_sync_ctx.timer_ctx().assert_some_timers_installed([(
            NudTimerId::garbage_collection(FakeLinkDeviceId),
            expected_gc_time,
        )]);

        // Add a new entry and transition it to UNREACHABLE. The existing GC timer
        // should not have been rescheduled given a GC pass is already pending.
        init_unreachable_neighbor_with_ip(
            &mut sync_ctx,
            &mut non_sync_ctx,
            generate_ip_addr::<I>(MAX_ENTRIES + 1),
            LINK_ADDR1,
        );
        non_sync_ctx.timer_ctx().assert_some_timers_installed([(
            NudTimerId::garbage_collection(FakeLinkDeviceId),
            expected_gc_time,
        )]);
    }

    #[ip_test]
    fn garbage_collection_not_triggered_on_new_incomplete_entry<I: Ip + TestIpExt>() {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeCtxImpl::<I>::new());

        // Fill the neighbor table to maximum capacity with static entries.
        for i in 0..MAX_ENTRIES {
            NudHandler::set_static_neighbor(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                generate_ip_addr::<I>(i),
                LINK_ADDR1,
            );
        }
        assert_eq!(sync_ctx.outer.nud.neighbors.len(), MAX_ENTRIES);

        let _: VecDeque<Buf<Vec<u8>>> = init_incomplete_neighbor_with_ip(
            &mut sync_ctx,
            &mut non_sync_ctx,
            generate_ip_addr::<I>(MAX_ENTRIES),
            true,
        );
        assert_eq!(
            non_sync_ctx
                .timer_ctx()
                .scheduled_instant(NudTimerId::garbage_collection(FakeLinkDeviceId)),
            None
        );
    }

    #[test]
    fn router_advertisement_with_source_link_layer_option_should_add_neighbor() {
        let FakeEventDispatcherConfig {
            local_mac,
            remote_mac,
            local_ip: _,
            remote_ip: _,
            subnet: _,
        } = Ipv6::FAKE_CONFIG;

        let testutil::FakeCtx { sync_ctx, mut non_sync_ctx } = testutil::FakeCtx::default();
        let sync_ctx = &sync_ctx;
        let device_id = crate::device::add_ethernet_device(
            sync_ctx,
            local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        Ipv6::set_ip_device_enabled(sync_ctx, &mut non_sync_ctx, &device_id, true, false);

        let remote_mac_bytes = remote_mac.bytes();
        let options = vec![NdpOptionBuilder::SourceLinkLayerAddress(&remote_mac_bytes[..])];

        let src_ip = remote_mac.to_ipv6_link_local().addr();
        let dst_ip = Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.get();
        let ra_packet_buf = |options: &[NdpOptionBuilder<'_>]| {
            OptionSequenceBuilder::new(options.iter())
                .into_serializer()
                .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                    src_ip,
                    dst_ip,
                    IcmpUnusedCode,
                    RouterAdvertisement::new(0, false, false, 0, 0, 0),
                ))
                .encapsulate(Ipv6PacketBuilder::new(
                    src_ip,
                    dst_ip,
                    REQUIRED_NDP_IP_PACKET_HOP_LIMIT,
                    Ipv6Proto::Icmpv6,
                ))
                .serialize_vec_outer()
                .unwrap()
                .unwrap_b()
        };

        // First receive a Router Advertisement without the source link layer
        // and make sure no new neighbor gets added.
        receive_ip_packet::<_, _, Ipv6>(
            &sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            FrameDestination::Multicast,
            ra_packet_buf(&[][..]),
        );
        let link_device_id = device_id.clone().try_into().unwrap();
        assert_neighbors::<Ipv6, _>(&sync_ctx, &link_device_id, Default::default());

        // RA with a source link layer option should create a new entry.
        receive_ip_packet::<_, _, Ipv6>(
            &sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            FrameDestination::Multicast,
            ra_packet_buf(&options[..]),
        );
        assert_neighbors::<Ipv6, _>(
            &sync_ctx,
            &link_device_id,
            HashMap::from([(
                {
                    let src_ip: UnicastAddr<_> = src_ip.into_addr();
                    src_ip.into_specified()
                },
                NeighborState::Dynamic(DynamicNeighborState::Stale(Stale {
                    link_address: remote_mac.get(),
                })),
            )]),
        );
    }

    const LOCAL_IP: Ipv6Addr = net_ip_v6!("fe80::1");
    const OTHER_IP: Ipv6Addr = net_ip_v6!("fe80::2");
    const MULTICAST_IP: Ipv6Addr = net_ip_v6!("ff02::1234");

    #[test_case(LOCAL_IP, None, true; "targeting assigned address")]
    #[test_case(LOCAL_IP, NonZeroU8::new(1), false; "targeting tentative address")]
    #[test_case(OTHER_IP, None, false; "targeting other host")]
    #[test_case(MULTICAST_IP, None, false; "targeting multicast address")]
    fn ns_response(target_addr: Ipv6Addr, dad_transmits: Option<NonZeroU8>, expect_handle: bool) {
        let FakeEventDispatcherConfig {
            local_mac,
            remote_mac,
            local_ip: _,
            remote_ip: _,
            subnet: _,
        } = Ipv6::FAKE_CONFIG;

        let testutil::FakeCtx { sync_ctx, mut non_sync_ctx } = testutil::FakeCtx::default();
        let sync_ctx = &sync_ctx;
        let link_device_id = crate::device::add_ethernet_device(
            sync_ctx,
            local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        );
        let device_id = link_device_id.clone().into();
        Ipv6::set_ip_device_enabled(sync_ctx, &mut non_sync_ctx, &device_id, true, false);

        // Set DAD config after enabling the device so that the default address
        // does not perform DAD.
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(dad_transmits),
                ..Default::default()
            },
        )
        .unwrap();
        crate::device::add_ip_addr_subnet(
            &sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            AddrSubnet::new(LOCAL_IP, Ipv6Addr::BYTES * 8).unwrap(),
        )
        .unwrap();
        if let Some(NonZeroU8 { .. }) = dad_transmits {
            // Take DAD message.
            assert_matches!(
                &non_sync_ctx.take_frames()[..],
                [(got_device_id, got_frame)] => {
                    assert_eq!(got_device_id, &link_device_id);

                    let (src_mac, dst_mac, got_src_ip, got_dst_ip, ttl, message, code) =
                        parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                            Ipv6,
                            _,
                            NeighborSolicitation,
                            _,
                        >(got_frame, EthernetFrameLengthCheck::NoCheck, |_| {})
                            .unwrap();
                    let dst_ip = LOCAL_IP.to_solicited_node_address();
                    assert_eq!(src_mac, local_mac.get());
                    assert_eq!(dst_mac, dst_ip.into());
                    assert_eq!(got_src_ip, Ipv6::UNSPECIFIED_ADDRESS);
                    assert_eq!(got_dst_ip, dst_ip.get());
                    assert_eq!(ttl, REQUIRED_NDP_IP_PACKET_HOP_LIMIT);
                    assert_eq!(message.target_address(), &LOCAL_IP);
                    assert_eq!(code, IcmpUnusedCode);
                }
            );
        }

        // Send a neighbor solicitation with the test target address to the
        // host.
        let src_ip = remote_mac.to_ipv6_link_local().addr();
        let snmc = target_addr.to_solicited_node_address();
        let dst_ip = snmc.get();
        receive_ip_packet::<_, _, Ipv6>(
            &sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            FrameDestination::Multicast,
            neighbor_solicitation_ip_packet(**src_ip, dst_ip, target_addr, *remote_mac),
        );

        // Check if a neighbor advertisement was sent as a response and the
        // new state of the neighbor table.
        let expected_neighbors = if expect_handle {
            assert_matches!(
                &non_sync_ctx.take_frames()[..],
                [(got_device_id, got_frame)] => {
                    assert_eq!(got_device_id, &link_device_id);

                    let (src_mac, dst_mac, got_src_ip, got_dst_ip, ttl, message, code) =
                        parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                            Ipv6,
                            _,
                            NeighborAdvertisement,
                            _,
                        >(got_frame, EthernetFrameLengthCheck::NoCheck, |_| {})
                            .unwrap();
                    assert_eq!(src_mac, local_mac.get());
                    assert_eq!(dst_mac, remote_mac.get());
                    assert_eq!(got_src_ip, target_addr);
                    assert_eq!(got_dst_ip, src_ip.into_addr());
                    assert_eq!(ttl, REQUIRED_NDP_IP_PACKET_HOP_LIMIT);
                    assert_eq!(message.target_address(), &target_addr);
                    assert_eq!(code, IcmpUnusedCode);
                }
            );

            HashMap::from([(
                {
                    let src_ip: UnicastAddr<_> = src_ip.into_addr();
                    src_ip.into_specified()
                },
                // TODO(https://fxbug.dev/131547): expect STALE instead once we correctly do not
                // go through NUD to send NDP packets.
                NeighborState::Dynamic(DynamicNeighborState::Delay(Delay {
                    link_address: remote_mac.get(),
                })),
            )])
        } else {
            assert_matches!(&non_sync_ctx.take_frames()[..], []);
            HashMap::default()
        };

        assert_neighbors::<Ipv6, _>(&sync_ctx, &link_device_id, expected_neighbors);
    }

    #[test]
    fn ipv6_integration() {
        let FakeEventDispatcherConfig {
            local_mac,
            remote_mac,
            local_ip: _,
            remote_ip: _,
            subnet: _,
        } = Ipv6::FAKE_CONFIG;

        let testutil::FakeCtx { sync_ctx, mut non_sync_ctx } = testutil::FakeCtx::default();
        let sync_ctx = &sync_ctx;
        let eth_device_id = crate::device::add_ethernet_device(
            sync_ctx,
            local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        );
        let device_id = eth_device_id.clone().into();
        // Configure the device to generate a link-local address.
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        Ipv6::set_ip_device_enabled(sync_ctx, &mut non_sync_ctx, &device_id, true, false);

        let neighbor_ip = remote_mac.to_ipv6_link_local().addr();
        let neighbor_ip: UnicastAddr<_> = neighbor_ip.into_addr();
        let dst_ip = Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.get();
        let na_packet_buf = |solicited_flag, override_flag| {
            neighbor_advertisement_ip_packet(
                *neighbor_ip,
                dst_ip,
                false, /* router_flag */
                solicited_flag,
                override_flag,
                *remote_mac,
            )
        };

        // NeighborAdvertisements should not create a new entry even if
        // the advertisement has both the solicited and override flag set.
        receive_ip_packet::<_, _, Ipv6>(
            &sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            FrameDestination::Multicast,
            na_packet_buf(false, false),
        );
        let link_device_id = device_id.clone().try_into().unwrap();
        assert_neighbors::<Ipv6, _>(&sync_ctx, &link_device_id, Default::default());
        receive_ip_packet::<_, _, Ipv6>(
            &sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            FrameDestination::Multicast,
            na_packet_buf(true, true),
        );
        assert_neighbors::<Ipv6, _>(&sync_ctx, &link_device_id, Default::default());

        assert_eq!(non_sync_ctx.take_frames(), []);

        // Trigger a neighbor solicitation to be sent.
        let body = [u8::MAX];
        let pending_frames = VecDeque::from([Buf::new(body.to_vec(), ..)]);
        assert_matches!(
            BufferNudHandler::<_, Ipv6, EthernetLinkDevice, _>::send_ip_packet_to_neighbor(
                &mut Locked::new(sync_ctx),
                &mut non_sync_ctx,
                &eth_device_id,
                neighbor_ip.into_specified(),
                Buf::new(body, ..),
            ),
            Ok(())
        );
        assert_matches!(
            &non_sync_ctx.take_frames()[..],
            [(got_device_id, got_frame)] => {
                assert_eq!(got_device_id, &eth_device_id);

                let (src_mac, dst_mac, got_src_ip, got_dst_ip, ttl, message, code) = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                    Ipv6,
                    _,
                    NeighborSolicitation,
                    _,
                >(got_frame, EthernetFrameLengthCheck::NoCheck, |_| {})
                    .unwrap();
                let target = neighbor_ip;
                let snmc = target.to_solicited_node_address();
                assert_eq!(src_mac, local_mac.get());
                assert_eq!(dst_mac, snmc.into());
                assert_eq!(got_src_ip, local_mac.to_ipv6_link_local().addr().into());
                assert_eq!(got_dst_ip, snmc.get());
                assert_eq!(ttl, 255);
                assert_eq!(message.target_address(), &target.get());
                assert_eq!(code, IcmpUnusedCode);
            }
        );
        assert_neighbors::<Ipv6, _>(
            &sync_ctx,
            &link_device_id,
            HashMap::from([(
                neighbor_ip.into_specified(),
                NeighborState::Dynamic(DynamicNeighborState::Incomplete(Incomplete {
                    transmit_counter: NonZeroU8::new(MAX_MULTICAST_SOLICIT - 1),
                    pending_frames,
                    notifiers: Vec::new(),
                    _marker: PhantomData,
                })),
            )]),
        );

        // A Neighbor advertisement should now update the entry.
        receive_ip_packet::<_, _, Ipv6>(
            &sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            FrameDestination::Multicast,
            na_packet_buf(true, true),
        );
        assert_neighbors::<Ipv6, _>(
            &sync_ctx,
            &link_device_id,
            HashMap::from([(
                neighbor_ip.into_specified(),
                NeighborState::Dynamic(DynamicNeighborState::Reachable(Reachable {
                    link_address: remote_mac.get(),
                    last_confirmed_at: non_sync_ctx.now(),
                })),
            )]),
        );
        let frames = non_sync_ctx.take_frames();
        let (got_device_id, got_frame) = assert_matches!(&frames[..], [x] => x);
        assert_eq!(got_device_id, &eth_device_id);

        let (payload, src_mac, dst_mac, ether_type) =
            parse_ethernet_frame(got_frame, EthernetFrameLengthCheck::NoCheck).unwrap();
        assert_eq!(src_mac, local_mac.get());
        assert_eq!(dst_mac, remote_mac.get());
        assert_eq!(ether_type, Some(EtherType::Ipv6));
        assert_eq!(payload, body);

        // Disabling the device should clear the neighbor table.
        Ipv6::set_ip_device_enabled(sync_ctx, &mut non_sync_ctx, &device_id, false, true);
        assert_neighbors::<Ipv6, _>(&sync_ctx, &link_device_id, HashMap::new());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
    }

    type FakeNudNetwork<L> = FakeNetwork<
        &'static str,
        EthernetDeviceId<testutil::FakeNonSyncCtx>,
        testutil::Ctx<testutil::FakeNonSyncCtx>,
        L,
    >;

    fn new_test_net<I: Ip + testutil::TestIpExt>() -> (
        FakeNudNetwork<
            impl FakeNetworkLinks<
                EthernetWeakDeviceId<testutil::FakeNonSyncCtx>,
                EthernetDeviceId<testutil::FakeNonSyncCtx>,
                &'static str,
            >,
        >,
        EthernetDeviceId<testutil::FakeNonSyncCtx>,
        EthernetDeviceId<testutil::FakeNonSyncCtx>,
    ) {
        let build_ctx = |config: FakeEventDispatcherConfig<I::Addr>| {
            let mut builder = testutil::FakeEventDispatcherBuilder::default();
            let device =
                builder.add_device_with_ip(config.local_mac, config.local_ip.get(), config.subnet);
            let (ctx, device_ids) = builder.build();
            (ctx, device_ids[device].clone())
        };

        let (local, local_device) = build_ctx(I::FAKE_CONFIG);
        let (remote, remote_device) = build_ctx(I::FAKE_CONFIG.swap());
        let net = crate::context::testutil::new_simple_fake_network(
            "local",
            local,
            local_device.downgrade(),
            "remote",
            remote,
            remote_device.downgrade(),
        );
        (net, local_device, remote_device)
    }

    fn bind_and_connect_sockets<
        I: Ip + testutil::TestIpExt,
        L: FakeNetworkLinks<
            EthernetWeakDeviceId<testutil::FakeNonSyncCtx>,
            EthernetDeviceId<testutil::FakeNonSyncCtx>,
            &'static str,
        >,
    >(
        net: &mut FakeNudNetwork<L>,
        local_buffers: tcp::buffer::testutil::ProvidedBuffers,
    ) -> tcp::socket::TcpSocketId<I, WeakDeviceId<testutil::FakeNonSyncCtx>, testutil::FakeNonSyncCtx>
    {
        const REMOTE_PORT: NonZeroU16 = const_unwrap::const_unwrap_option(NonZeroU16::new(33333));

        net.with_context("remote", |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
            let socket = tcp::socket::create_socket::<I, _>(
                sync_ctx,
                non_sync_ctx,
                tcp::buffer::testutil::ProvidedBuffers::default(),
            );
            tcp::socket::bind(
                sync_ctx,
                non_sync_ctx,
                &socket,
                Some(net_types::ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                Some(REMOTE_PORT),
            )
            .unwrap();
            tcp::socket::listen(sync_ctx, &socket, NonZeroUsize::new(1).unwrap()).unwrap();
        });

        net.with_context("local", |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
            let socket = tcp::socket::create_socket::<I, _>(sync_ctx, non_sync_ctx, local_buffers);
            tcp::socket::connect(
                sync_ctx,
                non_sync_ctx,
                &socket,
                Some(net_types::ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                REMOTE_PORT,
            )
            .unwrap();
            socket
        })
    }

    #[ip_test]
    fn upper_layer_confirmation_tcp_handshake<I: Ip + testutil::TestIpExt>()
    where
        for<'a> Locked<&'a SyncCtx<testutil::FakeNonSyncCtx>, lock_order::Unlocked>:
            DeviceIdContext<
                    EthernetLinkDevice,
                    DeviceId = EthernetDeviceId<testutil::FakeNonSyncCtx>,
                > + NudContext<I, EthernetLinkDevice, testutil::FakeNonSyncCtx>
                + BufferNudContext<Buf<Vec<u8>>, I, EthernetLinkDevice, testutil::FakeNonSyncCtx>,
        testutil::FakeNonSyncCtx: TimerContext<
            NudTimerId<I, EthernetLinkDevice, EthernetDeviceId<testutil::FakeNonSyncCtx>>,
        >,
    {
        let (mut net, local_device, remote_device) = new_test_net::<I>();

        let FakeEventDispatcherConfig { local_ip, local_mac, remote_mac, remote_ip, .. } =
            I::FAKE_CONFIG;

        // Insert a STALE neighbor in each node's neighbor table so that they don't
        // initiate neighbor resolution before performing the TCP handshake.
        for (ctx, device, neighbor, link_addr) in [
            ("local", local_device.clone(), remote_ip, remote_mac),
            ("remote", remote_device.clone(), local_ip, local_mac),
        ] {
            net.with_context(ctx, |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
                NudHandler::handle_neighbor_update(
                    &mut Locked::new(sync_ctx),
                    non_sync_ctx,
                    &device,
                    neighbor,
                    link_addr.get(),
                    DynamicNeighborUpdateSource::Probe,
                );
                super::testutil::assert_dynamic_neighbor_state(
                    &mut Locked::new(sync_ctx),
                    device.clone(),
                    neighbor,
                    DynamicNeighborState::Stale(Stale { link_address: link_addr.get() }),
                );
            });
        }

        // Initiate a TCP connection and make sure the SYN and resulting SYN/ACK are
        // received by each context.
        let _: tcp::socket::TcpSocketId<I, _, _> = bind_and_connect_sockets::<I, _>(
            &mut net,
            tcp::buffer::testutil::ProvidedBuffers::default(),
        );
        for _ in 0..2 {
            assert_eq!(
                net.step(crate::device::testutil::receive_frame, testutil::handle_timer)
                    .frames_sent,
                1
            );
        }

        // The three-way handshake should now be complete, and the neighbor should have
        // transitioned to REACHABLE.
        net.with_context("local", |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
            super::testutil::assert_dynamic_neighbor_state(
                &mut Locked::new(sync_ctx),
                local_device.clone(),
                remote_ip,
                DynamicNeighborState::Reachable(Reachable {
                    link_address: remote_mac.get(),
                    last_confirmed_at: non_sync_ctx.now(),
                }),
            );
        });

        // Remove the devices so that existing NUD timers get cleaned up; otherwise,
        // they would hold dangling references to the devices when the `SyncCtx`s are
        // dropped at the end of the test.
        for (ctx, device) in [("local", local_device), ("remote", remote_device)] {
            net.with_context(ctx, |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
                crate::testutil::clear_routes_and_remove_ethernet_device(
                    sync_ctx,
                    non_sync_ctx,
                    device,
                );
            });
        }
    }

    #[ip_test]
    fn upper_layer_confirmation_tcp_ack<I: Ip + testutil::TestIpExt>()
    where
        for<'a> Locked<&'a SyncCtx<testutil::FakeNonSyncCtx>, lock_order::Unlocked>:
            DeviceIdContext<
                    EthernetLinkDevice,
                    DeviceId = EthernetDeviceId<testutil::FakeNonSyncCtx>,
                > + NudContext<I, EthernetLinkDevice, testutil::FakeNonSyncCtx>,
        testutil::FakeNonSyncCtx: TimerContext<
            NudTimerId<I, EthernetLinkDevice, EthernetDeviceId<testutil::FakeNonSyncCtx>>,
        >,
    {
        let (mut net, local_device, remote_device) = new_test_net::<I>();

        let FakeEventDispatcherConfig { remote_mac, remote_ip, .. } = I::FAKE_CONFIG;

        // Initiate a TCP connection, allow the handshake to complete, and wait until
        // the neighbor entry goes STALE due to lack of traffic on the connection.
        let client_ends = tcp::buffer::testutil::WriteBackClientBuffers::default();
        let local_socket = bind_and_connect_sockets::<I, _>(
            &mut net,
            tcp::buffer::testutil::ProvidedBuffers::Buffers(client_ends.clone()),
        );
        net.run_until_idle(crate::device::testutil::receive_frame, testutil::handle_timer);
        net.with_context("local", |testutil::FakeCtx { sync_ctx, non_sync_ctx: _ }| {
            super::testutil::assert_dynamic_neighbor_state(
                &mut Locked::new(sync_ctx),
                local_device.clone(),
                remote_ip,
                DynamicNeighborState::Stale(Stale { link_address: remote_mac.get() }),
            );
        });

        // Send some data on the local socket and wait for it to be ACKed by the peer.
        let tcp::buffer::testutil::ClientBuffers { send, receive: _ } =
            client_ends.0.as_ref().lock().take().unwrap();
        send.lock().extend_from_slice(b"hello");
        net.with_context("local", |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
            tcp::socket::do_send(sync_ctx, non_sync_ctx, &local_socket);
        });
        for _ in 0..2 {
            assert_eq!(
                net.step(crate::device::testutil::receive_frame, testutil::handle_timer)
                    .frames_sent,
                1
            );
        }

        // The ACK should have been processed, and the neighbor should have transitioned
        // to REACHABLE.
        net.with_context("local", |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
            super::testutil::assert_dynamic_neighbor_state(
                &mut Locked::new(sync_ctx),
                local_device.clone(),
                remote_ip,
                DynamicNeighborState::Reachable(Reachable {
                    link_address: remote_mac.get(),
                    last_confirmed_at: non_sync_ctx.now(),
                }),
            );
        });

        // Remove the devices so that existing NUD timers get cleaned up; otherwise,
        // they would hold dangling references to the devices when the `SyncCtx`s are
        // dropped at the end of the test.
        for (ctx, device) in [("local", local_device), ("remote", remote_device)] {
            net.with_context(ctx, |testutil::FakeCtx { sync_ctx, non_sync_ctx }| {
                crate::testutil::clear_routes_and_remove_ethernet_device(
                    sync_ctx,
                    non_sync_ctx,
                    device,
                );
            });
        }
    }
}
