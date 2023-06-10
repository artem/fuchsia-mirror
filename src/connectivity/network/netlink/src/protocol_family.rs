// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing protocol-specific aspects of Netlink.

use netlink_packet_core::{NetlinkMessage, NetlinkPayload, NetlinkSerializable};

use std::fmt::Debug;

// TODO(https://github.com/rust-lang/rust/issues/91611): Replace this with
// #![feature(async_fn_in_trait)] once it supports `Send` bounds. See
// https://blog.rust-lang.org/inside-rust/2023/05/03/stabilizing-async-fn-in-trait.html.
use async_trait::async_trait;
use tracing::{debug, warn};

use crate::{
    client::{ExternalClient, InternalClient},
    messaging::Sender,
    multicast_groups::{
        InvalidLegacyGroupsError, InvalidModernGroupError, LegacyGroups, ModernGroup,
        MulticastCapableNetlinkFamily,
    },
    NETLINK_LOG_TAG,
};

/// A type representing a Netlink Protocol Family.
pub(crate) trait ProtocolFamily:
    MulticastCapableNetlinkFamily + Send + Sized + 'static
{
    /// The message type associated with the protocol family.
    type InnerMessage: Clone + Debug + NetlinkSerializable + Send + 'static;
    /// The implementation for handling requests from this protocol family.
    type RequestHandler<S: Sender<Self::InnerMessage>>: NetlinkFamilyRequestHandler<Self, S>;

    const NAME: &'static str;
}

#[async_trait]
/// A request handler implementation for a particular Netlink protocol family.
pub(crate) trait NetlinkFamilyRequestHandler<F: ProtocolFamily, S: Sender<F::InnerMessage>>:
    Clone + Send + 'static
{
    /// Handles the given request and generates the associated response(s).
    async fn handle_request(
        &mut self,
        req: NetlinkMessage<F::InnerMessage>,
        client: &mut InternalClient<F, S>,
    );
}

pub mod route {
    //! This module implements the Route Netlink Protocol Family.

    use super::*;

    use std::{fmt::Display, num::NonZeroU32};

    use futures::{
        channel::{mpsc, oneshot},
        sink::SinkExt as _,
    };
    use net_types::ip::{
        AddrSubnetEither, AddrSubnetError, IpAddr, IpAddress as _, IpVersion, Ipv4Addr, Ipv6Addr,
    };

    use crate::{
        interfaces,
        netlink_packet::{new_ack, new_done, AckErrorCode, NackErrorCode},
        routes,
    };

