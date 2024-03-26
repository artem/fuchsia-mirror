// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements the DHCP client state machine.

use crate::deps::{self, DatagramInfo, Instant as _, Socket as _};
use crate::parse::{OptionCodeMap, OptionRequested};
use anyhow::Context as _;
use dhcp_protocol::{AtLeast, AtMostBytes, CLIENT_PORT, SERVER_PORT};
use futures::{
    channel::mpsc, pin_mut, select, FutureExt as _, Stream, StreamExt as _, TryStreamExt as _,
};
use net_types::{ethernet::Mac, SpecifiedAddr, Witness as _};
use rand::Rng as _;

use std::{
    fmt::{Debug, Display},
    net::Ipv4Addr,
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

/// Unexpected, non-recoverable errors encountered by the DHCP client.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Error encountered while performing a socket operation.
    #[error("error while using socket: {0:?}")]
    Socket(deps::SocketError),
}

/// The reason the DHCP client exited.
pub enum ExitReason {
    /// Executed due to a request for graceful shutdown.
    GracefulShutdown,
}

/// All possible core state machine states from the state-transition diagram in
/// [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Debug)]
pub enum State<I> {
    /// The default initial state of the state machine (no known
    /// currently-assigned IP address or DHCP server).
    Init(Init),
    /// The Selecting state (broadcasting DHCPDISCOVERs and receiving
    /// DHCPOFFERs).
    Selecting(Selecting<I>),
    /// The Requesting state (broadcasting DHCPREQUESTs and receiving DHCPACKs
    /// and DHCPNAKs).
    Requesting(Requesting<I>),
    /// The Bound state (we actively have a lease and are waiting to transition
    /// to Renewing).
    Bound(Bound<I>),
    /// The Renewing state (we actively have a lease that we are trying to
    /// renew by unicasting requests to our known DHCP server).
    Renewing(Renewing<I>),
    /// The Rebinding state (we actively have a lease that we are trying to
    /// renew by broadcasting requests to any DHCP server).
    Rebinding(Rebinding<I>),
    /// Waiting to restart the configuration process (via transitioning to Init).
    WaitingToRestart(WaitingToRestart<I>),
}

/// The next step to take after running the core state machine for one step.
pub enum Step<I> {
    /// Transition to another state.
    NextState(Transition<I>),
    /// Exit the client.
    Exit(ExitReason),
}

/// A state-transition to execute (see `State` enum variant documentation for a
/// description of each state).
pub enum Transition<I> {
    /// Transition to Init.
    Init(Init),
    /// Transition to Selecting.
    Selecting(Selecting<I>),
    /// Transition to Requesting.
    Requesting(Requesting<I>),
    /// Transition to Bound, having newly acquired a lease.
    BoundWithNewLease(Bound<I>, NewlyAcquiredLease<I>),
    /// Transition to Bound, having renewed a previously-acquired lease.
    BoundWithRenewedLease(Bound<I>, LeaseRenewal<I>),
    /// Transition to Renewing.
    Renewing(Renewing<I>),
    /// Transition to Rebinding.
    Rebinding(Rebinding<I>),
    /// Transition to wait to restart the configuration process.
    WaitingToRestart(WaitingToRestart<I>),
}

/// A side-effect of a state transition.
#[must_use]
#[derive(Debug)]
pub enum TransitionEffect<I> {
    /// Drop the existing lease.
    DropLease,
    /// Handle a newly-acquired lease.
    HandleNewLease(NewlyAcquiredLease<I>),
    /// Handle a renewed lease.
    HandleRenewedLease(LeaseRenewal<I>),
}

/// Outcome of handling an address rejection.
#[derive(Debug)]
pub enum AddressRejectionOutcome<I> {
    /// Observing an address rejection in this state should be impossible due to
    /// not having an active lease.
    ShouldBeImpossible,
    /// Transition to a new state.
    NextState(State<I>),
}

// Per RFC 2131 section 3.1, after sending a DHCPDECLINE message, "the client
// SHOULD wait a minimum of ten seconds before restarting the configuration
// process to avoid excessive network traffic in case of looping."
const WAIT_TIME_BEFORE_RESTARTING_AFTER_ADDRESS_REJECTION: Duration = Duration::from_secs(10);

impl<I: deps::Instant> State<I> {
    /// Run the client state machine for one "step".
    pub async fn run<C: deps::Clock<Instant = I>>(
        &self,
        config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        udp_socket_provider: &impl deps::UdpSocketProvider,
        rng: &mut impl deps::RngProvider,
        clock: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<Step<I>, Error> {
        let debug_log_prefix = &config.debug_log_prefix;
        match self {
            State::Init(init) => {
                Ok(Step::NextState(Transition::Selecting(init.do_init(rng, clock))))
            }
            State::Selecting(selecting) => match selecting
                .do_selecting(config, packet_socket_provider, rng, clock, stop_receiver)
                .await?
            {
                SelectingOutcome::GracefulShutdown => Ok(Step::Exit(ExitReason::GracefulShutdown)),
                SelectingOutcome::Requesting(requesting) => {
                    Ok(Step::NextState(Transition::Requesting(requesting)))
                }
            },
            State::Requesting(requesting) => {
                match requesting
                    .do_requesting(config, packet_socket_provider, rng, clock, stop_receiver)
                    .await?
                {
                    RequestingOutcome::RanOutOfRetransmits => {
                        tracing::info!(
                            "{debug_log_prefix} Returning to Init due to \
                            running out of DHCPREQUEST retransmits"
                        );
                        Ok(Step::NextState(Transition::Init(Init)))
                    }
                    RequestingOutcome::GracefulShutdown => {
                        Ok(Step::Exit(ExitReason::GracefulShutdown))
                    }
                    RequestingOutcome::Bound(bound, parameters) => {
                        let Bound {
                            discover_options: _,
                            yiaddr,
                            server_identifier: _,
                            ip_address_lease_time,
                            renewal_time: _,
                            rebinding_time: _,
                            start_time,
                        } = &bound;
                        let newly_acquired_lease = NewlyAcquiredLease {
                            ip_address: *yiaddr,
                            start_time: *start_time,
                            lease_time: *ip_address_lease_time,
                            parameters,
                        };
                        Ok(Step::NextState(Transition::BoundWithNewLease(
                            bound,
                            newly_acquired_lease,
                        )))
                    }
                    RequestingOutcome::Nak(nak) => {
                        // Per RFC 2131 section 3.1: "If the client receives a
                        // DHCPNAK message, the client restarts the
                        // configuration process."
                        tracing::warn!(
                            "{debug_log_prefix} Returning to Init due to DHCPNAK: {:?}",
                            nak
                        );
                        Ok(Step::NextState(Transition::Init(Init)))
                    }
                }
            }
            State::Bound(bound) => Ok(match bound.do_bound(config, clock, stop_receiver).await {
                BoundOutcome::GracefulShutdown => Step::Exit(ExitReason::GracefulShutdown),
                BoundOutcome::Renewing(renewing) => Step::NextState(Transition::Renewing(renewing)),
            }),
            State::Renewing(renewing) => {
                match renewing
                    .do_renewing(config, udp_socket_provider, clock, stop_receiver)
                    .await?
                {
                    RenewingOutcome::GracefulShutdown => {
                        Ok(Step::Exit(ExitReason::GracefulShutdown))
                    }
                    RenewingOutcome::Renewed(bound, parameters) => {
                        let Bound {
                            discover_options: _,
                            yiaddr: _,
                            server_identifier: _,
                            ip_address_lease_time,
                            renewal_time: _,
                            rebinding_time: _,
                            start_time,
                        } = &bound;
                        let lease_renewal = LeaseRenewal {
                            start_time: *start_time,
                            lease_time: *ip_address_lease_time,
                            parameters,
                        };
                        Ok(Step::NextState(Transition::BoundWithRenewedLease(bound, lease_renewal)))
                    }
                    RenewingOutcome::NewAddress(bound, parameters) => {
                        let Bound {
                            discover_options: _,
                            yiaddr,
                            server_identifier: _,
                            ip_address_lease_time,
                            renewal_time: _,
                            rebinding_time: _,
                            start_time,
                        } = &bound;
                        let new_lease = NewlyAcquiredLease {
                            ip_address: *yiaddr,
                            start_time: *start_time,
                            lease_time: *ip_address_lease_time,
                            parameters,
                        };
                        Ok(Step::NextState(Transition::BoundWithNewLease(bound, new_lease)))
                    }
                    RenewingOutcome::Rebinding(rebinding) => {
                        Ok(Step::NextState(Transition::Rebinding(rebinding)))
                    }
                    RenewingOutcome::Nak(nak) => {
                        // Per RFC 2131 section 3.1: "If the client receives a
                        // DHCPNAK message, the client restarts the
                        // configuration process."

                        let Renewing {
                            bound:
                                Bound {
                                    discover_options: _,
                                    yiaddr,
                                    server_identifier: _,
                                    ip_address_lease_time: _,
                                    start_time: _,
                                    renewal_time: _,
                                    rebinding_time: _,
                                },
                        } = renewing;
                        tracing::warn!(
                            "{debug_log_prefix} Dropping lease on {} \
                            and returning to Init due to DHCPNAK: {:?}",
                            yiaddr,
                            nak
                        );
                        Ok(Step::NextState(Transition::Init(Init)))
                    }
                }
            }
            State::Rebinding(rebinding) => {
                match rebinding
                    .do_rebinding(config, udp_socket_provider, clock, stop_receiver)
                    .await?
                {
                    RebindingOutcome::GracefulShutdown => {
                        Ok(Step::Exit(ExitReason::GracefulShutdown))
                    }
                    RebindingOutcome::Renewed(bound, parameters) => {
                        let Bound {
                            discover_options: _,
                            yiaddr: _,
                            server_identifier: _,
                            ip_address_lease_time,
                            renewal_time: _,
                            rebinding_time: _,
                            start_time,
                        } = &bound;
                        let renewal = LeaseRenewal {
                            start_time: *start_time,
                            lease_time: *ip_address_lease_time,
                            parameters,
                        };
                        Ok(Step::NextState(Transition::BoundWithRenewedLease(bound, renewal)))
                    }
                    RebindingOutcome::NewAddress(bound, parameters) => {
                        let Bound {
                            discover_options: _,
                            yiaddr,
                            server_identifier: _,
                            ip_address_lease_time,
                            renewal_time: _,
                            rebinding_time: _,
                            start_time,
                        } = &bound;
                        let new_lease = NewlyAcquiredLease {
                            ip_address: *yiaddr,
                            start_time: *start_time,
                            lease_time: *ip_address_lease_time,
                            parameters,
                        };
                        Ok(Step::NextState(Transition::BoundWithNewLease(bound, new_lease)))
                    }
                    RebindingOutcome::Nak(nak) => {
                        // Per RFC 2131 section 3.1: "If the client receives a
                        // DHCPNAK message, the client restarts the
                        // configuration process."

                        let Rebinding {
                            bound:
                                Bound {
                                    discover_options: _,
                                    yiaddr,
                                    server_identifier: _,
                                    ip_address_lease_time: _,
                                    start_time: _,
                                    renewal_time: _,
                                    rebinding_time: _,
                                },
                        } = rebinding;
                        tracing::warn!(
                            "{debug_log_prefix} Dropping lease on {} \
                            and returning to Init due to DHCPNAK: {:?}",
                            yiaddr,
                            nak
                        );
                        Ok(Step::NextState(Transition::Init(Init)))
                    }
                    RebindingOutcome::TimedOut => {
                        let Rebinding {
                            bound:
                                Bound {
                                    discover_options: _,
                                    yiaddr,
                                    server_identifier: _,
                                    ip_address_lease_time: _,
                                    start_time: _,
                                    renewal_time: _,
                                    rebinding_time: _,
                                },
                        } = rebinding;
                        tracing::warn!(
                            "{debug_log_prefix} Dropping lease on {} \
                            and returning to Init due to lease expiration",
                            yiaddr,
                        );
                        Ok(Step::NextState(Transition::Init(Init)))
                    }
                }
            }
            State::WaitingToRestart(waiting_to_restart) => {
                match waiting_to_restart.do_waiting_to_restart(clock, stop_receiver).await {
                    WaitingToRestartOutcome::GracefulShutdown => {
                        Ok(Step::Exit(ExitReason::GracefulShutdown))
                    }
                    WaitingToRestartOutcome::Init(init) => {
                        Ok(Step::NextState(Transition::Init(init)))
                    }
                }
            }
        }
    }

    /// Handles an acquired address being rejected.
    pub async fn on_address_rejection<C: deps::Clock<Instant = I>>(
        &self,
        config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        clock: &C,
        ip_address: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    ) -> Result<AddressRejectionOutcome<I>, Error> {
        let debug_log_prefix = &config.debug_log_prefix;
        match self {
            State::Init(_)
            | State::Selecting(_)
            | State::Requesting(_)
            | State::WaitingToRestart(_) => {
                tracing::warn!(
                    "{debug_log_prefix} received address rejection in state {}; ignoring",
                    self.state_name()
                );
                Ok(AddressRejectionOutcome::ShouldBeImpossible)
            }
            State::Bound(bound)
            | State::Renewing(Renewing { bound })
            | State::Rebinding(Rebinding { bound }) => {
                let Bound {
                    discover_options,
                    yiaddr,
                    server_identifier,
                    ip_address_lease_time: _,
                    start_time: _,
                    renewal_time: _,
                    rebinding_time: _,
                } = bound;

                if *yiaddr != ip_address {
                    tracing::warn!(
                        "{debug_log_prefix} received rejection of address {} while bound to \
                         different address {}; ignoring",
                        *yiaddr,
                        ip_address,
                    );
                    return Ok(AddressRejectionOutcome::ShouldBeImpossible);
                }

                let socket =
                    packet_socket_provider.get_packet_socket().await.map_err(Error::Socket)?;

                let message =
                    build_decline(config, discover_options, ip_address, *server_identifier);

                socket
                    .send_to(
                        crate::parse::serialize_dhcp_message_to_ip_packet(
                            message,
                            Ipv4Addr::UNSPECIFIED,
                            CLIENT_PORT,
                            Ipv4Addr::BROADCAST,
                            SERVER_PORT,
                        )
                        .as_ref(),
                        Mac::BROADCAST,
                    )
                    .await
                    .map_err(Error::Socket)?;

                tracing::info!(
                    "{debug_log_prefix} sent DHCPDECLINE for {}; waiting to restart",
                    ip_address
                );

                Ok(AddressRejectionOutcome::NextState(State::WaitingToRestart(WaitingToRestart {
                    waiting_until: clock
                        .now()
                        .add(WAIT_TIME_BEFORE_RESTARTING_AFTER_ADDRESS_REJECTION),
                })))
            }
        }
    }

    fn state_name(&self) -> &'static str {
        match self {
            State::Init(_) => "Init",
            State::Selecting(_) => "Selecting",
            State::Requesting(_) => "Requesting",
            State::Bound(_) => "Bound",
            State::Renewing(_) => "Renewing",
            State::Rebinding(_) => "Rebinding",
            State::WaitingToRestart(_) => "Waiting to Restart",
        }
    }

    fn has_lease(&self) -> bool {
        match self {
            State::Init(_) => false,
            State::Selecting(_) => false,
            State::Requesting(_) => false,
            State::Bound(_) => true,
            State::Renewing(_) => true,
            State::Rebinding(_) => true,
            State::WaitingToRestart(_) => false,
        }
    }

    /// Applies a state-transition to `self`, returning the next state and
    /// effects that need to be performed by bindings as a result of the transition.
    pub fn apply(
        &self,
        config: &ClientConfig,
        transition: Transition<I>,
    ) -> (State<I>, Option<TransitionEffect<I>>) {
        let debug_log_prefix = &config.debug_log_prefix;

        let (next_state, effect) = match transition {
            Transition::Init(init) => (State::Init(init), None),
            Transition::Selecting(selecting) => (State::Selecting(selecting), None),
            Transition::Requesting(requesting) => (State::Requesting(requesting), None),
            Transition::BoundWithRenewedLease(bound, lease_renewal) => {
                (State::Bound(bound), Some(TransitionEffect::HandleRenewedLease(lease_renewal)))
            }
            Transition::BoundWithNewLease(bound, new_lease) => {
                (State::Bound(bound), Some(TransitionEffect::HandleNewLease(new_lease)))
            }
            Transition::Renewing(renewing) => (State::Renewing(renewing), None),
            Transition::Rebinding(rebinding) => (State::Rebinding(rebinding), None),
            Transition::WaitingToRestart(waiting) => (State::WaitingToRestart(waiting), None),
        };

        tracing::info!(
            "{debug_log_prefix} transitioning from {} to {}",
            self.state_name(),
            next_state.state_name()
        );

        let effect = match effect {
            Some(effect) => Some(effect),
            None => match (self.has_lease(), next_state.has_lease()) {
                (true, false) => Some(TransitionEffect::DropLease),
                (false, true) => {
                    unreachable!("should already have decided on TransitionEffect::HandleNewLease")
                }
                (false, false) | (true, true) => None,
            },
        };
        (next_state, effect)
    }
}

impl<I> Default for State<I> {
    fn default() -> Self {
        State::Init(Init::default())
    }
}

/// Debug information to include in log messages about the client.
#[derive(Clone, Copy)]
pub struct DebugLogPrefix {
    /// The numerical interface ID the client is running on.
    pub interface_id: NonZeroU64,
}

impl Display for DebugLogPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { interface_id } = self;
        f.write_fmt(format_args!("(interface_id = {interface_id})"))
    }
}

/// Configuration for the DHCP client to be used while negotiating with DHCP
/// servers.
#[derive(Clone)]
pub struct ClientConfig {
    /// The hardware address of the interface on which the DHCP client is run.
    pub client_hardware_address: Mac,
    /// If set, a unique-on-the-local-network string to be used to identify this
    /// device while negotiating with DHCP servers.
    pub client_identifier:
        Option<AtLeast<2, AtMostBytes<{ dhcp_protocol::U8_MAX_AS_USIZE }, Vec<u8>>>>,
    /// Parameters to request from DHCP servers.
    pub requested_parameters: OptionCodeMap<OptionRequested>,
    /// If set, the preferred IP address lease time in seconds.
    pub preferred_lease_time_secs: Option<NonZeroU32>,
    /// If set, the IP address to request from DHCP servers.
    pub requested_ip_address: Option<SpecifiedAddr<net_types::ip::Ipv4Addr>>,
    /// Debug information to include in log messages about the client.
    pub debug_log_prefix: DebugLogPrefix,
}

#[derive(Clone, Debug, PartialEq)]
struct DiscoverOptions {
    xid: TransactionId,
}

/// Transaction ID for an exchange of DHCP messages.
///
/// Per [RFC 2131], "Transaction ID, a random number chosen by the client, used
/// by the client and server to associate messages and responses between a
/// client and a server."
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.3.1
#[derive(Clone, Copy, Debug, PartialEq)]
struct TransactionId(
    // While the DHCP RFC does not require that the XID be nonzero, it's helpful
    // to maintain that it is nonzero in order to make it clear that it is set
    // while debugging.
    NonZeroU32,
);

/// The initial state as depicted in the state-transition diagram in [RFC 2131].
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Default, Debug, PartialEq)]
pub struct Init;

impl Init {
    /// Generates a random transaction ID, records the starting time, and
    /// transitions to Selecting.
    fn do_init<C: deps::Clock>(
        &self,
        rng: &mut impl deps::RngProvider,
        clock: &C,
    ) -> Selecting<C::Instant> {
        let discover_options = DiscoverOptions {
            xid: TransactionId(NonZeroU32::new(rng.get_rng().gen_range(1..=u32::MAX)).unwrap()),
        };
        Selecting {
            discover_options,
            // Per RFC 2131 section 4.4.1, "The client records its own local time
            // for later use in computing the lease expiration" when it starts
            // sending DHCPDISCOVERs.
            start_time: clock.now(),
        }
    }
}

/// The state of waiting to restart the configuration process.
#[derive(Debug)]
pub struct WaitingToRestart<I> {
    waiting_until: I,
}

#[derive(Debug, PartialEq)]
enum WaitingToRestartOutcome {
    GracefulShutdown,
    Init(Init),
}

impl<I: deps::Instant> WaitingToRestart<I> {
    async fn do_waiting_to_restart<C: deps::Clock<Instant = I>>(
        &self,
        clock: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> WaitingToRestartOutcome {
        let Self { waiting_until } = self;
        let wait_fut = clock.wait_until(*waiting_until).fuse();
        pin_mut!(wait_fut);

        select! {
            () = wait_fut => WaitingToRestartOutcome::Init(Init::default()),
            () = stop_receiver.select_next_some() => WaitingToRestartOutcome::GracefulShutdown,
        }
    }
}

fn build_decline(
    client_config: &ClientConfig,
    discover_options: &DiscoverOptions,
    ip_address: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    server_identifier: SpecifiedAddr<net_types::ip::Ipv4Addr>,
) -> dhcp_protocol::Message {
    build_outgoing_message(
        client_config,
        discover_options,
        OutgoingOptions {
            ciaddr: None,
            requested_ip_address: Some(ip_address),
            ip_address_lease_time_secs: None,
            message_type: dhcp_protocol::MessageType::DHCPDECLINE,
            server_identifier: Some(server_identifier),
            include_parameter_request_list: false,
        },
    )
}

async fn send_with_retransmits<T: Clone + Send + Debug>(
    time: &impl deps::Clock,
    retransmit_schedule: impl IntoIterator<Item = Duration>,
    message: &[u8],
    socket: &impl deps::Socket<T>,
    dest: T,
    debug_log_prefix: DebugLogPrefix,
) -> Result<(), Error> {
    send_with_retransmits_at_instants(
        time,
        retransmit_schedule.into_iter().map(|duration| time.now().add(duration)),
        message,
        socket,
        dest,
        debug_log_prefix,
    )
    .await
}

async fn send_with_retransmits_at_instants<I: deps::Instant, T: Clone + Send + Debug>(
    time: &impl deps::Clock<Instant = I>,
    retransmit_schedule: impl IntoIterator<Item = I>,
    message: &[u8],
    socket: &impl deps::Socket<T>,
    dest: T,
    debug_log_prefix: DebugLogPrefix,
) -> Result<(), Error> {
    for wait_until in std::iter::once(None).chain(retransmit_schedule.into_iter().map(Some)) {
        if let Some(wait_until) = wait_until {
            time.wait_until(wait_until).await;
        }
        let result = socket.send_to(message, dest.clone()).await;
        match result {
            Ok(()) => (),
            Err(e) => match e {
                // We view these errors as non-recoverable, so we bubble them up
                // to bindings:
                deps::SocketError::FailedToOpen(_)
                | deps::SocketError::NoInterface
                | deps::SocketError::NetworkUnreachable
                | deps::SocketError::UnsupportedHardwareType => return Err(Error::Socket(e)),
                // We view EHOSTUNREACH as a recoverable error, as the desired
                // destination could only be temporarily offline, and this does
                // not necessarily indicate an issue with our own network stack.
                // Log a warning and continue retransmitting.
                deps::SocketError::HostUnreachable => {
                    tracing::warn!("{debug_log_prefix} destination host unreachable: {:?}", dest);
                }
                // For errors that we don't recognize, default to logging an
                // error and continuing to operate.
                deps::SocketError::Other(_) => {
                    tracing::error!(
                        "{debug_log_prefix} socket error while sending to {:?}: {:?}",
                        dest,
                        e
                    );
                }
            },
        }
    }
    Ok(())
}

fn retransmit_schedule_during_acquisition(
    rng: &mut (impl rand::Rng + ?Sized),
) -> impl Iterator<Item = Duration> + '_ {
    const MILLISECONDS_PER_SECOND: i32 = 1000;
    [4i32, 8, 16, 32]
        .into_iter()
        .chain(std::iter::repeat(64))
        // Per RFC 2131 Section 4.3.1, "the delay before the first
        // retransmission SHOULD be 4 seconds randomized by the value of a
        // uniform random number chosen from the range -1 to +1.  [...] The
        // delay before the next retransmission SHOULD be 8 seconds randomized
        // by the value of a uniform number chosen from the range -1 to +1.  The
        // retransmission delay SHOULD be doubled with subsequent
        // retransmissions up to a maximum of 64 seconds."
        .zip(std::iter::from_fn(|| {
            Some(rng.gen_range((-MILLISECONDS_PER_SECOND)..=MILLISECONDS_PER_SECOND))
        }))
        .map(|(base_seconds, jitter_millis)| {
            let millis = u64::try_from(base_seconds * MILLISECONDS_PER_SECOND + jitter_millis)
                .expect("retransmit wait is never negative");
            Duration::from_millis(millis)
        })
}

// This is assumed to be an appropriate buffer size due to Ethernet's common MTU
// of 1500 bytes.
const BUFFER_SIZE: usize = 1500;

fn recv_stream<'a, T: 'a, U: Send>(
    socket: &'a impl deps::Socket<U>,
    recv_buf: &'a mut [u8],
    parser: impl Fn(&[u8], U) -> T + 'a,
    debug_log_prefix: DebugLogPrefix,
) -> impl Stream<Item = Result<T, Error>> + 'a {
    futures::stream::try_unfold((recv_buf, parser), move |(recv_buf, parser)| async move {
        let result = socket.recv_from(recv_buf).await;
        let DatagramInfo { length, address } = match result {
            Ok(datagram_info) => datagram_info,
            Err(e) => match e {
                // We view these errors as non-recoverable, so we bubble them up
                // to bindings:
                deps::SocketError::FailedToOpen(_)
                | deps::SocketError::NoInterface
                | deps::SocketError::NetworkUnreachable
                | deps::SocketError::UnsupportedHardwareType => return Err(Error::Socket(e)),
                // We view EHOSTUNREACH as a recoverable error, as the server
                // we're communicating with could only be temporarily offline,
                // and this does not indicate an issue with our own network
                // stack. Log a warning and continue operating.
                // (While it seems like this ought not to be relevant while
                // receiving, there are instances where this could be observed,
                // like when IP_RECVERR is set on the socket and link resolution
                // fails, or as a result of an ICMP message.)
                deps::SocketError::HostUnreachable => {
                    tracing::warn!("{debug_log_prefix} EHOSTUNREACH from recv_from");
                    return Ok(Some((None, (recv_buf, parser))));
                }
                // For errors that we don't recognize, default to logging an
                // error and continuing to operate.
                deps::SocketError::Other(_) => {
                    tracing::error!("{debug_log_prefix} socket error while receiving: {:?}", e);
                    return Ok(Some((None, (recv_buf, parser))));
                }
            },
        };
        let raw_msg = &recv_buf[..length];
        let parsed = parser(raw_msg, address);
        Ok(Some((Some(parsed), (recv_buf, parser))))
    })
    .try_filter_map(|item| futures::future::ok(item))
}

struct OutgoingOptions {
    ciaddr: Option<SpecifiedAddr<net_types::ip::Ipv4Addr>>,
    requested_ip_address: Option<SpecifiedAddr<net_types::ip::Ipv4Addr>>,
    ip_address_lease_time_secs: Option<NonZeroU32>,
    message_type: dhcp_protocol::MessageType,
    server_identifier: Option<SpecifiedAddr<net_types::ip::Ipv4Addr>>,
    include_parameter_request_list: bool,
}