    use netlink_packet_core::{NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::rtnl::{
        address::{nlas::Nla as AddressNla, AddressMessage},
        constants::{
            AF_INET, AF_INET6, AF_UNSPEC, RTNLGRP_DCB, RTNLGRP_DECNET_IFADDR, RTNLGRP_DECNET_ROUTE,
            RTNLGRP_DECNET_RULE, RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV4_MROUTE, RTNLGRP_IPV4_MROUTE_R,
            RTNLGRP_IPV4_NETCONF, RTNLGRP_IPV4_ROUTE, RTNLGRP_IPV4_RULE, RTNLGRP_IPV6_IFADDR,
            RTNLGRP_IPV6_IFINFO, RTNLGRP_IPV6_MROUTE, RTNLGRP_IPV6_MROUTE_R, RTNLGRP_IPV6_NETCONF,
            RTNLGRP_IPV6_PREFIX, RTNLGRP_IPV6_ROUTE, RTNLGRP_IPV6_RULE, RTNLGRP_LINK, RTNLGRP_MDB,
            RTNLGRP_MPLS_NETCONF, RTNLGRP_MPLS_ROUTE, RTNLGRP_ND_USEROPT, RTNLGRP_NEIGH,
            RTNLGRP_NONE, RTNLGRP_NOP2, RTNLGRP_NOP4, RTNLGRP_NOTIFY, RTNLGRP_NSID,
            RTNLGRP_PHONET_IFADDR, RTNLGRP_PHONET_ROUTE, RTNLGRP_TC,
        },
        RtnlMessage,
    };

    /// An implementation of the Netlink Route protocol family.
    pub(crate) enum NetlinkRoute {}

    impl MulticastCapableNetlinkFamily for NetlinkRoute {
        fn is_valid_group(ModernGroup(group): &ModernGroup) -> bool {
            match *group {
                RTNLGRP_DCB
                | RTNLGRP_DECNET_IFADDR
                | RTNLGRP_DECNET_ROUTE
                | RTNLGRP_DECNET_RULE
                | RTNLGRP_IPV4_IFADDR
                | RTNLGRP_IPV4_MROUTE
                | RTNLGRP_IPV4_MROUTE_R
                | RTNLGRP_IPV4_NETCONF
                | RTNLGRP_IPV4_ROUTE
                | RTNLGRP_IPV4_RULE
                | RTNLGRP_IPV6_IFADDR
                | RTNLGRP_IPV6_IFINFO
                | RTNLGRP_IPV6_MROUTE
                | RTNLGRP_IPV6_MROUTE_R
                | RTNLGRP_IPV6_NETCONF
                | RTNLGRP_IPV6_PREFIX
                | RTNLGRP_IPV6_ROUTE
                | RTNLGRP_IPV6_RULE
                | RTNLGRP_LINK
                | RTNLGRP_MDB
                | RTNLGRP_MPLS_NETCONF
                | RTNLGRP_MPLS_ROUTE
                | RTNLGRP_ND_USEROPT
                | RTNLGRP_NEIGH
                | RTNLGRP_NONE
                | RTNLGRP_NOP2
                | RTNLGRP_NOP4
                | RTNLGRP_NOTIFY
                | RTNLGRP_NSID
                | RTNLGRP_PHONET_IFADDR
                | RTNLGRP_PHONET_ROUTE
                | RTNLGRP_TC => true,
                _ => false,
            }
        }
    }

    impl ProtocolFamily for NetlinkRoute {
        type InnerMessage = RtnlMessage;
        type RequestHandler<S: Sender<Self::InnerMessage>> = NetlinkRouteRequestHandler<S>;

        const NAME: &'static str = "NETLINK_ROUTE";
    }

    #[derive(Clone)]
    pub(crate) struct NetlinkRouteRequestHandler<
        S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>,
    > {
        pub(crate) interfaces_request_sink: mpsc::Sender<interfaces::Request<S>>,
        #[allow(unused)]
        pub(crate) v4_routes_request_sink: mpsc::Sender<routes::Request<S>>,
        #[allow(unused)]
        pub(crate) v6_routes_request_sink: mpsc::Sender<routes::Request<S>>,
    }

    fn extract_if_id_and_addr_from_addr_message(
        message: &AddressMessage,
        client: &impl Display,
        req: &RtnlMessage,
    ) -> Option<interfaces::AddressAndInterfaceArgs> {
        let interface_id = match NonZeroU32::new(message.header.index) {
            Some(interface_id) => interface_id,
            None => {
                debug!(
                    "unspecified interface ID in address add request from {}: {:?}",
                    client, req,
                );
                return None;
            }
        };

        let address_bytes = message.nlas.iter().find_map(|nla| match nla {
            AddressNla::Local(bytes) => Some(bytes),
            nla => {
                warn!(
                    "unexpected Address NLA in add request from {}: {:?}; req = {:?}",
                    client, nla, req,
                );
                None
            }
        });
        let address_bytes = match address_bytes {
            Some(address_bytes) => address_bytes,
            None => {
                debug!("missing `Local` NLA in address add request from {}: {:?}", client, req);
                return None;
            }
        };

        let addr = match message.header.family.into() {
            AF_INET => {
                const BYTES: usize = Ipv4Addr::BYTES as usize;
                if address_bytes.len() < BYTES {
                    return None;
                }

                let mut bytes = [0; BYTES as usize];
                bytes.copy_from_slice(&address_bytes[..BYTES]);
                IpAddr::V4(bytes.into())
            }
            AF_INET6 => {
                const BYTES: usize = Ipv6Addr::BYTES as usize;
                if address_bytes.len() < BYTES {
                    return None;
                }

                let mut bytes = [0; BYTES];
                bytes.copy_from_slice(&address_bytes[..BYTES]);
                IpAddr::V6(bytes.into())
            }
            family => {
                debug!(
                    "invalid address family ({}) in address add request from {}: {:?}",
                    family, client, req,
                );
                return None;
            }
        };

        let address = match AddrSubnetEither::new(addr, message.header.prefix_len) {
            Ok(address) => address,
            Err(
                AddrSubnetError::PrefixTooLong
                | AddrSubnetError::NotUnicastInSubnet
                | AddrSubnetError::InvalidWitness,
            ) => {
                debug!("invalid address in address add request from {}: {:?}", client, req);
                return None;
            }
        };

        Some(interfaces::AddressAndInterfaceArgs { address, interface_id })
    }

    #[async_trait]
    impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>
        NetlinkFamilyRequestHandler<NetlinkRoute, S> for NetlinkRouteRequestHandler<S>
    {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<RtnlMessage>,
            client: &mut InternalClient<NetlinkRoute, S>,
        ) {
            let Self {
                interfaces_request_sink,
                v4_routes_request_sink: _v4_routes_request_sink,
                v6_routes_request_sink: _v6_routes_request_sink,
            } = self;

            let (req_header, payload) = req.into_parts();
            let req = match payload {
                NetlinkPayload::InnerMessage(p) => p,
                p => {
                    warn!(
                        tag = NETLINK_LOG_TAG,
                        "Ignoring request from client {} with unexpected payload: {:?}", client, p
                    );
                    return;
                }
            };

            let is_dump = req_header.flags & NLM_F_DUMP == NLM_F_DUMP;
            let is_ack = req_header.flags & NLM_F_ACK == NLM_F_ACK;

            use RtnlMessage::*;
            match req {
                GetLink(_) if is_dump => {
                    let (completer, waiter) = oneshot::channel();
                    interfaces_request_sink.send(interfaces::Request {
                        args: interfaces::RequestArgs::Link(
                            interfaces::LinkRequestArgs::Get(
                                interfaces::GetLinkArgs::Dump,
                            ),
                        ),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    waiter
                        .await
                        .expect("interfaces event loop should have handled the request")
                        .expect("link dump requests are infallible");
                    client.send(new_done(req_header))
                }
                GetAddress(ref message) if is_dump => {
                    let ip_version_filter = match message.header.family.into() {
                        AF_UNSPEC => None,
                        AF_INET => Some(IpVersion::V4),
                        AF_INET6 => Some(IpVersion::V6),
                        family => {
                            debug!(
                                "invalid address family ({}) in address dump request from {}: {:?}",
                                family, client, req,
                            );
                            client.send(new_ack(NackErrorCode::INVALID, req_header));
                            return;
                        }
                    };

                    let (completer, waiter) = oneshot::channel();
                    interfaces_request_sink.send(interfaces::Request {
                        args: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::Get(
                                interfaces::GetAddressArgs::Dump {
                                    ip_version_filter,
                                },
                            ),
                        ),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    waiter
                        .await
                        .expect("interfaces event loop should have handled the request")
                        .expect("addr dump requests are infallible");
                    client.send(new_done(req_header))
                }
                NewAddress(ref message) => {
                    let address_and_interface_id = match extract_if_id_and_addr_from_addr_message(
                        message,
                        client,
                        &req,
                    ) {
                        Some(s) => s,
                        None => {
                            return client.send(new_ack(NackErrorCode::INVALID, req_header));
                        }
                    };

                    let (completer, waiter) = oneshot::channel();
                    interfaces_request_sink.send(interfaces::Request {
                        args: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::New(
                                interfaces::NewAddressArgs {
                                    address_and_interface_id,
                                },
                            ),
                        ),
                        sequence_number: req_header.sequence_number,
                        client: client.clone(),
                        completer,
                    }).await.expect("interface event loop should never terminate");
                    let result = waiter
                        .await
                        .expect("interfaces event loop should have handled the request");
                    if is_ack || result.is_err() {
                        client.send(new_ack(result, req_header))
                    }
                }
                NewLink(_)
                | DelLink(_)
                | NewLinkProp(_)
                | DelLinkProp(_)
                | NewNeighbourTable(_)
                | SetNeighbourTable(_)
                | NewTrafficClass(_)
                | DelTrafficClass(_)
                | NewTrafficFilter(_)
                | DelTrafficFilter(_)
                | NewTrafficChain(_)
                | DelTrafficChain(_)
                | NewNsId(_)
                | DelNsId(_)
                // TODO(https://issuetracker.google.com/285127790): Implement NewNeighbour.
                | NewNeighbour(_)
                // TODO(https://issuetracker.google.com/285127790): Implement DelNeighbour.
                | DelNeighbour(_)
                // TODO(https://issuetracker.google.com/283136220): Implement SetLink.
                | SetLink(_)
                // TODO(https://issuetracker.google.com/283134942): Implement DelAddress.
                | DelAddress(_)
                // TODO(https://issuetracker.google.com/283136222): Implement NewRoute.
                | NewRoute(_)
                // TODO(https://issuetracker.google.com/283136222): Implement DelRoute.
                | DelRoute(_)
                // TODO(https://issuetracker.google.com/283137907): Implement NewQueueDiscipline.
                | NewQueueDiscipline(_)
                // TODO(https://issuetracker.google.com/283137907): Implement DelQueueDiscipline.
                | DelQueueDiscipline(_)
                // TODO(https://issuetracker.google.com/283134947): Implement NewRule.
                | NewRule(_)
                // TODO(https://issuetracker.google.com/283134947): Implement DelRule.
                | DelRule(_) => {
                    if is_ack {
                        warn!(
                            "Received unsupported NETLINK_ROUTE request; responding with an Ack: {:?}",
                            req,
                        );
                        client.send(new_ack(AckErrorCode, req_header))
                    } else {
                        warn!(
                            "Received unsupported NETLINK_ROUTE request that does not expect an Ack: {:?}",
                            req,
                        )
                    }
                }
                GetNeighbourTable(_)
                | GetTrafficClass(_)
                | GetTrafficFilter(_)
                | GetTrafficChain(_)
                | GetNsId(_)
                // TODO(https://issuetracker.google.com/285127384): Implement GetNeighbour.
                | GetNeighbour(_)
                // TODO(https://issuetracker.google.com/283134954): Implement GetLink.
                | GetLink(_)
                // TODO(https://issuetracker.google.com/283134032): Implement GetAddress.
                | GetAddress(_)
                // TODO(https://issuetracker.google.com/283137647): Implement GetRoute.
                | GetRoute(_)
                // TODO(https://issuetracker.google.com/283137907): Implement GetQueueDiscipline.
                | GetQueueDiscipline(_)
                // TODO(https://issuetracker.google.com/283134947): Implement GetRule.
                | GetRule(_) => {
                    if is_dump {
                        warn!(
                            "Received unsupported NETLINK_ROUTE DUMP request; responding with Done: {:?}",
                            req
                        );
                        client.send(new_done(req_header))
                    } else if is_ack {
                        warn!(
                            "Received unsupported NETLINK_ROUTE GET request: responding with Ack {:?}",
                            req
                        );
                        client.send(new_ack(AckErrorCode, req_header))
                    } else {
                        warn!(
                            "Received unsupported NETLINK_ROUTE GET request that does not expect an Ack {:?}",
                            req
                        )
                    }
                },
                req => panic!("unexpected RtnlMessage: {:?}", req),
            }
        }
    }

    /// A connection to the Route Netlink Protocol family.
    pub struct NetlinkRouteClient(pub(crate) ExternalClient<NetlinkRoute>);

    impl NetlinkRouteClient {
        /// Sets the PID assigned to the client.
        pub fn set_pid(&self, pid: NonZeroU32) {
            let NetlinkRouteClient(client) = self;
            client.set_port_number(pid)
        }

        /// Adds the given multicast group membership.
        pub fn add_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
            let NetlinkRouteClient(client) = self;
            client.add_membership(group)
        }

        /// Deletes the given multicast group membership.
        pub fn del_membership(&self, group: ModernGroup) -> Result<(), InvalidModernGroupError> {
            let NetlinkRouteClient(client) = self;
            client.del_membership(group)
        }

        /// Sets the legacy multicast group memberships.
        pub fn set_legacy_memberships(
            &self,
            legacy_memberships: LegacyGroups,
        ) -> Result<(), InvalidLegacyGroupsError> {
            let NetlinkRouteClient(client) = self;
            client.set_legacy_memberships(legacy_memberships)
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use netlink_packet_core::NetlinkHeader;

    pub(crate) const LEGACY_GROUP1: u32 = 0x00000001;
    pub(crate) const LEGACY_GROUP2: u32 = 0x00000002;
    pub(crate) const LEGACY_GROUP3: u32 = 0x00000004;
    pub(crate) const INVALID_LEGACY_GROUP: u32 = 0x00000008;
    pub(crate) const MODERN_GROUP1: ModernGroup = ModernGroup(1);
    pub(crate) const MODERN_GROUP2: ModernGroup = ModernGroup(2);
    pub(crate) const MODERN_GROUP3: ModernGroup = ModernGroup(3);
    pub(crate) const INVALID_MODERN_GROUP: ModernGroup = ModernGroup(4);

    #[derive(Debug)]
    pub(crate) enum FakeProtocolFamily {}

    impl MulticastCapableNetlinkFamily for FakeProtocolFamily {
        fn is_valid_group(group: &ModernGroup) -> bool {
            match *group {
                MODERN_GROUP1 | MODERN_GROUP2 | MODERN_GROUP3 => true,
                _ => false,
            }
        }
    }

    pub(crate) fn new_fake_netlink_message() -> NetlinkMessage<FakeNetlinkInnerMessage> {
        NetlinkMessage::new(
            NetlinkHeader::default(),
            NetlinkPayload::InnerMessage(FakeNetlinkInnerMessage),
        )
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    pub(crate) struct FakeNetlinkInnerMessage;

    impl NetlinkSerializable for FakeNetlinkInnerMessage {
        fn message_type(&self) -> u16 {
            u16::MAX
        }

        fn buffer_len(&self) -> usize {
            0
        }

        fn serialize(&self, _buffer: &mut [u8]) {}
    }

    /// Handler of [`FakeNetlinkInnerMessage`] requests.
    ///
    /// Reflects the given request back as the response.
    #[derive(Clone)]
    pub(crate) struct FakeNetlinkRequestHandler;

    #[async_trait]
    impl<S: Sender<FakeNetlinkInnerMessage>> NetlinkFamilyRequestHandler<FakeProtocolFamily, S>
        for FakeNetlinkRequestHandler
    {
        async fn handle_request(
            &mut self,
            req: NetlinkMessage<FakeNetlinkInnerMessage>,
            client: &mut InternalClient<FakeProtocolFamily, S>,
        ) {
            client.send(req)
        }
    }

    impl ProtocolFamily for FakeProtocolFamily {
        type InnerMessage = FakeNetlinkInnerMessage;
        type RequestHandler<S: Sender<Self::InnerMessage>> = FakeNetlinkRequestHandler;

        const NAME: &'static str = "FAKE_PROTOCOL_FAMILY";
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::num::NonZeroU32;

    use assert_matches::assert_matches;
    use futures::{channel::mpsc, future::FutureExt as _, stream::StreamExt as _};
    use net_declare::net_addr_subnet;
    use net_types::{
        ip::{AddrSubnetEither, IpVersion},
        Witness as _,
    };
    use netlink_packet_core::{NetlinkHeader, NLM_F_ACK, NLM_F_DUMP};
    use netlink_packet_route::{
        rtnl::address::nlas::Nla as AddressNla, AddressMessage, LinkMessage, RtnlMessage,
        TcMessage, AF_INET, AF_INET6, AF_PACKET, AF_UNSPEC,
    };
    use test_case::test_case;

    use crate::{
        interfaces,
        messaging::testutil::FakeSender,
        netlink_packet::{new_ack, new_done, AckErrorCode, NackErrorCode},
        protocol_family::route::{NetlinkRoute, NetlinkRouteRequestHandler},
    };

    enum ExpectedResponse<C> {
        Ack(C),
        Done,
    }

    fn header_with_flags(flags: u16) -> NetlinkHeader {
        let mut header = NetlinkHeader::default();
        header.flags = flags;
        header
    }

    /// Tests that unhandled requests are treated as a no-op.
    ///
    /// Get requests are responded to with a Done message if the dump flag
    /// is set, an Ack message if the ack flag is set or nothing. New/Del
    /// requests are responded to with an Ack message if the ack flag is set
    /// or nothing.
    #[test_case(
        RtnlMessage::GetTrafficChain,
        0,
        None; "get_with_no_flags")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack(AckErrorCode)); "get_with_ack_flag")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_DUMP,
        Some(ExpectedResponse::Done); "get_with_dump_flag")]
    #[test_case(
        RtnlMessage::GetTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Done); "get_with_ack_and_dump_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        0,
        None; "new_with_no_flags")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_DUMP,
        None; "new_with_dump_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack(AckErrorCode)); "new_with_ack_flag")]
    #[test_case(
        RtnlMessage::NewTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack(AckErrorCode)); "new_with_ack_and_dump_flags")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        0,
        None; "del_with_no_flags")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_DUMP,
        None; "del_with_dump_flag")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_ACK,
        Some(ExpectedResponse::Ack(AckErrorCode)); "del_with_ack_flag")]
    #[test_case(
        RtnlMessage::DelTrafficChain,
        NLM_F_ACK | NLM_F_DUMP,
        Some(ExpectedResponse::Ack(AckErrorCode)); "del_with_ack_and_dump_flags")]
    #[fuchsia::test]
    async fn test_handle_unsupported_request_response(
        tc_fn: fn(TcMessage) -> RtnlMessage,
        flags: u16,
        expected_response: Option<ExpectedResponse<AckErrorCode>>,
    ) {
        let (interfaces_request_sink, _interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
        };

        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let header = header_with_flags(flags);

        handler
            .handle_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(tc_fn(TcMessage::default())),
                ),
                &mut client,
            )
            .await;

        match expected_response {
            Some(ExpectedResponse::Ack(code)) => {
                assert_eq!(client_sink.take_messages(), [new_ack(code, header)])
            }
            Some(ExpectedResponse::Done) => {
                assert_eq!(client_sink.take_messages(), [new_done(header)])
            }
            None => {
                assert_eq!(client_sink.take_messages(), [])
            }
        }
    }

    struct RequestAndResponse<R> {
        request: R,
        response: Result<(), interfaces::RequestError>,
    }

    async fn test_request(
        request: NetlinkMessage<RtnlMessage>,
        req_and_resp: Option<RequestAndResponse<interfaces::RequestArgs>>,
    ) -> Vec<NetlinkMessage<RtnlMessage>> {
        let (mut client_sink, mut client) = crate::client::testutil::new_fake_client::<NetlinkRoute>(
            crate::client::testutil::CLIENT_ID_1,
            &[],
        );

        let (interfaces_request_sink, mut interfaces_request_stream) = mpsc::channel(0);
        let (v4_routes_request_sink, _v4_routes_request_stream) = mpsc::channel(0);
        let (v6_routes_request_sink, _v6_routes_request_stream) = mpsc::channel(0);

        let mut handler = NetlinkRouteRequestHandler::<FakeSender<_>> {
            interfaces_request_sink,
            v4_routes_request_sink,
            v6_routes_request_sink,
        };

        let ((), ()) = futures::future::join(handler.handle_request(request, &mut client), async {
            let next = interfaces_request_stream.next();
            match req_and_resp {
                Some(RequestAndResponse { request, response }) => {
                    let interfaces::Request { args, sequence_number: _, client: _, completer } =
                        next.await.expect("handler should send request");
                    assert_eq!(args, request);
                    completer.send(response).expect("handler should be alive");
                }
                None => assert_matches!(next.now_or_never(), None),
            }
        })
        .await;

        client_sink.take_messages()
    }

    /// Test RTM_GETLINK.
    #[test_case(
        0,
        None,
        None; "no_flags")]
    #[test_case(
        NLM_F_ACK,
        None,
        Some(ExpectedResponse::Ack(AckErrorCode)); "ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        Some(interfaces::GetLinkArgs::Dump),
        Some(ExpectedResponse::Done); "dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        Some(interfaces::GetLinkArgs::Dump),
        Some(ExpectedResponse::Done); "dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_link(
        flags: u16,
        expected_request_args: Option<interfaces::GetLinkArgs>,
        expected_response: Option<ExpectedResponse<AckErrorCode>>,
    ) {
        let header = header_with_flags(flags);

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetLink(LinkMessage::default())),
                ),
                expected_request_args.map(|a| RequestAndResponse {
                    request: interfaces::RequestArgs::Link(interfaces::LinkRequestArgs::Get(a)),
                    response: Ok(()),
                }),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| {
                    match expected_response {
                        ExpectedResponse::Ack(code) => new_ack(code, header),
                        ExpectedResponse::Done => new_done(header),
                    }
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Test RTM_GETADDR.
    #[test_case(
        0,
        AF_UNSPEC,
        None,
        None; "af_unspec_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_UNSPEC,
        None,
        Some(ExpectedResponse::Ack(Ok(()))); "af_unspec_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_UNSPEC,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: None,
        }),
        Some(ExpectedResponse::Done); "af_unspec_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_UNSPEC,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: None,
        }),
        Some(ExpectedResponse::Done); "af_unspec_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET,
        None,
        None; "af_inet_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET,
        None,
        Some(ExpectedResponse::Ack(Ok(()))); "af_inet_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V4),
        }),
        Some(ExpectedResponse::Done); "af_inet_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V4),
        }),
        Some(ExpectedResponse::Done); "af_inet_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_INET6,
        None,
        None; "af_inet6_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_INET6,
        None,
        Some(ExpectedResponse::Ack(Ok(()))); "af_inet6_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_INET6,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V6),
        }),
        Some(ExpectedResponse::Done); "af_inet6_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_INET6,
        Some(interfaces::GetAddressArgs::Dump {
            ip_version_filter: Some(IpVersion::V6),
        }),
        Some(ExpectedResponse::Done); "af_inet6_dump_and_ack_flags")]
    #[test_case(
        0,
        AF_PACKET,
        None,
        None; "af_other_no_flags")]
    #[test_case(
        NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Ack(Ok(()))); "af_other_ack_flag")]
    #[test_case(
        NLM_F_DUMP,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Ack(Err(NackErrorCode::INVALID))); "af_other_dump_flag")]
    #[test_case(
        NLM_F_DUMP | NLM_F_ACK,
        AF_PACKET,
        None,
        Some(ExpectedResponse::Ack(Err(NackErrorCode::INVALID))); "af_other_dump_and_ack_flags")]
    #[fuchsia::test]
    async fn test_get_addr(
        flags: u16,
        family: u16,
        expected_request_args: Option<interfaces::GetAddressArgs>,
        expected_response: Option<ExpectedResponse<Result<(), NackErrorCode>>>,
    ) {
        let header = header_with_flags(flags);
        let address_message = {
            let mut message = AddressMessage::default();
            message.header.family = family.try_into().unwrap();
            message
        };

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::GetAddress(address_message)),
                ),
                expected_request_args.map(|a| RequestAndResponse {
                    request: interfaces::RequestArgs::Address(interfaces::AddressRequestArgs::Get(
                        a
                    )),
                    response: Ok(()),
                }),
            )
            .await,
            expected_response
                .into_iter()
                .map(|expected_response| match expected_response {
                    ExpectedResponse::Ack(code) => new_ack(code, header),
                    ExpectedResponse::Done => new_done(header),
                })
                .collect::<Vec<_>>(),
        )
    }

    struct TestNewAddrCase {
        flags: u16,
        family: u16,
        nlas: Vec<AddressNla>,
        prefix_len: u8,
        interface_id: u32,
        expected_request_args: Option<RequestAndResponse<interfaces::NewAddressArgs>>,
        expected_response: Option<Result<(), NackErrorCode>>,
    }

    fn bytes_from_addr(a: AddrSubnetEither) -> Vec<u8> {
        match a {
            AddrSubnetEither::V4(a) => a.addr().get().ipv4_bytes().to_vec(),
            AddrSubnetEither::V6(a) => a.addr().get().ipv6_bytes().to_vec(),
        }
    }

    fn prefix_from_addr(a: AddrSubnetEither) -> u8 {
        let (_addr, prefix) = a.addr_prefix();
        prefix
    }

    fn interface_id_as_u32(id: u64) -> u32 {
        id.try_into().unwrap()
    }

    fn valid_new_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
        response: Result<(), interfaces::RequestError>,
    ) -> TestNewAddrCase {
        TestNewAddrCase {
            flags: if ack { NLM_F_ACK } else { 0 },
            family: match addr {
                AddrSubnetEither::V4(_) => AF_INET,
                AddrSubnetEither::V6(_) => AF_INET6,
            },
            nlas: vec![AddressNla::Local(bytes_from_addr(addr))],
            prefix_len: prefix_from_addr(addr),
            interface_id: interface_id_as_u32(interface_id),
            expected_request_args: Some(RequestAndResponse {
                request: interfaces::NewAddressArgs {
                    address_and_interface_id: interfaces::AddressAndInterfaceArgs {
                        address: addr,
                        interface_id: NonZeroU32::new(interface_id_as_u32(interface_id)).unwrap(),
                    },
                },
                response,
            }),
            expected_response: ack.then_some(Ok(())),
        }
    }

    fn invalid_new_addr_request(
        ack: bool,
        addr: AddrSubnetEither,
        interface_id: u64,
    ) -> TestNewAddrCase {
        TestNewAddrCase {
            expected_request_args: None,
            expected_response: Some(Err(NackErrorCode::INVALID)),
            ..valid_new_addr_request(ack, addr, interface_id, Ok(()))
        }
    }

    // Test RTM_NEWADDR
    #[test_case(
        valid_new_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "v4_ok_ack")]
    #[test_case(
        valid_new_addr_request(
            true,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::LO_INTERFACE_ID,
            Ok(())); "v6_ok_ack")]
    #[test_case(
        valid_new_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v4(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "v4_ok_no_ack")]
    #[test_case(
        valid_new_addr_request(
            false,
            interfaces::testutil::test_addr_subnet_v6(),
            interfaces::testutil::ETH_INTERFACE_ID,
            Ok(())); "v6_ok_no_ack")]
    #[test_case(
        TestNewAddrCase {
            expected_response: Some(Err(NackErrorCode::INVALID)),
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::InvalidRequest),
            )
        }; "v4_invalid_response_ack")]
    #[test_case(
        TestNewAddrCase {
            expected_response: Some(Err(NackErrorCode::new(libc::EEXIST).unwrap())),
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
                Err(interfaces::RequestError::AlreadyExists),
            )
        }; "v6_exist_response_no_ack")]
    #[test_case(
        TestNewAddrCase {
            expected_response: Some(Err(NackErrorCode::new(libc::ENODEV).unwrap())),
            ..valid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Err(interfaces::RequestError::UnrecognizedInterface),
            )
        }; "v6_unrecognized_interface_response_no_ack")]
    #[test_case(
        TestNewAddrCase {
            expected_response: Some(Err(NackErrorCode::new(libc::EADDRNOTAVAIL).unwrap())),
            ..valid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::ETH_INTERFACE_ID,
                Err(interfaces::RequestError::AddressNotFound),
            )
        }; "v4_not_found_response_ck")]
    #[test_case(
        TestNewAddrCase {
            interface_id: 0,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::LO_INTERFACE_ID,
            )
        }; "zero_interface_id_ack")]
    #[test_case(
        TestNewAddrCase {
            interface_id: 0,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "zero_interface_id_no_ack")]
    #[test_case(
        TestNewAddrCase {
            nlas: Vec::new(),
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "no_nlas_ack")]
    #[test_case(
        TestNewAddrCase {
            nlas: Vec::new(),
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "no_nlas_no_ack")]
    #[test_case(
        TestNewAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "missing_local_nla_ack")]
    #[test_case(
        TestNewAddrCase {
            nlas: vec![AddressNla::Flags(0)],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "missing_local_nla_no_ack")]
    #[test_case(
        TestNewAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "invalid_local_nla_ack")]
    #[test_case(
        TestNewAddrCase {
            nlas: vec![AddressNla::Local(Vec::new())],
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "invalid_local_nla_no_ack")]
    #[test_case(
        TestNewAddrCase {
            prefix_len: 0,
            ..valid_new_addr_request(
                true,
                net_addr_subnet!("192.0.2.123/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "zero_prefix_len_ack")]
    #[test_case(
        TestNewAddrCase {
            prefix_len: 0,
            ..valid_new_addr_request(
                false,
                net_addr_subnet!("2001:db8::1324/0"),
                interfaces::testutil::WLAN_INTERFACE_ID,
                Ok(()),
            )
        }; "zero_prefix_len_no_ack")]
    #[test_case(
        TestNewAddrCase {
            prefix_len: u8::MAX,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "invalid_prefix_len_ack")]
    #[test_case(
        TestNewAddrCase {
            prefix_len: u8::MAX,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::WLAN_INTERFACE_ID,
            )
        }; "invalid_prefix_len_no_ack")]
    #[test_case(
        TestNewAddrCase {
            family: AF_UNSPEC,
            ..invalid_new_addr_request(
                true,
                interfaces::testutil::test_addr_subnet_v4(),
                interfaces::testutil::LO_INTERFACE_ID,
            )
        }; "invalid_family_ack")]
    #[test_case(
        TestNewAddrCase {
            family: AF_UNSPEC,
            ..invalid_new_addr_request(
                false,
                interfaces::testutil::test_addr_subnet_v6(),
                interfaces::testutil::PPP_INTERFACE_ID,
            )
        }; "invalid_family_no_ack")]
    #[fuchsia::test]
    async fn test_new_addr(test_case: TestNewAddrCase) {
        let TestNewAddrCase {
            flags,
            family,
            nlas,
            prefix_len,
            interface_id,
            expected_request_args,
            expected_response,
        } = test_case;

        let header = header_with_flags(flags);
        let address_message = {
            let mut message = AddressMessage::default();
            message.header.family = family.try_into().unwrap();
            message.header.index = interface_id;
            message.header.prefix_len = prefix_len;
            message.nlas = nlas;
            message
        };

        pretty_assertions::assert_eq!(
            test_request(
                NetlinkMessage::new(
                    header,
                    NetlinkPayload::InnerMessage(RtnlMessage::NewAddress(address_message)),
                ),
                expected_request_args.map(|RequestAndResponse { request, response }| {
                    RequestAndResponse {
                        request: interfaces::RequestArgs::Address(
                            interfaces::AddressRequestArgs::New(request),
                        ),
                        response,
                    }
                }),
            )
            .await,
            expected_response.into_iter().map(|code| new_ack(code, header)).collect::<Vec<_>>(),
        )
    }
}