fn build_outgoing_message(
    ClientConfig {
        client_hardware_address,
        client_identifier,
        requested_parameters,
        preferred_lease_time_secs: _,
        requested_ip_address: _,
        debug_log_prefix: _,
    }: &ClientConfig,
    DiscoverOptions { xid: TransactionId(xid) }: &DiscoverOptions,
    OutgoingOptions {
        ciaddr,
        requested_ip_address,
        ip_address_lease_time_secs,
        message_type,
        server_identifier,
        include_parameter_request_list,
    }: OutgoingOptions,
) -> dhcp_protocol::Message {
    use dhcp_protocol::DhcpOption;

    dhcp_protocol::Message {
        op: dhcp_protocol::OpCode::BOOTREQUEST,
        xid: xid.get(),
        secs: 0,
        bdcast_flag: false,
        ciaddr: ciaddr.map(|ip| ip.get().into()).unwrap_or(Ipv4Addr::UNSPECIFIED),
        yiaddr: Ipv4Addr::UNSPECIFIED,
        siaddr: Ipv4Addr::UNSPECIFIED,
        giaddr: Ipv4Addr::UNSPECIFIED,
        chaddr: *client_hardware_address,
        sname: String::new(),
        file: String::new(),
        options: [
            requested_ip_address.map(|ip| DhcpOption::RequestedIpAddress(ip.get().into())),
            ip_address_lease_time_secs.map(|time| DhcpOption::IpAddressLeaseTime(time.get())),
            Some(DhcpOption::DhcpMessageType(message_type)),
            client_identifier.clone().map(DhcpOption::ClientIdentifier),
            server_identifier.map(|ip| DhcpOption::ServerIdentifier(ip.get().into())),
            include_parameter_request_list
                .then(|| requested_parameters.try_to_parameter_request_list())
                .flatten()
                .map(DhcpOption::ParameterRequestList),
        ]
        .into_iter()
        .flatten()
        .collect(),
    }
}

fn build_discover(
    client_config: &ClientConfig,
    discover_options: &DiscoverOptions,
) -> dhcp_protocol::Message {
    let ClientConfig {
        client_hardware_address: _,
        client_identifier: _,
        requested_parameters: _,
        preferred_lease_time_secs,
        requested_ip_address,
        debug_log_prefix: _,
    } = client_config;

    // Per the table in RFC 2131 section 4.4.1:
    //
    // 'ciaddr'                 0 (DHCPDISCOVER)
    // Requested IP Address     MAY (DISCOVER)
    // IP address lease time    MAY
    // DHCP message type        DHCPDISCOVER
    // Server Identifier        MUST NOT
    // Parameter Request List   MAY
    build_outgoing_message(
        client_config,
        discover_options,
        OutgoingOptions {
            ciaddr: None,
            requested_ip_address: *requested_ip_address,
            ip_address_lease_time_secs: *preferred_lease_time_secs,
            message_type: dhcp_protocol::MessageType::DHCPDISCOVER,
            server_identifier: None,
            include_parameter_request_list: true,
        },
    )
}

// Returns Ok(Some) if a DHCP message was successfully parsed, Ok(None) if the
// IP packet should just be discarded (but does not indicate an error), and
// Err if the IP packet indicates that some error should be logged.
fn parse_incoming_dhcp_message_from_ip_packet(
    packet: &[u8],
    debug_log_prefix: DebugLogPrefix,
) -> Result<Option<(net_types::ip::Ipv4Addr, dhcp_protocol::Message)>, anyhow::Error> {
    match crate::parse::parse_dhcp_message_from_ip_packet(packet, CLIENT_PORT) {
        Ok(message) => Ok(Some(message)),
        Err(err) => match err {
            crate::parse::ParseError::NotUdp => {
                tracing::debug!("{debug_log_prefix} ignoring non-UDP incoming packet");
                return Ok(None);
            }
            crate::parse::ParseError::WrongPort(port) => {
                tracing::debug!(
                    "{debug_log_prefix} ignoring incoming UDP packet \
                    to non-DHCP-client port {port}"
                );
                return Ok(None);
            }
            err @ (crate::parse::ParseError::Ipv4(_)
            | crate::parse::ParseError::Udp(_)
            | crate::parse::ParseError::WrongSource(_)
            | crate::parse::ParseError::Dhcp(_)) => {
                return Err(err).context("error while parsing DHCP message from IP packet");
            }
        },
    }
}

#[derive(Debug)]
pub(crate) enum SelectingOutcome<I> {
    GracefulShutdown,
    Requesting(Requesting<I>),
}

/// The Selecting state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Debug)]
pub struct Selecting<I> {
    discover_options: DiscoverOptions,
    // The time at which the DHCP transaction was initiated (used as the offset
    // from which lease expiration times are computed).
    start_time: I,
}

impl<I: deps::Instant> Selecting<I> {
    /// Executes the Selecting state.
    ///
    /// Transmits (and retransmits, if necessary) DHCPDISCOVER messages, and
    /// receives DHCPOFFER messages, on a packet socket. Tries to select a
    /// DHCPOFFER. If successful, transitions to Requesting.
    async fn do_selecting<C: deps::Clock<Instant = I>>(
        &self,
        client_config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        rng: &mut impl deps::RngProvider,
        time: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<SelectingOutcome<I>, Error> {
        // TODO(https://fxbug.dev/42075580): avoid dropping/recreating the packet
        // socket unnecessarily by taking an `&impl
        // deps::Socket<net_types::ethernet::Mac>` here instead.
        let socket = packet_socket_provider.get_packet_socket().await.map_err(Error::Socket)?;
        let Selecting { discover_options, start_time } = self;
        let message = build_discover(client_config, discover_options);

        let ClientConfig {
            client_hardware_address: _,
            client_identifier: _,
            requested_parameters,
            preferred_lease_time_secs: _,
            requested_ip_address: _,
            debug_log_prefix,
        } = client_config;

        let message = crate::parse::serialize_dhcp_message_to_ip_packet(
            message,
            Ipv4Addr::UNSPECIFIED, // src_ip
            CLIENT_PORT,
            Ipv4Addr::BROADCAST, // dst_ip
            SERVER_PORT,
        );

        let send_fut = send_with_retransmits(
            time,
            retransmit_schedule_during_acquisition(rng.get_rng()),
            message.as_ref(),
            &socket,
            /* dest= */ Mac::BROADCAST,
            *debug_log_prefix,
        )
        .fuse();

        let mut recv_buf = [0u8; BUFFER_SIZE];
        let offer_fields_stream = recv_stream(
            &socket,
            &mut recv_buf,
            |packet, src_addr| {
                // We don't care about the src addr of incoming offers, because we
                // identify DHCP servers via the Server Identifier option.
                let _: Mac = src_addr;

                let (src_addr, message) =
                    match parse_incoming_dhcp_message_from_ip_packet(packet, *debug_log_prefix)? {
                        Some(message) => message,
                        None => return Ok(None),
                    };
                validate_message(discover_options, client_config, &message)
                    .context("invalid DHCP message")?;
                crate::parse::fields_to_retain_from_selecting(requested_parameters, message)
                    .context(
                        "error while retrieving fields to use in DHCPREQUEST from DHCP message",
                    )
                    .map(|fields| Some((src_addr, fields)))
            },
            *debug_log_prefix,
        )
        .try_filter_map(|parse_result| {
            futures::future::ok(match parse_result {
                Ok(fields) => fields,
                Err(error) => {
                    tracing::warn!("{debug_log_prefix} discarding incoming packet: {:?}", error);
                    None
                }
            })
        })
        .fuse();

        pin_mut!(send_fut, offer_fields_stream);

        select! {
            send_discovers_result = send_fut => {
                send_discovers_result?;
                unreachable!("should never stop retransmitting DHCPDISCOVER unless we hit an error");
            },
            () = stop_receiver.select_next_some() => {
                Ok(SelectingOutcome::GracefulShutdown)
            },
            fields_to_use_in_request_result = offer_fields_stream.select_next_some() => {
                let (src_addr, fields_from_offer_to_use_in_request) =
                    fields_to_use_in_request_result?;

                if src_addr != fields_from_offer_to_use_in_request.server_identifier.get() {
                    tracing::warn!("{debug_log_prefix} received offer from {src_addr} with \
                        differing server_identifier = {}",
                        fields_from_offer_to_use_in_request.server_identifier);
                }

                // Currently, we take the naive approach of accepting the first
                // DHCPOFFER we see without doing any special selection logic.
                Ok(SelectingOutcome::Requesting(Requesting {
                    discover_options: discover_options.clone(),
                    fields_from_offer_to_use_in_request,
                    start_time: *start_time,
                }))
            }
        }
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
enum ValidateMessageError {
    #[error("xid {actual} doesn't match expected xid {expected}")]
    WrongXid { expected: u32, actual: u32 },
    #[error("chaddr {actual} doesn't match expected chaddr {expected}")]
    WrongChaddr { expected: Mac, actual: Mac },
}

fn validate_message(
    DiscoverOptions { xid: TransactionId(my_xid) }: &DiscoverOptions,
    ClientConfig {
        client_hardware_address: my_chaddr,
        client_identifier: _,
        requested_parameters: _,
        preferred_lease_time_secs: _,
        requested_ip_address: _,
        debug_log_prefix: _,
    }: &ClientConfig,
    dhcp_protocol::Message {
        op: _,
        xid: msg_xid,
        secs: _,
        bdcast_flag: _,
        ciaddr: _,
        yiaddr: _,
        siaddr: _,
        giaddr: _,
        chaddr: msg_chaddr,
        sname: _,
        file: _,
        options: _,
    }: &dhcp_protocol::Message,
) -> Result<(), ValidateMessageError> {
    if *msg_xid != u32::from(*my_xid) {
        return Err(ValidateMessageError::WrongXid { expected: my_xid.get(), actual: *msg_xid });
    }

    if msg_chaddr != my_chaddr {
        return Err(ValidateMessageError::WrongChaddr {
            expected: *my_chaddr,
            actual: *msg_chaddr,
        });
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
pub(crate) enum RequestingOutcome<I> {
    RanOutOfRetransmits,
    GracefulShutdown,
    Bound(Bound<I>, Vec<dhcp_protocol::DhcpOption>),
    Nak(crate::parse::FieldsToRetainFromNak),
}

/// The Requesting state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Debug, PartialEq)]
pub struct Requesting<I> {
    discover_options: DiscoverOptions,
    fields_from_offer_to_use_in_request: crate::parse::FieldsFromOfferToUseInRequest,
    start_time: I,
}

// Per RFC 2131, section 3.1: "a client retransmitting as described in section
// 4.1 might retransmit the DHCPREQUEST message four times, for a total delay of
// 60 seconds, before restarting the initialization procedure".
const NUM_REQUEST_RETRANSMITS: usize = 4;

impl<I: deps::Instant> Requesting<I> {
    /// Executes the Requesting state.
    ///
    /// Transmits (and retransmits, if necessary) DHCPREQUEST messages, and
    /// receives DHCPACK and DHCPNAK messages, on a packet socket. Upon
    /// receiving a DHCPACK, transitions to Bound.
    async fn do_requesting<C: deps::Clock<Instant = I>>(
        &self,
        client_config: &ClientConfig,
        packet_socket_provider: &impl deps::PacketSocketProvider,
        rng: &mut impl deps::RngProvider,
        time: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<RequestingOutcome<I>, Error> {
        let socket = packet_socket_provider.get_packet_socket().await.map_err(Error::Socket)?;
        let Requesting { discover_options, fields_from_offer_to_use_in_request, start_time } = self;
        let message = build_request_during_address_acquisition(
            client_config,
            discover_options,
            fields_from_offer_to_use_in_request,
        );

        let message = crate::parse::serialize_dhcp_message_to_ip_packet(
            message,
            Ipv4Addr::UNSPECIFIED, // src_ip
            CLIENT_PORT,
            Ipv4Addr::BROADCAST, // dst_ip
            SERVER_PORT,
        );

        let ClientConfig {
            client_hardware_address: _,
            client_identifier: _,
            requested_parameters,
            preferred_lease_time_secs: _,
            requested_ip_address: _,
            debug_log_prefix,
        } = client_config;

        let send_fut = send_with_retransmits(
            time,
            retransmit_schedule_during_acquisition(rng.get_rng()).take(NUM_REQUEST_RETRANSMITS),
            message.as_ref(),
            &socket,
            Mac::BROADCAST,
            *debug_log_prefix,
        )
        .fuse();

        let mut recv_buf = [0u8; BUFFER_SIZE];

        let ack_or_nak_stream = recv_stream(
            &socket,
            &mut recv_buf,
            |packet, src_addr| {
                // We don't care about the src addr of incoming messages, because we
                // identify DHCP servers via the Server Identifier option.
                let _: Mac = src_addr;
                let (src_addr, message) =
                    match parse_incoming_dhcp_message_from_ip_packet(packet, *debug_log_prefix)? {
                        Some(message) => message,
                        None => return Ok(None),
                    };
                validate_message(discover_options, client_config, &message)
                    .context("invalid DHCP message")?;

                crate::parse::fields_to_retain_from_response_to_request(
                    requested_parameters,
                    message,
                )
                .context("error extracting needed fields from DHCP message during Requesting")
                .map(|fields| Some((src_addr, fields)))
            },
            *debug_log_prefix,
        )
        .try_filter_map(|parse_result| {
            futures::future::ok(match parse_result {
                Ok(msg) => msg,
                Err(error) => {
                    tracing::warn!("{debug_log_prefix} discarding incoming packet: {:?}", error);
                    None
                }
            })
        })
        .fuse();

        pin_mut!(send_fut, ack_or_nak_stream);
        let (src_addr, fields_to_retain) = select! {
            send_requests_result = send_fut => {
                send_requests_result?;
                return Ok(RequestingOutcome::RanOutOfRetransmits)
            },
            () = stop_receiver.select_next_some() => {
                return Ok(RequestingOutcome::GracefulShutdown)
            },
            fields_to_retain_result = ack_or_nak_stream.select_next_some() => {
                fields_to_retain_result?
            }
        };

        match fields_to_retain {
            crate::parse::IncomingResponseToRequest::Ack(ack) => {
                let crate::parse::FieldsToRetainFromAck {
                    yiaddr,
                    server_identifier,
                    ip_address_lease_time_secs,
                    renewal_time_value_secs,
                    rebinding_time_value_secs,
                    parameters,
                } = ack;

                let server_identifier = server_identifier.unwrap_or({
                    let crate::parse::FieldsFromOfferToUseInRequest {
                        server_identifier,
                        ip_address_lease_time_secs: _,
                        ip_address_to_request: _,
                    } = fields_from_offer_to_use_in_request;
                    *server_identifier
                });

                if src_addr != server_identifier.get() {
                    tracing::warn!(
                        "{debug_log_prefix} accepting DHCPACK from {src_addr} \
                        with differing server_identifier = {server_identifier}"
                    );
                }
                Ok(RequestingOutcome::Bound(
                    Bound {
                        discover_options: discover_options.clone(),
                        yiaddr,
                        server_identifier,
                        ip_address_lease_time: Duration::from_secs(
                            ip_address_lease_time_secs.get().into(),
                        ),
                        renewal_time: renewal_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        rebinding_time: rebinding_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        start_time: *start_time,
                    },
                    parameters,
                ))
            }
            crate::parse::IncomingResponseToRequest::Nak(nak) => Ok(RequestingOutcome::Nak(nak)),
        }
    }
}

fn build_request_during_address_acquisition(
    client_config: &ClientConfig,
    discover_options: &DiscoverOptions,
    crate::parse::FieldsFromOfferToUseInRequest {
        server_identifier,
        ip_address_lease_time_secs,
        ip_address_to_request,
    }: &crate::parse::FieldsFromOfferToUseInRequest,
) -> dhcp_protocol::Message {
    let ClientConfig {
        client_hardware_address: _,
        client_identifier: _,
        requested_parameters: _,
        preferred_lease_time_secs,
        requested_ip_address: _,
        debug_log_prefix: _,
    } = client_config;

    // Per the table in RFC 2131 section 4.4.1:
    //
    // 'ciaddr'                 0 or client's network address (currently 0)
    // Requested IP Address     MUST (in SELECTING)
    // IP address lease time    MAY
    // DHCP message type        DHCPREQUEST
    // Server Identifier        MUST (after SELECTING)
    // Parameter Request List   MAY
    build_outgoing_message(
        client_config,
        discover_options,
        OutgoingOptions {
            ciaddr: None,
            requested_ip_address: Some(*ip_address_to_request),
            ip_address_lease_time_secs: ip_address_lease_time_secs.or(*preferred_lease_time_secs),
            message_type: dhcp_protocol::MessageType::DHCPREQUEST,
            server_identifier: Some(*server_identifier),
            include_parameter_request_list: true,
        },
    )
}

/// A newly-acquired DHCP lease.
#[derive(Debug, PartialEq)]
pub struct NewlyAcquiredLease<I> {
    /// The IP address acquired.
    pub ip_address: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    /// The start time of the lease.
    pub start_time: I,
    /// The length of the lease.
    pub lease_time: Duration,
    /// Configuration parameters acquired from the server. Guaranteed to be a
    /// subset of the parameters requested in the `parameter_request_list` in
    /// `ClientConfig`.
    pub parameters: Vec<dhcp_protocol::DhcpOption>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum BoundOutcome<I> {
    GracefulShutdown,
    Renewing(Renewing<I>),
}

/// The Bound state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Debug, PartialEq, Clone)]
pub struct Bound<I> {
    discover_options: DiscoverOptions,
    yiaddr: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    server_identifier: SpecifiedAddr<net_types::ip::Ipv4Addr>,
    ip_address_lease_time: Duration,
    start_time: I,
    renewal_time: Option<Duration>,
    rebinding_time: Option<Duration>,
}

impl<I: deps::Instant> Bound<I> {
    async fn do_bound<C: deps::Clock<Instant = I>>(
        &self,
        client_config: &ClientConfig,
        time: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> BoundOutcome<I> {
        let Self {
            discover_options: _,
            yiaddr: _,
            server_identifier,
            ip_address_lease_time,
            start_time,
            renewal_time,
            rebinding_time: _,
        } = self;

        // Per RFC 2131 section 4.4.5, "T1 defaults to (0.5 * duration_of_lease)".
        // (T1 is how the RFC refers to the time at which we transition to Renewing.)
        let renewal_time = renewal_time.unwrap_or(*ip_address_lease_time / 2);

        let debug_log_prefix = &client_config.debug_log_prefix;
        tracing::info!(
            "{debug_log_prefix} In Bound state; ip_address_lease_time = {}, renewal_time = {}, \
             server_identifier = {server_identifier}",
            ip_address_lease_time.as_secs(),
            renewal_time.as_secs(),
        );

        let renewal_timeout_fut = time.wait_until(start_time.add(renewal_time)).fuse();
        pin_mut!(renewal_timeout_fut);
        select! {
            () = renewal_timeout_fut => BoundOutcome::Renewing(Renewing { bound: self.clone() }),
            () = stop_receiver.select_next_some() => BoundOutcome::GracefulShutdown,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum RenewingOutcome<I> {
    GracefulShutdown,
    Renewed(Bound<I>, Vec<dhcp_protocol::DhcpOption>),
    // It might be surprising to see that it's possible to yield a _new_ address
    // while renewing, but per RFC 2131 section 4.4.5, "if the client is given a
    // new network address, it MUST NOT continue using the previous network
    // address and SHOULD notify the local users of the problem." This suggests
    // that we should be prepared for a DHCP server to send us a different
    // address from the one we asked for while renewing.
    NewAddress(Bound<I>, Vec<dhcp_protocol::DhcpOption>),
    Nak(crate::parse::FieldsToRetainFromNak),
    Rebinding(Rebinding<I>),
}

/// The Renewing state as depicted in the state-transition diagram in [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Debug, PartialEq)]
pub struct Renewing<I> {
    bound: Bound<I>,
}

// Per RFC 2131 section 4.4.5: "In both RENEWING and REBINDING states,
// if the client receives no response to its DHCPREQUEST message, the
// client SHOULD wait one-half of the remaining time until T2 (in
// RENEWING state) and one-half of the remaining lease time (in
// REBINDING state), down to a minimum of 60 seconds, before
// retransmitting the DHCPREQUEST message."
const RENEW_RETRANSMIT_MINIMUM_DELAY: Duration = Duration::from_secs(60);

impl<I: deps::Instant> Renewing<I> {
    async fn do_renewing<C: deps::Clock<Instant = I>>(
        &self,
        client_config: &ClientConfig,
        // TODO(https://fxbug.dev/42075580): avoid dropping/recreating the packet
        // socket unnecessarily by taking an `&impl
        // deps::Socket<std::net::SocketAddr>` here instead.
        udp_socket_provider: &impl deps::UdpSocketProvider,
        time: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<RenewingOutcome<I>, Error> {
        let renewal_start_time = time.now();
        let debug_log_prefix = client_config.debug_log_prefix;

        let Self {
            bound:
                bound @ Bound {
                    discover_options,
                    yiaddr,
                    server_identifier,
                    ip_address_lease_time,
                    start_time,
                    renewal_time: _,
                    rebinding_time,
                },
        } = self;
        let rebinding_time = rebinding_time.unwrap_or(*ip_address_lease_time / 8 * 7);
        let socket = udp_socket_provider
            .bind_new_udp_socket(std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                yiaddr.get().into(),
                CLIENT_PORT.get(),
            )))
            .await
            .map_err(Error::Socket)?;

        // Per the table in RFC 2131 section 4.4.1:
        //
        // 'ciaddr'                 client's network address (BOUND/RENEW/REBIND)
        // Requested IP Address     MUST NOT (in BOUND or RENEWING)
        // IP address lease time    MAY
        // DHCP message type        DHCPREQUEST
        // Server Identifier        MUST NOT (after INIT-REBOOT, BOUND, RENEWING or REBINDING)
        // Parameter Request List   MAY
        let message = build_outgoing_message(
            client_config,
            discover_options,
            OutgoingOptions {
                ciaddr: Some(*yiaddr),
                requested_ip_address: None,
                ip_address_lease_time_secs: client_config.preferred_lease_time_secs,
                message_type: dhcp_protocol::MessageType::DHCPREQUEST,
                server_identifier: None,
                include_parameter_request_list: true,
            },
        );
        let message_bytes = message.serialize();

        let t2 = start_time.add(rebinding_time);
        let server_sockaddr = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
            server_identifier.get().into(),
            SERVER_PORT.get(),
        ));
        let send_fut = send_with_retransmits_at_instants(
            time,
            std::iter::from_fn(|| {
                let now = time.now();
                let half_time_until_t2 = now.average(t2);
                Some(half_time_until_t2.max(now.add(RENEW_RETRANSMIT_MINIMUM_DELAY)))
            }),
            message_bytes.as_ref(),
            &socket,
            server_sockaddr,
            debug_log_prefix,
        )
        .fuse();

        let mut recv_buf = [0u8; BUFFER_SIZE];
        let responses_stream = recv_stream(
            &socket,
            &mut recv_buf,
            |packet, addr| {
                if addr != server_sockaddr {
                    return Err(anyhow::Error::from(crate::parse::ParseError::WrongSource(addr)));
                }
                let message = dhcp_protocol::Message::from_buffer(packet)
                    .map_err(crate::parse::ParseError::Dhcp)
                    .context("error while parsing DHCP message from UDP datagram")?;
                validate_message(discover_options, client_config, &message)
                    .context("invalid DHCP message")?;
                crate::parse::fields_to_retain_from_response_to_request(
                    &client_config.requested_parameters,
                    message,
                )
                .context("error extracting needed fields from DHCP message during Renewing")
            },
            debug_log_prefix,
        )
        .try_filter_map(|parse_result| {
            futures::future::ok(match parse_result {
                Ok(msg) => Some(msg),
                Err(error) => {
                    tracing::warn!("{debug_log_prefix} discarding incoming packet: {:?}", error);
                    None
                }
            })
        })
        .fuse();

        let timeout_fut = time.wait_until(t2).fuse();
        pin_mut!(send_fut, responses_stream, timeout_fut);

        let response = select! {
            () = timeout_fut => return Ok(RenewingOutcome::Rebinding(
                    Rebinding { bound: bound.clone() }
                )),
            () = stop_receiver.select_next_some() => return Ok(RenewingOutcome::GracefulShutdown),
            response = responses_stream.select_next_some() => {
                response?
            }
            send_result = send_fut => {
                return Err(send_result.expect_err("send_fut should never complete without error"))
            }
        };

        match response {
            crate::parse::IncomingResponseToRequest::Ack(ack) => {
                let crate::parse::FieldsToRetainFromAck {
                    yiaddr: new_yiaddr,
                    server_identifier: _,
                    ip_address_lease_time_secs,
                    renewal_time_value_secs,
                    rebinding_time_value_secs,
                    parameters,
                } = ack;
                let variant = if new_yiaddr == *yiaddr {
                    tracing::debug!(
                        "{debug_log_prefix} renewed with new lease time: {}",
                        ip_address_lease_time_secs
                    );
                    RenewingOutcome::Renewed
                } else {
                    tracing::info!(
                        "{debug_log_prefix} obtained different address from renewal: {}",
                        new_yiaddr
                    );
                    RenewingOutcome::NewAddress
                };
                Ok(variant(
                    Bound {
                        discover_options: discover_options.clone(),
                        yiaddr: new_yiaddr,
                        server_identifier: *server_identifier,
                        ip_address_lease_time: Duration::from_secs(
                            ip_address_lease_time_secs.get().into(),
                        ),
                        renewal_time: renewal_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        rebinding_time: rebinding_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        start_time: renewal_start_time,
                    },
                    parameters,
                ))
            }
            crate::parse::IncomingResponseToRequest::Nak(nak) => Ok(RenewingOutcome::Nak(nak)),
        }
    }
}

/// A renewal of a DHCP lease.
#[derive(Debug, PartialEq)]
pub struct LeaseRenewal<I> {
    /// The start time of the lease after renewal.
    pub start_time: I,
    /// The length of the lease after renewal.
    pub lease_time: Duration,
    /// Configuration parameters acquired from the server. Guaranteed to be a
    /// subset of the parameters requested in the `parameter_request_list` in
    /// `ClientConfig`.
    pub parameters: Vec<dhcp_protocol::DhcpOption>,
}

/// The Rebinding state as depicted in the state-transition diagram in
/// [RFC 2131].
///
/// [RFC 2131]: https://datatracker.ietf.org/doc/html/rfc2131#section-4.4
#[derive(Debug, PartialEq)]
pub struct Rebinding<I> {
    bound: Bound<I>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum RebindingOutcome<I> {
    GracefulShutdown,
    Renewed(Bound<I>, Vec<dhcp_protocol::DhcpOption>),
    // It might be surprising to see that it's possible to yield a _new_ address
    // while rebinding, but per RFC 2131 section 4.4.5, "if the client is given a
    // new network address, it MUST NOT continue using the previous network
    // address and SHOULD notify the local users of the problem." This suggests
    // that we should be prepared for a DHCP server to send us a different
    // address from the one we asked for while rebinding.
    NewAddress(Bound<I>, Vec<dhcp_protocol::DhcpOption>),
    Nak(crate::parse::FieldsToRetainFromNak),
    TimedOut,
}

impl<I: deps::Instant> Rebinding<I> {
    async fn do_rebinding<C: deps::Clock<Instant = I>>(
        &self,
        client_config: &ClientConfig,
        // TODO(https://fxbug.dev/42075580): avoid dropping/recreating the packet
        // socket unnecessarily by taking an `&impl
        // deps::Socket<std::net::SocketAddr>` here instead.
        udp_socket_provider: &impl deps::UdpSocketProvider,
        time: &C,
        stop_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<RebindingOutcome<I>, Error> {
        let rebinding_start_time = time.now();
        let debug_log_prefix = client_config.debug_log_prefix;

        let Self {
            bound:
                Bound {
                    discover_options,
                    yiaddr,
                    server_identifier: _,
                    ip_address_lease_time,
                    start_time,
                    renewal_time: _,
                    rebinding_time: _,
                },
        } = self;
        let socket = udp_socket_provider
            .bind_new_udp_socket(std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                yiaddr.get().into(),
                CLIENT_PORT.get(),
            )))
            .await
            .map_err(Error::Socket)?;

        // Per the table in RFC 2131 section 4.4.1:
        //
        // 'ciaddr'                 client's network address (BOUND/RENEW/REBIND)
        // Requested IP Address     MUST NOT (in BOUND or RENEWING)
        // IP address lease time    MAY
        // DHCP message type        DHCPREQUEST
        // Server Identifier        MUST NOT (after INIT-REBOOT, BOUND, RENEWING or REBINDING)
        // Parameter Request List   MAY
        let message = build_outgoing_message(
            client_config,
            discover_options,
            OutgoingOptions {
                ciaddr: Some(*yiaddr),
                requested_ip_address: None,
                ip_address_lease_time_secs: client_config.preferred_lease_time_secs,
                message_type: dhcp_protocol::MessageType::DHCPREQUEST,
                server_identifier: None,
                include_parameter_request_list: true,
            },
        );
        let message_bytes = message.serialize();

        let lease_expiry = start_time.add(*ip_address_lease_time);
        let server_sockaddr = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
            Ipv4Addr::BROADCAST,
            SERVER_PORT.get(),
        ));
        let send_fut = send_with_retransmits_at_instants(
            time,
            std::iter::from_fn(|| {
                let now = time.now();
                let half_time_until_lease_expiry = now.average(lease_expiry);
                Some(half_time_until_lease_expiry.max(now.add(RENEW_RETRANSMIT_MINIMUM_DELAY)))
            }),
            message_bytes.as_ref(),
            &socket,
            server_sockaddr,
            debug_log_prefix,
        )
        .fuse();

        let mut recv_buf = [0u8; BUFFER_SIZE];
        let responses_stream = recv_stream(
            &socket,
            &mut recv_buf,
            |packet, _addr| {
                let message = dhcp_protocol::Message::from_buffer(packet)
                    .map_err(crate::parse::ParseError::Dhcp)
                    .context("error while parsing DHCP message from UDP datagram")?;
                validate_message(discover_options, client_config, &message)
                    .context("invalid DHCP message")?;
                crate::parse::fields_to_retain_from_response_to_request(
                    &client_config.requested_parameters,
                    message,
                )
                .and_then(|response| match response {
                    crate::parse::IncomingResponseToRequest::Ack(ack) => {
                        // We need to enforce that DHCPACKs in REBINDING include
                        // a server identifier, as otherwise we won't know which
                        // server to send future renewal requests to.
                        Ok(crate::parse::IncomingResponseToRequest::Ack(
                            ack.map_server_identifier(|server_identifier| {
                                server_identifier.ok_or(
                                crate::parse::IncomingResponseToRequestError::NoServerIdentifier,
                            )
                            })?,
                        ))
                    }
                    crate::parse::IncomingResponseToRequest::Nak(nak) => {
                        Ok(crate::parse::IncomingResponseToRequest::Nak(nak))
                    }
                })
                .context("error extracting needed fields from DHCP message during Rebinding")
            },
            debug_log_prefix,
        )
        .try_filter_map(|parse_result| {
            futures::future::ok(match parse_result {
                Ok(msg) => Some(msg),
                Err(error) => {
                    tracing::warn!("{debug_log_prefix} discarding incoming packet: {:?}", error);
                    None
                }
            })
        })
        .fuse();

        let timeout_fut = time.wait_until(lease_expiry).fuse();
        pin_mut!(send_fut, responses_stream, timeout_fut);

        let response = select! {
            () = timeout_fut => return Ok(RebindingOutcome::TimedOut),
            () = stop_receiver.select_next_some() => return Ok(RebindingOutcome::GracefulShutdown),
            response = responses_stream.select_next_some() => {
                response?
            }
            send_result = send_fut => {
                return Err(send_result.expect_err("send_fut should never complete without error"))
            }
        };

        match response {
            crate::parse::IncomingResponseToRequest::Ack(ack) => {
                let crate::parse::FieldsToRetainFromAck {
                    yiaddr: new_yiaddr,
                    server_identifier,
                    ip_address_lease_time_secs,
                    renewal_time_value_secs,
                    rebinding_time_value_secs,
                    parameters,
                } = ack;
                let variant = if new_yiaddr == *yiaddr {
                    tracing::debug!(
                        "{debug_log_prefix} rebound with new lease time: {}",
                        ip_address_lease_time_secs
                    );
                    RebindingOutcome::Renewed
                } else {
                    tracing::info!(
                        "{debug_log_prefix} obtained different address from rebinding: {}",
                        new_yiaddr
                    );
                    RebindingOutcome::NewAddress
                };
                Ok(variant(
                    Bound {
                        discover_options: discover_options.clone(),
                        yiaddr: new_yiaddr,
                        server_identifier,
                        ip_address_lease_time: Duration::from_secs(
                            ip_address_lease_time_secs.get().into(),
                        ),
                        renewal_time: renewal_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        rebinding_time: rebinding_time_value_secs
                            .map(u64::from)
                            .map(Duration::from_secs),
                        start_time: rebinding_start_time,
                    },
                    parameters,
                ))
            }
            crate::parse::IncomingResponseToRequest::Nak(nak) => Ok(RebindingOutcome::Nak(nak)),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::deps::testutil::{
        advance, run_until_next_timers_fire, FakeRngProvider, FakeSocket, FakeSocketProvider,
        FakeTimeController,
    };
    use crate::deps::Clock as _;
    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use fuchsia_async as fasync;
    use futures::{join, Future};
    use itertools::Itertools as _;
    use net_declare::{net::prefix_length_v4, net_mac, std_ip_v4};
    use net_types::ip::{Ipv4, PrefixLength};
    use std::cell::RefCell;
    use std::rc::Rc;
    use test_case::test_case;

    fn initialize_logging() {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_max_level(tracing::Level::INFO)
            .finish();

        // Intentionally don't use the result here, since it'll succeed with the
        // first test case that calls it and fail with the others.
        let _: Result<_, _> = tracing::subscriber::set_global_default(subscriber);
    }

    const TEST_MAC_ADDRESS: Mac = net_mac!("01:01:01:01:01:01");
    const TEST_SERVER_MAC_ADDRESS: Mac = net_mac!("02:02:02:02:02:02");
    const OTHER_MAC_ADDRESS: Mac = net_mac!("03:03:03:03:03:03");

    const SERVER_IP: Ipv4Addr = std_ip_v4!("192.168.1.1");
    const OTHER_SERVER_IP: Ipv4Addr = std_ip_v4!("192.168.1.11");
    const YIADDR: Ipv4Addr = std_ip_v4!("198.168.1.5");
    const OTHER_ADDR: Ipv4Addr = std_ip_v4!("198.168.1.6");
    const DEFAULT_LEASE_LENGTH_SECONDS: u32 = 100;
    const MAX_LEASE_LENGTH_SECONDS: u32 = 200;
    const TEST_PREFIX_LENGTH: PrefixLength<Ipv4> = prefix_length_v4!(24);

    fn test_requested_parameters() -> OptionCodeMap<OptionRequested> {
        use dhcp_protocol::OptionCode;
        [
            (OptionCode::SubnetMask, OptionRequested::Required),
            (OptionCode::Router, OptionRequested::Optional),
            (OptionCode::DomainNameServer, OptionRequested::Optional),
        ]
        .into_iter()
        .collect::<OptionCodeMap<_>>()
    }

    fn test_parameter_values_excluding_subnet_mask() -> [dhcp_protocol::DhcpOption; 2] {
        [
            dhcp_protocol::DhcpOption::Router([SERVER_IP].into()),
            dhcp_protocol::DhcpOption::DomainNameServer([SERVER_IP, std_ip_v4!("8.8.8.8")].into()),
        ]
    }

    fn test_parameter_values() -> impl IntoIterator<Item = dhcp_protocol::DhcpOption> {
        std::iter::once(dhcp_protocol::DhcpOption::SubnetMask(TEST_PREFIX_LENGTH))
            .chain(test_parameter_values_excluding_subnet_mask())
    }

    fn test_client_config() -> ClientConfig {
        ClientConfig {
            client_hardware_address: TEST_MAC_ADDRESS,
            client_identifier: None,
            requested_parameters: test_requested_parameters(),
            preferred_lease_time_secs: None,
            requested_ip_address: None,
            debug_log_prefix: DebugLogPrefix { interface_id: NonZeroU64::new(2).unwrap() },
        }
    }

    #[test]
    fn do_init_uses_rng() {
        let mut rng = FakeRngProvider::new(0);
        let time = FakeTimeController::new();
        let arbitrary_start_time = std::time::Duration::from_secs(42);
        advance(&time, arbitrary_start_time);

        let Selecting {
            discover_options: DiscoverOptions { xid: xid_a },
            start_time: start_time_a,
        } = Init.do_init(&mut rng, &time);
        let Selecting {
            discover_options: DiscoverOptions { xid: xid_b },
            start_time: start_time_b,
        } = Init.do_init(&mut rng, &time);
        assert_ne!(xid_a, xid_b);
        assert_eq!(start_time_a, arbitrary_start_time);
        assert_eq!(start_time_b, arbitrary_start_time);
    }

    fn run_with_accelerated_time<F>(
        executor: &mut fasync::TestExecutor,
        time: &Rc<RefCell<FakeTimeController>>,
        main_future: &mut F,
    ) -> F::Output
    where
        F: Future + Unpin,
    {
        loop {
            match run_until_next_timers_fire(executor, time, main_future) {
                std::task::Poll::Ready(result) => break result,
                std::task::Poll::Pending => (),
            }
        }
    }

    fn build_test_selecting_state() -> Selecting<Duration> {
        Selecting {
            discover_options: DiscoverOptions {
                xid: TransactionId(const_unwrap_option(NonZeroU32::new(1))),
            },
            start_time: std::time::Duration::from_secs(0),
        }
    }

    #[test]
    fn do_selecting_obeys_graceful_shutdown() {
        initialize_logging();

        let mut executor = fasync::TestExecutor::new();
        let time = FakeTimeController::new();

        let selecting = build_test_selecting_state();
        let mut rng = FakeRngProvider::new(0);

        let (_server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (stop_sender, mut stop_receiver) = mpsc::unbounded();

        let selecting_fut = selecting
            .do_selecting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let time = &time;

        let wait_fut = async {
            // Wait some arbitrary amount of time to ensure `do_selecting` is waiting on a reply.
            // Note that this is fake time, not 30 actual seconds.
            time.wait_until(std::time::Duration::from_secs(30)).await;
        }
        .fuse();

        pin_mut!(selecting_fut, wait_fut);

        {
            let main_future = async {
                select! {
                    _ = selecting_fut => unreachable!("should keep retransmitting DHCPDISCOVER forever"),
                    () = wait_fut => (),
                }
            };
            pin_mut!(main_future);

            run_with_accelerated_time(&mut executor, time, &mut main_future);
        }

        stop_sender.unbounded_send(()).expect("sending stop signal should succeed");

        let selecting_result = selecting_fut.now_or_never().expect(
            "selecting_fut should complete after single poll after stop signal has been sent",
        );

        assert_matches!(selecting_result, Ok(SelectingOutcome::GracefulShutdown));
    }

    struct VaryingOutgoingMessageFields {
        xid: u32,
        options: Vec<dhcp_protocol::DhcpOption>,
    }

    #[track_caller]
    fn assert_outgoing_message_when_not_assigned_address(
        got_message: &dhcp_protocol::Message,
        fields: VaryingOutgoingMessageFields,
    ) {
        let VaryingOutgoingMessageFields { xid, options } = fields;
        let want_message = dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREQUEST,
            xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: TEST_MAC_ADDRESS,
            sname: String::new(),
            file: String::new(),
            options,
        };
        assert_eq!(got_message, &want_message);
    }

    #[test]
    fn do_selecting_sends_discover() {
        initialize_logging();

        let mut executor = fasync::TestExecutor::new();
        let time = FakeTimeController::new();

        let selecting = Selecting {
            discover_options: DiscoverOptions {
                xid: TransactionId(const_unwrap_option(NonZeroU32::new(1))),
            },
            start_time: std::time::Duration::from_secs(0),
        };
        let mut rng = FakeRngProvider::new(0);

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let selecting_fut = selecting
            .do_selecting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let time = &time;

        // These are the time ranges in which we expect to see messages from the
        // DHCP client. They are ranges in order to account for randomized
        // delays.
        const EXPECTED_RANGES: [(u64, u64); 7] =
            [(0, 0), (3, 5), (7, 9), (15, 17), (31, 33), (63, 65), (63, 65)];

        let receive_fut = async {
            let mut previous_time = std::time::Duration::from_secs(0);

            for (start, end) in EXPECTED_RANGES {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");

                assert_eq!(address, Mac::BROADCAST);

                let (_src_addr, msg) = crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    dhcp_protocol::SERVER_PORT,
                )
                .expect("received packet should parse as DHCP message");

                assert_outgoing_message_when_not_assigned_address(
                    &msg,
                    VaryingOutgoingMessageFields {
                        xid: msg.xid,
                        options: vec![
                            dhcp_protocol::DhcpOption::DhcpMessageType(
                                dhcp_protocol::MessageType::DHCPDISCOVER,
                            ),
                            dhcp_protocol::DhcpOption::ParameterRequestList(
                                test_requested_parameters()
                                    .iter_keys()
                                    .collect::<Vec<_>>()
                                    .try_into()
                                    .expect("should fit parameter request list size constraints"),
                            ),
                        ],
                    },
                );

                let received_time = time.now();

                let duration_range =
                    std::time::Duration::from_secs(start)..=std::time::Duration::from_secs(end);
                assert!(duration_range.contains(&(received_time - previous_time)));

                previous_time = received_time;
            }
        }
        .fuse();

        pin_mut!(selecting_fut, receive_fut);

        let main_future = async {
            select! {
                _ = selecting_fut => unreachable!("should keep retransmitting DHCPDISCOVER forever"),
                () = receive_fut => (),
            }
        };
        pin_mut!(main_future);

        run_with_accelerated_time(&mut executor, time, &mut main_future);
    }

    const XID: NonZeroU32 = const_unwrap_option(NonZeroU32::new(1));
    #[test_case(u32::from(XID), TEST_MAC_ADDRESS => Ok(()) ; "accepts good reply")]
    #[test_case(u32::from(XID), TEST_SERVER_MAC_ADDRESS => Err(
        ValidateMessageError::WrongChaddr {
            expected: TEST_MAC_ADDRESS,
            actual: TEST_SERVER_MAC_ADDRESS,
        }) ; "rejects wrong chaddr")]
    #[test_case(u32::from(XID).wrapping_add(1), TEST_MAC_ADDRESS => Err(
        ValidateMessageError::WrongXid {
            expected: u32::from(XID),
            actual: u32::from(XID).wrapping_add(1),
        }) ; "rejects wrong xid")]
    fn test_validate_message(
        message_xid: u32,
        message_chaddr: Mac,
    ) -> Result<(), ValidateMessageError> {
        let discover_options = DiscoverOptions { xid: TransactionId(XID) };
        let client_config = ClientConfig {
            client_hardware_address: TEST_MAC_ADDRESS,
            client_identifier: None,
            requested_parameters: test_requested_parameters(),
            preferred_lease_time_secs: None,
            requested_ip_address: None,
            debug_log_prefix: DebugLogPrefix { interface_id: NonZeroU64::new(2).unwrap() },
        };

        let reply = dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREPLY,
            xid: message_xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: message_chaddr,
            sname: String::new(),
            file: String::new(),
            options: Vec::new(),
        };

        validate_message(&discover_options, &client_config, &reply)
    }

    #[allow(clippy::unused_unit)]
    #[test_case(false ; "with no garbage traffic on link")]
    #[test_case(true ; "ignoring garbage replies to discover")]
    fn do_selecting_good_offer(reply_to_discover_with_garbage: bool) {
        initialize_logging();

        let mut rng = FakeRngProvider::new(0);
        let time = FakeTimeController::new();

        let arbitrary_start_time = std::time::Duration::from_secs(42);
        advance(&time, arbitrary_start_time);

        let selecting = Init.do_init(&mut rng, &time);
        let TransactionId(xid) = selecting.discover_options.xid;

        let (server_end, client_end) = FakeSocket::<Mac>::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let client_config = test_client_config();

        let selecting_fut = selecting
            .do_selecting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let server_fut = async {
            let mut recv_buf = [0u8; BUFFER_SIZE];

            if reply_to_discover_with_garbage {
                let DatagramInfo { length: _, address: dst_addr } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");
                assert_eq!(dst_addr, Mac::BROADCAST);

                server_end
                    .send_to(b"hello", OTHER_MAC_ADDRESS)
                    .await
                    .expect("send_to with garbage data should succeed");
            }

            let DatagramInfo { length, address } = server_end
                .recv_from(&mut recv_buf)
                .await
                .expect("recv_from on test socket should succeed");
            assert_eq!(address, Mac::BROADCAST);

            // `dhcp_protocol::Message` intentionally doesn't implement `Clone`,
            // so we re-parse instead for testing purposes.
            let parse_msg = || {
                let (_src_addr, msg) = crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    dhcp_protocol::SERVER_PORT,
                )
                .expect("received packet on test socket should parse as DHCP message");
                msg
            };

            let msg = parse_msg();
            assert_outgoing_message_when_not_assigned_address(
                &parse_msg(),
                VaryingOutgoingMessageFields {
                    xid: msg.xid,
                    options: vec![
                        dhcp_protocol::DhcpOption::DhcpMessageType(
                            dhcp_protocol::MessageType::DHCPDISCOVER,
                        ),
                        dhcp_protocol::DhcpOption::ParameterRequestList(
                            test_requested_parameters()
                                .iter_keys()
                                .collect::<Vec<_>>()
                                .try_into()
                                .expect("should fit parameter request list size constraints"),
                        ),
                    ],
                },
            );

            let build_reply = || {
                dhcpv4::server::build_offer(
                    parse_msg(),
                    dhcpv4::server::OfferOptions {
                        offered_ip: YIADDR,
                        server_ip: SERVER_IP,
                        lease_length_config: dhcpv4::configuration::LeaseLength {
                            default_seconds: DEFAULT_LEASE_LENGTH_SECONDS,
                            max_seconds: MAX_LEASE_LENGTH_SECONDS,
                        },
                        // The following fields don't matter for this test, as the
                        // client will read them from the DHCPACK rather than
                        // remembering them from the DHCPOFFER.
                        renewal_time_value: Some(20),
                        rebinding_time_value: Some(30),
                        subnet_mask: TEST_PREFIX_LENGTH,
                    },
                    &dhcpv4::server::options_repo(test_parameter_values()),
                )
                .expect("dhcp server crate error building offer")
            };

            let reply_with_wrong_xid = dhcp_protocol::Message {
                xid: (u32::from(xid).wrapping_add(1)),
                // Provide a different yiaddr in order to distinguish whether
                // the client correctly discarded this one, since we check which
                // `yiaddr` the client uses as its requested IP address later.
                yiaddr: OTHER_ADDR,
                ..build_reply()
            };

            let reply_without_subnet_mask = {
                let mut reply = build_reply();
                let options = std::mem::take(&mut reply.options);
                let (subnet_masks, other_options): (Vec<_>, Vec<_>) =
                    options.into_iter().partition_map(|option| match option {
                        dhcp_protocol::DhcpOption::SubnetMask(_) => itertools::Either::Left(option),
                        _ => itertools::Either::Right(option),
                    });
                assert_matches!(
                    &subnet_masks[..],
                    &[dhcp_protocol::DhcpOption::SubnetMask(TEST_PREFIX_LENGTH)]
                );
                reply.options = other_options;

                // Provide a different yiaddr in order to distinguish whether
                // the client correctly discarded this one, since we check which
                // `yiaddr` the client uses as its requested IP address later.
                reply.yiaddr = OTHER_ADDR;
                reply
            };

            let good_reply = build_reply();

            let send_reply = |reply: dhcp_protocol::Message| async {
                let dst_ip = reply.yiaddr;
                server_end
                    .send_to(
                        crate::parse::serialize_dhcp_message_to_ip_packet(
                            reply,
                            SERVER_IP,
                            SERVER_PORT,
                            dst_ip,
                            CLIENT_PORT,
                        )
                        .as_ref(),
                        // Note that this is the address the client under test
                        // observes in `recv_from`.
                        TEST_SERVER_MAC_ADDRESS,
                    )
                    .await
                    .expect("send_to on test socket should succeed");
            };

            // The DHCP client should ignore the reply with an incorrect xid.
            send_reply(reply_with_wrong_xid).await;

            // The DHCP client should ignore the reply without a subnet mask.
            send_reply(reply_without_subnet_mask).await;

            send_reply(good_reply).await;
        }
        .fuse();

        pin_mut!(selecting_fut, server_fut);

        let main_future = async move {
            let (selecting_result, ()) = join!(selecting_fut, server_fut);
            selecting_result
        }
        .fuse();
        pin_mut!(main_future);
        let mut executor = fasync::TestExecutor::new();
        let selecting_result = run_with_accelerated_time(&mut executor, &time, &mut main_future);

        let requesting = assert_matches!(
            selecting_result,
            Ok(SelectingOutcome::Requesting(requesting)) => requesting,
            "should have successfully transitioned to Requesting"
        );

        assert_eq!(
            requesting,
            Requesting {
                discover_options: DiscoverOptions { xid: requesting.discover_options.xid },
                fields_from_offer_to_use_in_request: crate::parse::FieldsFromOfferToUseInRequest {
                    server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
                        .try_into()
                        .expect("should be specified"),
                    ip_address_lease_time_secs: Some(const_unwrap_option(NonZeroU32::new(
                        DEFAULT_LEASE_LENGTH_SECONDS
                    ))),
                    ip_address_to_request: net_types::ip::Ipv4Addr::from(YIADDR)
                        .try_into()
                        .expect("should be specified"),
                },
                start_time: arbitrary_start_time,
            }
        );
    }

    const TEST_XID: TransactionId = TransactionId(const_unwrap_option(NonZeroU32::new(1)));
    const TEST_DISCOVER_OPTIONS: DiscoverOptions = DiscoverOptions { xid: TEST_XID };

    fn build_test_requesting_state() -> Requesting<std::time::Duration> {
        Requesting {
            discover_options: TEST_DISCOVER_OPTIONS,
            start_time: std::time::Duration::from_secs(0),
            fields_from_offer_to_use_in_request: crate::parse::FieldsFromOfferToUseInRequest {
                server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
                    .try_into()
                    .expect("should be specified"),
                ip_address_lease_time_secs: Some(const_unwrap_option(NonZeroU32::new(
                    DEFAULT_LEASE_LENGTH_SECONDS,
                ))),
                ip_address_to_request: net_types::ip::Ipv4Addr::from(YIADDR)
                    .try_into()
                    .expect("should be specified"),
            },
        }
    }

    #[test]
    fn do_requesting_obeys_graceful_shutdown() {
        initialize_logging();

        let time = FakeTimeController::new();

        let requesting = build_test_requesting_state();
        let mut rng = FakeRngProvider::new(0);

        let (_server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (stop_sender, mut stop_receiver) = mpsc::unbounded();

        let requesting_fut = requesting
            .do_requesting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();
        pin_mut!(requesting_fut);

        let mut executor = fasync::TestExecutor::new();
        assert_matches!(executor.run_until_stalled(&mut requesting_fut), std::task::Poll::Pending);

        stop_sender.unbounded_send(()).expect("sending stop signal should succeed");

        let requesting_result = requesting_fut.now_or_never().expect(
            "requesting_fut should complete after single poll after stop signal has been sent",
        );

        assert_matches!(requesting_result, Ok(RequestingOutcome::GracefulShutdown));
    }

    #[test]
    fn do_requesting_sends_requests() {
        initialize_logging();

        let mut executor = fasync::TestExecutor::new();
        let time = FakeTimeController::new();

        let requesting = build_test_requesting_state();
        let mut rng = FakeRngProvider::new(0);

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let requesting_fut = requesting
            .do_requesting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                &time,
                &mut stop_receiver,
            )
            .fuse();

        let time = &time;

        // These are the time ranges in which we expect to see messages from the
        // DHCP client. They are ranges in order to account for randomized
        // delays.
        const EXPECTED_RANGES: [(u64, u64); NUM_REQUEST_RETRANSMITS + 1] =
            [(0, 0), (3, 5), (7, 9), (15, 17), (31, 33)];

        let receive_fut = async {
            let mut previous_time = std::time::Duration::from_secs(0);

            for (start, end) in EXPECTED_RANGES {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");

                assert_eq!(address, Mac::BROADCAST);

                let (_src_addr, msg) = crate::parse::parse_dhcp_message_from_ip_packet(
                    &recv_buf[..length],
                    dhcp_protocol::SERVER_PORT,
                )
                .expect("received packet should parse as DHCP message");

                assert_outgoing_message_when_not_assigned_address(
                    &msg,
                    VaryingOutgoingMessageFields {
                        xid: msg.xid,
                        options: vec![
                            dhcp_protocol::DhcpOption::RequestedIpAddress(YIADDR),
                            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                                DEFAULT_LEASE_LENGTH_SECONDS,
                            ),
                            dhcp_protocol::DhcpOption::DhcpMessageType(
                                dhcp_protocol::MessageType::DHCPREQUEST,
                            ),
                            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
                            dhcp_protocol::DhcpOption::ParameterRequestList(
                                test_requested_parameters()
                                    .iter_keys()
                                    .collect::<Vec<_>>()
                                    .try_into()
                                    .expect("should fit parameter request list size constraints"),
                            ),
                        ],
                    },
                );

                let received_time = time.now();

                let duration_range =
                    std::time::Duration::from_secs(start)..=std::time::Duration::from_secs(end);
                assert!(duration_range.contains(&(received_time - previous_time)));

                previous_time = received_time;
            }
        }
        .fuse();

        pin_mut!(requesting_fut, receive_fut);

        let main_future = async { join!(requesting_fut, receive_fut) };
        pin_mut!(main_future);

        let (requesting_result, ()) =
            run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(requesting_result, Ok(RequestingOutcome::RanOutOfRetransmits));
    }

    struct VaryingIncomingMessageFields {
        yiaddr: Ipv4Addr,
        options: Vec<dhcp_protocol::DhcpOption>,
    }

    fn build_incoming_message(
        xid: u32,
        fields: VaryingIncomingMessageFields,
    ) -> dhcp_protocol::Message {
        let VaryingIncomingMessageFields { yiaddr, options } = fields;

        dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREPLY,
            xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: TEST_MAC_ADDRESS,
            sname: String::new(),
            file: String::new(),
            options,
        }
    }

    const NAK_MESSAGE: &str = "something went wrong";

    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RequestingOutcome::Bound(Bound {
        discover_options: TEST_DISCOVER_OPTIONS,
        yiaddr: net_types::ip::Ipv4Addr::from(YIADDR)
            .try_into()
            .expect("should be specified"),
        server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
            .try_into()
            .expect("should be specified"),
        ip_address_lease_time: std::time::Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
        renewal_time: None,
        rebinding_time: None,
        start_time: std::time::Duration::from_secs(0),
    }, test_parameter_values().into_iter().collect()) ; "transitions to Bound after receiving DHCPACK")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values_excluding_subnet_mask())
        .collect(),
    } => RequestingOutcome::RanOutOfRetransmits ; "ignores replies lacking required option SubnetMask")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: Ipv4Addr::UNSPECIFIED,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPNAK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::Message(NAK_MESSAGE.to_owned()),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RequestingOutcome::Nak(crate::parse::FieldsToRetainFromNak {
        server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
            .try_into()
            .expect("should be specified"),
        message: Some(NAK_MESSAGE.to_owned()),
        client_identifier: None,
    }) ; "transitions to Init after receiving DHCPNAK")]
    fn do_requesting_transitions_on_reply(
        incoming_message: VaryingIncomingMessageFields,
    ) -> RequestingOutcome<std::time::Duration> {
        initialize_logging();

        let time = &FakeTimeController::new();

        let requesting = build_test_requesting_state();
        let mut rng = FakeRngProvider::new(0);

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = FakeSocketProvider::new(client_end);

        let client_config = test_client_config();

        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();

        let requesting_fut = requesting
            .do_requesting(
                &client_config,
                &test_socket_provider,
                &mut rng,
                time,
                &mut stop_receiver,
            )
            .fuse();

        let server_fut = async {
            let mut recv_buf = [0u8; BUFFER_SIZE];

            let DatagramInfo { length, address } = server_end
                .recv_from(&mut recv_buf)
                .await
                .expect("recv_from on test socket should succeed");
            assert_eq!(address, Mac::BROADCAST);

            let (_src_addr, msg) = crate::parse::parse_dhcp_message_from_ip_packet(
                &recv_buf[..length],
                dhcp_protocol::SERVER_PORT,
            )
            .expect("received packet on test socket should parse as DHCP message");

            assert_outgoing_message_when_not_assigned_address(
                &msg,
                VaryingOutgoingMessageFields {
                    xid: msg.xid,
                    options: vec![
                        dhcp_protocol::DhcpOption::RequestedIpAddress(YIADDR),
                        dhcp_protocol::DhcpOption::IpAddressLeaseTime(DEFAULT_LEASE_LENGTH_SECONDS),
                        dhcp_protocol::DhcpOption::DhcpMessageType(
                            dhcp_protocol::MessageType::DHCPREQUEST,
                        ),
                        dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
                        dhcp_protocol::DhcpOption::ParameterRequestList(
                            test_requested_parameters()
                                .iter_keys()
                                .collect::<Vec<_>>()
                                .try_into()
                                .expect("should fit parameter request list size constraints"),
                        ),
                    ],
                },
            );

            let reply = build_incoming_message(msg.xid, incoming_message);

            server_end
                .send_to(
                    crate::parse::serialize_dhcp_message_to_ip_packet(
                        reply,
                        SERVER_IP,
                        SERVER_PORT,
                        YIADDR,
                        CLIENT_PORT,
                    )
                    .as_ref(),
                    // Note that this is the address the client under test
                    // observes in `recv_from`.
                    TEST_SERVER_MAC_ADDRESS,
                )
                .await
                .expect("send_to on test socket should succeed");
        }
        .fuse();

        pin_mut!(requesting_fut, server_fut);

        let main_future = async move {
            let (requesting_result, ()) = join!(requesting_fut, server_fut);
            requesting_result
        }
        .fuse();

        pin_mut!(main_future);

        let mut executor = fasync::TestExecutor::new();
        let requesting_result = run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(requesting_result, Ok(outcome) => outcome)
    }

    fn build_test_bound_state() -> Bound<std::time::Duration> {
        build_test_bound_state_with_times(
            Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
            None,
            None,
        )
    }

    fn build_test_bound_state_with_times(
        lease_length: Duration,
        renewal_time: Option<Duration>,
        rebinding_time: Option<Duration>,
    ) -> Bound<std::time::Duration> {
        Bound {
            discover_options: TEST_DISCOVER_OPTIONS,
            yiaddr: net_types::ip::Ipv4Addr::from(YIADDR).try_into().expect("should be specified"),
            server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
                .try_into()
                .expect("should be specified"),
            ip_address_lease_time: lease_length,
            start_time: std::time::Duration::from_secs(0),
            renewal_time,
            rebinding_time,
        }
    }

    fn build_test_newly_acquired_lease() -> NewlyAcquiredLease<Duration> {
        NewlyAcquiredLease {
            ip_address: net_types::ip::Ipv4Addr::from(YIADDR)
                .try_into()
                .expect("should be specified"),
            start_time: std::time::Duration::from_secs(0),
            lease_time: Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
            parameters: Vec::new(),
        }
    }

    #[test_case(
        (
            State::Init(Init::default()),
            Transition::BoundWithNewLease(build_test_bound_state(), build_test_newly_acquired_lease())
        ) => matches Some(TransitionEffect::HandleNewLease(_));
        "yields newly-acquired lease effect"
    )]
    #[test_case(
        (
            State::Bound(build_test_bound_state()),
            Transition::Init(Init),
        ) => matches Some(TransitionEffect::DropLease);
        "recognizes loss of lease"
    )]
    fn apply_transition(
        (state, transition): (State<Duration>, Transition<Duration>),
    ) -> Option<TransitionEffect<Duration>> {
        let (_next_state, effect) = state.apply(&test_client_config(), transition);
        effect
    }

    #[test_case(
        State::Init(Init),
        false;
        "should not have lease during Init"
    )]
    #[test_case(
        State::Selecting(build_test_selecting_state()),
        false;
        "should not have lease during Selecting"
    )]
    #[test_case(
        State::Requesting(build_test_requesting_state()),
        false;
        "should not have lease during Requesting"
    )]
    #[test_case(
        State::Bound(build_test_bound_state()),
        true;
        "should decline and restart during Bound"
    )]
    #[test_case(
        State::WaitingToRestart(WaitingToRestart { waiting_until: WAIT_TIME_BEFORE_RESTARTING_AFTER_ADDRESS_REJECTION }),
        false;
        "should not have lease during WaitingToRestart"
    )]
    fn on_address_rejection(state: State<Duration>, expect_decline: bool) {
        let config = &test_client_config();
        let time = &FakeTimeController::new();
        let (server_end, client_end) = FakeSocket::new_pair();
        let packet_socket_provider = FakeSocketProvider::new(client_end);
        let reject_fut = state
            .on_address_rejection(
                config,
                &packet_socket_provider,
                time,
                net_types::ip::Ipv4Addr::from(YIADDR).try_into().expect("should be specified"),
            )
            .fuse();
        pin_mut!(reject_fut);

        let mut executor = fasync::TestExecutor::new();
        let reject_result = run_with_accelerated_time(&mut executor, time, &mut reject_fut);

        if expect_decline {
            let WaitingToRestart { waiting_until } = assert_matches!(
                reject_result,
                Ok(AddressRejectionOutcome::NextState(
                    State::WaitingToRestart(waiting)
                )) => waiting
            );
            assert_eq!(waiting_until, Duration::from_secs(10));

            let mut buf = [0u8; BUFFER_SIZE];
            let DatagramInfo { length, address } = server_end
                .recv_from(&mut buf)
                .now_or_never()
                .expect("should be ready")
                .expect("should succeed");
            assert_eq!(address, Mac::BROADCAST);

            let (_src_addr, message) =
                crate::parse::parse_dhcp_message_from_ip_packet(&buf[..length], SERVER_PORT)
                    .expect("should succeed");

            use dhcp_protocol::DhcpOption;
            assert_outgoing_message_when_not_assigned_address(
                &message,
                VaryingOutgoingMessageFields {
                    xid: message.xid,
                    options: vec![
                        DhcpOption::RequestedIpAddress(YIADDR),
                        DhcpOption::DhcpMessageType(dhcp_protocol::MessageType::DHCPDECLINE),
                        DhcpOption::ServerIdentifier(SERVER_IP),
                    ],
                },
            );
        } else {
            assert_matches!(reject_result, Ok(AddressRejectionOutcome::ShouldBeImpossible));
        }
    }

    #[test]
    fn waiting_to_restart() {
        let time = &FakeTimeController::new();

        const WAITING_UNTIL: Duration = Duration::from_secs(10);

        // Set the start time to some arbitrary time below WAITING_UNTIL to show
        // that `WaitingToRestart` waits until an absolute time rather than for
        // a particular duration.
        advance(time, Duration::from_secs(3));

        let waiting = WaitingToRestart { waiting_until: WAITING_UNTIL };
        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();
        let main_fut = waiting.do_waiting_to_restart(time, &mut stop_receiver).fuse();
        pin_mut!(main_fut);
        let mut executor = fasync::TestExecutor::new();
        let outcome = run_with_accelerated_time(&mut executor, time, &mut main_fut);
        assert_eq!(outcome, WaitingToRestartOutcome::Init(Init));

        assert_eq!(time.now(), WAITING_UNTIL);
    }

    #[test_case(
        build_test_bound_state() =>
            Duration::from_secs(u64::from(DEFAULT_LEASE_LENGTH_SECONDS) / 2);
        "waits default renewal time when not specified")]
    #[test_case(
        Bound {
            renewal_time: Some(Duration::from_secs(10)),
            ..build_test_bound_state()
        } => Duration::from_secs(10);
        "waits specified renewal time")]
    fn bound_waits_for_renewal_time(bound: Bound<Duration>) -> Duration {
        let time = &FakeTimeController::new();
        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();
        let config = &test_client_config();
        let main_fut = bound.do_bound(config, time, &mut stop_receiver).fuse();
        pin_mut!(main_fut);
        let mut executor = fasync::TestExecutor::new();
        let outcome = run_with_accelerated_time(&mut executor, time, &mut main_fut);
        assert_eq!(outcome, BoundOutcome::Renewing(Renewing { bound: bound.clone() }));
        time.now()
    }

    #[test]
    fn bound_obeys_graceful_shutdown() {
        let time = &FakeTimeController::new();
        let (stop_sender, mut stop_receiver) = mpsc::unbounded();
        let bound = build_test_bound_state();
        let config = &test_client_config();
        let bound_fut = bound.do_bound(&config, time, &mut stop_receiver).fuse();

        stop_sender.unbounded_send(()).expect("send should succeed");
        assert_eq!(
            bound_fut.now_or_never().expect("should have completed"),
            BoundOutcome::GracefulShutdown
        );
    }

    fn build_test_renewing_state(
        lease_length: Duration,
        renewal_time: Option<Duration>,
        rebinding_time: Option<Duration>,
    ) -> Renewing<Duration> {
        Renewing {
            bound: build_test_bound_state_with_times(lease_length, renewal_time, rebinding_time),
        }
    }

    #[test]
    fn do_renewing_obeys_graceful_shutdown() {
        initialize_logging();

        let renewing = build_test_renewing_state(
            Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
            None,
            None,
        );
        let client_config = &test_client_config();

        let (_server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = &FakeSocketProvider::new(client_end);
        let (stop_sender, mut stop_receiver) = mpsc::unbounded();
        let time = &FakeTimeController::new();

        let renewing_fut = renewing
            .do_renewing(client_config, test_socket_provider, time, &mut stop_receiver)
            .fuse();
        pin_mut!(renewing_fut);

        let mut executor = fasync::TestExecutor::new();
        assert_matches!(executor.run_until_stalled(&mut renewing_fut), std::task::Poll::Pending);

        stop_sender.unbounded_send(()).expect("sending stop signal should succeed");

        let renewing_result = renewing_fut.now_or_never().expect(
            "renewing_fut should complete after single poll after stop signal has been sent",
        );

        assert_matches!(renewing_result, Ok(RenewingOutcome::GracefulShutdown));
    }

    #[track_caller]
    fn assert_outgoing_message_when_assigned_address(
        got_message: &dhcp_protocol::Message,
        fields: VaryingOutgoingMessageFields,
    ) {
        let VaryingOutgoingMessageFields { xid, options } = fields;
        let want_message = dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREQUEST,
            xid,
            secs: 0,
            bdcast_flag: false,
            ciaddr: YIADDR,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: TEST_MAC_ADDRESS,
            sname: String::new(),
            file: String::new(),
            options,
        };
        assert_eq!(got_message, &want_message);
    }

    #[test]
    fn do_renewing_sends_requests() {
        initialize_logging();

        // Just needs to be larger than rebinding time.
        const LEASE_LENGTH: Duration = Duration::from_secs(100000);

        // Set to have timestamps be conveniently derivable from powers of 2.
        const RENEWAL_TIME: Duration = Duration::from_secs(0);
        const REBINDING_TIME: Duration = Duration::from_secs(1024);

        let renewing =
            build_test_renewing_state(LEASE_LENGTH, Some(RENEWAL_TIME), Some(REBINDING_TIME));
        let client_config = &test_client_config();

        let (server_end, client_end) = FakeSocket::new_pair();
        let (binds_sender, mut binds_receiver) = mpsc::unbounded();
        let test_socket_provider = &FakeSocketProvider::new_with_events(client_end, binds_sender);
        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();
        let time = &FakeTimeController::new();

        let renewing_fut = renewing
            .do_renewing(client_config, test_socket_provider, time, &mut stop_receiver)
            .fuse();

        // Observe the "64, 4" instead of "64, 32" due to the 60 second minimum
        // retransmission delay.
        let expected_times_requests_are_sent =
            [1024, 512, 256, 128, 64, 4].map(|time_remaining_when_request_is_sent| {
                Duration::from_secs(1024 - time_remaining_when_request_is_sent)
            });

        let receive_fut = async {
            for expected_time in expected_times_requests_are_sent {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");

                assert_eq!(
                    address,
                    std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                        SERVER_IP,
                        SERVER_PORT.get()
                    ))
                );
                assert_eq!(time.now(), expected_time);
                let msg = dhcp_protocol::Message::from_buffer(&recv_buf[..length])
                    .expect("received packet should parse as DHCP message");

                assert_outgoing_message_when_assigned_address(
                    &msg,
                    VaryingOutgoingMessageFields {
                        xid: msg.xid,
                        options: vec![
                            dhcp_protocol::DhcpOption::DhcpMessageType(
                                dhcp_protocol::MessageType::DHCPREQUEST,
                            ),
                            dhcp_protocol::DhcpOption::ParameterRequestList(
                                test_requested_parameters()
                                    .iter_keys()
                                    .collect::<Vec<_>>()
                                    .try_into()
                                    .expect("should fit parameter request list size constraints"),
                            ),
                        ],
                    },
                );
            }
        }
        .fuse();

        pin_mut!(renewing_fut, receive_fut);

        let main_future = async { join!(renewing_fut, receive_fut) };
        pin_mut!(main_future);

        let mut executor = fasync::TestExecutor::new();
        let (requesting_result, ()) =
            run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(requesting_result, Ok(RenewingOutcome::Rebinding(_)));
        assert_matches!(server_end.recv_from(&mut []).now_or_never(), None);

        let bound_socket_addr = binds_receiver
            .next()
            .now_or_never()
            .expect("should have completed")
            .expect("should be present");
        assert_eq!(
            bound_socket_addr,
            std::net::SocketAddr::V4(std::net::SocketAddrV4::new(YIADDR, CLIENT_PORT.into()))
        );
    }

    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RenewingOutcome::Renewed(Bound {
        discover_options: TEST_DISCOVER_OPTIONS,
        yiaddr: net_types::ip::Ipv4Addr::from(YIADDR)
            .try_into()
            .expect("should be specified"),
        server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
            .try_into()
            .expect("should be specified"),
        ip_address_lease_time: std::time::Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
        renewal_time: None,
        rebinding_time: None,
        start_time: std::time::Duration::from_secs(0),
    }, test_parameter_values().into_iter().collect()) ; "successfully renews after receiving DHCPACK")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: OTHER_ADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RenewingOutcome::NewAddress(Bound {
        discover_options: TEST_DISCOVER_OPTIONS,
        yiaddr: net_types::ip::Ipv4Addr::from(OTHER_ADDR)
            .try_into()
            .expect("should be specified"),
        server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
            .try_into()
            .expect("should be specified"),
        ip_address_lease_time: std::time::Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
        renewal_time: None,
        rebinding_time: None,
        start_time: std::time::Duration::from_secs(0),
    }, test_parameter_values().into_iter().collect()) ; "observes new address from DHCPACK")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values_excluding_subnet_mask())
        .collect(),
    } => RenewingOutcome::Rebinding(
        Rebinding {
            bound: build_test_bound_state()
        }) ; "ignores replies lacking required option SubnetMask")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: Ipv4Addr::UNSPECIFIED,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPNAK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(SERVER_IP),
            dhcp_protocol::DhcpOption::Message(NAK_MESSAGE.to_owned()),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RenewingOutcome::Nak(crate::parse::FieldsToRetainFromNak {
        server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
            .try_into()
            .expect("should be specified"),
        message: Some(NAK_MESSAGE.to_owned()),
        client_identifier: None,
    }) ; "transitions to Init after receiving DHCPNAK")]
    fn do_renewing_transitions_on_reply(
        incoming_message: VaryingIncomingMessageFields,
    ) -> RenewingOutcome<std::time::Duration> {
        initialize_logging();

        let renewing = build_test_renewing_state(
            Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
            None,
            None,
        );
        let client_config = &test_client_config();

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = &FakeSocketProvider::new(client_end);
        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();
        let time = &FakeTimeController::new();

        let renewing_fut = renewing
            .do_renewing(client_config, test_socket_provider, time, &mut stop_receiver)
            .fuse();
        pin_mut!(renewing_fut);

        let server_socket_addr =
            std::net::SocketAddr::V4(std::net::SocketAddrV4::new(SERVER_IP, SERVER_PORT.into()));

        let server_fut = async {
            let mut recv_buf = [0u8; BUFFER_SIZE];

            let DatagramInfo { length, address } = server_end
                .recv_from(&mut recv_buf)
                .await
                .expect("recv_from on test socket should succeed");
            assert_eq!(address, server_socket_addr);

            let msg = dhcp_protocol::Message::from_buffer(&recv_buf[..length])
                .expect("received packet on test socket should parse as DHCP message");

            assert_outgoing_message_when_assigned_address(
                &msg,
                VaryingOutgoingMessageFields {
                    xid: msg.xid,
                    options: vec![
                        dhcp_protocol::DhcpOption::DhcpMessageType(
                            dhcp_protocol::MessageType::DHCPREQUEST,
                        ),
                        dhcp_protocol::DhcpOption::ParameterRequestList(
                            test_requested_parameters()
                                .iter_keys()
                                .collect::<Vec<_>>()
                                .try_into()
                                .expect("should fit parameter request list size constraints"),
                        ),
                    ],
                },
            );

            let reply = build_incoming_message(msg.xid, incoming_message);

            server_end
                .send_to(
                    &reply.serialize(),
                    // Note that this is the address the client under test
                    // observes in `recv_from`.
                    server_socket_addr,
                )
                .await
                .expect("send_to on test socket should succeed");
        }
        .fuse();

        pin_mut!(renewing_fut, server_fut);

        let main_future = async move {
            let (renewing_result, ()) = join!(renewing_fut, server_fut);
            renewing_result
        }
        .fuse();

        pin_mut!(main_future);

        let mut executor = fasync::TestExecutor::new();
        let renewing_result = run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(renewing_result, Ok(outcome) => outcome)
    }

    fn build_test_rebinding_state(
        lease_length: Duration,
        renewal_time: Option<Duration>,
        rebinding_time: Option<Duration>,
    ) -> Rebinding<Duration> {
        Rebinding {
            bound: build_test_bound_state_with_times(lease_length, renewal_time, rebinding_time),
        }
    }

    #[test]
    fn do_rebinding_sends_requests() {
        initialize_logging();

        // Set to have timestamps be conveniently derivable from powers of 2.
        const RENEWAL_TIME: Duration = Duration::from_secs(0);
        const REBINDING_TIME: Duration = Duration::from_secs(0);
        const LEASE_LENGTH: Duration = Duration::from_secs(1024);

        let rebinding =
            build_test_rebinding_state(LEASE_LENGTH, Some(RENEWAL_TIME), Some(REBINDING_TIME));
        let client_config = &test_client_config();

        let (server_end, client_end) = FakeSocket::new_pair();
        let (binds_sender, mut binds_receiver) = mpsc::unbounded();
        let test_socket_provider = &FakeSocketProvider::new_with_events(client_end, binds_sender);
        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();
        let time = &FakeTimeController::new();

        let rebinding_fut = rebinding
            .do_rebinding(client_config, test_socket_provider, time, &mut stop_receiver)
            .fuse();

        // Observe the "64, 4" instead of "64, 32" due to the 60 second minimum
        // retransmission delay.
        let expected_times_requests_are_sent =
            [1024, 512, 256, 128, 64, 4].map(|time_remaining_when_request_is_sent| {
                Duration::from_secs(1024 - time_remaining_when_request_is_sent)
            });

        let receive_fut = async {
            for expected_time in expected_times_requests_are_sent {
                let mut recv_buf = [0u8; BUFFER_SIZE];
                let DatagramInfo { length, address } = server_end
                    .recv_from(&mut recv_buf)
                    .await
                    .expect("recv_from on test socket should succeed");

                assert_eq!(
                    address,
                    std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                        std::net::Ipv4Addr::BROADCAST,
                        SERVER_PORT.get()
                    ))
                );
                assert_eq!(time.now(), expected_time);
                let msg = dhcp_protocol::Message::from_buffer(&recv_buf[..length])
                    .expect("received packet should parse as DHCP message");

                assert_outgoing_message_when_assigned_address(
                    &msg,
                    VaryingOutgoingMessageFields {
                        xid: msg.xid,
                        options: vec![
                            dhcp_protocol::DhcpOption::DhcpMessageType(
                                dhcp_protocol::MessageType::DHCPREQUEST,
                            ),
                            dhcp_protocol::DhcpOption::ParameterRequestList(
                                test_requested_parameters()
                                    .iter_keys()
                                    .collect::<Vec<_>>()
                                    .try_into()
                                    .expect("should fit parameter request list size constraints"),
                            ),
                        ],
                    },
                );
            }
        }
        .fuse();

        pin_mut!(rebinding_fut, receive_fut);

        let main_future = async { join!(rebinding_fut, receive_fut) };
        pin_mut!(main_future);

        let mut executor = fasync::TestExecutor::new();
        let (requesting_result, ()) =
            run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(requesting_result, Ok(RebindingOutcome::TimedOut));
        assert_matches!(server_end.recv_from(&mut []).now_or_never(), None);

        let bound_socket_addr = binds_receiver
            .next()
            .now_or_never()
            .expect("should have completed")
            .expect("should be present");
        assert_eq!(
            bound_socket_addr,
            std::net::SocketAddr::V4(std::net::SocketAddrV4::new(YIADDR, CLIENT_PORT.into()))
        );
    }

    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(OTHER_SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RebindingOutcome::Renewed(Bound {
        discover_options: TEST_DISCOVER_OPTIONS,
        yiaddr: net_types::ip::Ipv4Addr::from(YIADDR)
            .try_into()
            .expect("should be specified"),
        server_identifier: net_types::ip::Ipv4Addr::from(OTHER_SERVER_IP)
            .try_into()
            .expect("should be specified"),
        ip_address_lease_time: std::time::Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
        renewal_time: None,
        rebinding_time: None,
        start_time: std::time::Duration::from_secs(0),
    }, test_parameter_values().into_iter().collect());
    "successfully renews after receiving DHCPACK")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: OTHER_ADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(OTHER_SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RebindingOutcome::NewAddress(Bound {
        discover_options: TEST_DISCOVER_OPTIONS,
        yiaddr: net_types::ip::Ipv4Addr::from(OTHER_ADDR)
            .try_into()
            .expect("should be specified"),
        server_identifier: net_types::ip::Ipv4Addr::from(OTHER_SERVER_IP)
            .try_into()
            .expect("should be specified"),
        ip_address_lease_time: std::time::Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
        renewal_time: None,
        rebinding_time: None,
        start_time: std::time::Duration::from_secs(0),
    }, test_parameter_values().into_iter().collect()) ; "observes new address from DHCPACK")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: YIADDR,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPACK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(OTHER_SERVER_IP),
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(
                DEFAULT_LEASE_LENGTH_SECONDS,
            ),
        ]
        .into_iter()
        .chain(test_parameter_values_excluding_subnet_mask())
        .collect(),
    } => RebindingOutcome::TimedOut ; "ignores replies lacking required option SubnetMask")]
    #[test_case(VaryingIncomingMessageFields {
        yiaddr: Ipv4Addr::UNSPECIFIED,
        options: [
            dhcp_protocol::DhcpOption::DhcpMessageType(
                dhcp_protocol::MessageType::DHCPNAK,
            ),
            dhcp_protocol::DhcpOption::ServerIdentifier(OTHER_SERVER_IP),
            dhcp_protocol::DhcpOption::Message(NAK_MESSAGE.to_owned()),
        ]
        .into_iter()
        .chain(test_parameter_values())
        .collect(),
    } => RebindingOutcome::Nak(crate::parse::FieldsToRetainFromNak {
        server_identifier: net_types::ip::Ipv4Addr::from(OTHER_SERVER_IP)
            .try_into()
            .expect("should be specified"),
        message: Some(NAK_MESSAGE.to_owned()),
        client_identifier: None,
    }) ; "transitions to Init after receiving DHCPNAK")]
    fn do_rebinding_transitions_on_reply(
        incoming_message: VaryingIncomingMessageFields,
    ) -> RebindingOutcome<std::time::Duration> {
        initialize_logging();

        let rebinding = build_test_rebinding_state(
            Duration::from_secs(DEFAULT_LEASE_LENGTH_SECONDS.into()),
            None,
            None,
        );
        let client_config = &test_client_config();

        let (server_end, client_end) = FakeSocket::new_pair();
        let test_socket_provider = &FakeSocketProvider::new(client_end);
        let (_stop_sender, mut stop_receiver) = mpsc::unbounded();
        let time = &FakeTimeController::new();

        let rebinding_fut = rebinding
            .do_rebinding(client_config, test_socket_provider, time, &mut stop_receiver)
            .fuse();
        pin_mut!(rebinding_fut);

        let server_socket_addr = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
            OTHER_SERVER_IP,
            SERVER_PORT.into(),
        ));

        let server_fut = async {
            let mut recv_buf = [0u8; BUFFER_SIZE];

            let DatagramInfo { length, address } = server_end
                .recv_from(&mut recv_buf)
                .await
                .expect("recv_from on test socket should succeed");
            assert_eq!(
                address,
                std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                    std::net::Ipv4Addr::BROADCAST,
                    SERVER_PORT.into()
                ))
            );

            let msg = dhcp_protocol::Message::from_buffer(&recv_buf[..length])
                .expect("received packet on test socket should parse as DHCP message");

            assert_outgoing_message_when_assigned_address(
                &msg,
                VaryingOutgoingMessageFields {
                    xid: msg.xid,
                    options: vec![
                        dhcp_protocol::DhcpOption::DhcpMessageType(
                            dhcp_protocol::MessageType::DHCPREQUEST,
                        ),
                        dhcp_protocol::DhcpOption::ParameterRequestList(
                            test_requested_parameters()
                                .iter_keys()
                                .collect::<Vec<_>>()
                                .try_into()
                                .expect("should fit parameter request list size constraints"),
                        ),
                    ],
                },
            );

            let reply = build_incoming_message(msg.xid, incoming_message);

            server_end
                .send_to(
                    &reply.serialize(),
                    // Note that this is the address the client under test
                    // observes in `recv_from`.
                    server_socket_addr,
                )
                .await
                .expect("send_to on test socket should succeed");
        }
        .fuse();

        pin_mut!(rebinding_fut, server_fut);

        let main_future = async move {
            let (rebinding_result, ()) = join!(rebinding_fut, server_fut);
            rebinding_result
        }
        .fuse();

        pin_mut!(main_future);

        let mut executor = fasync::TestExecutor::new();
        let rebinding_result = run_with_accelerated_time(&mut executor, time, &mut main_future);

        assert_matches!(rebinding_result, Ok(outcome) => outcome)
    }
}
