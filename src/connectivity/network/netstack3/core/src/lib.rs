// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.

#![no_std]
// TODO(https://fxbug.dev/339502691): Return to the default limit once lock
// ordering no longer causes overflows.
#![recursion_limit = "256"]
// In case we roll the toolchain and something we're using as a feature has been
// stabilized.
#![allow(stable_features)]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]
// Turn off checks for dead code, but only when building for benchmarking.
// benchmarking. This allows the benchmarks to be written as part of the crate,
// with access to test utilities, without a bunch of build errors due to unused
// code. These checks are turned back on in the 'benchmark' module.
#![cfg_attr(benchmark, allow(dead_code, unused_imports, unused_macros))]

// TODO(https://github.com/rust-lang-nursery/portability-wg/issues/11): remove
// this module.
extern crate fakealloc as alloc;

// TODO(https://github.com/dtolnay/thiserror/pull/64): remove this module.
extern crate fakestd as std;

mod api;
mod context;
mod counters;
mod lock_ordering;
mod marker;
mod state;
mod time;
mod transport;

#[cfg(any(test, benchmark))]
pub mod benchmarks;
#[cfg(any(test, feature = "testutils"))]
pub mod testutil;

pub(crate) mod algorithm {
    pub(crate) use netstack3_base::{simple_randomized_port_alloc, PortAllocImpl};
}

pub(crate) mod data_structures {
    pub(crate) mod socketmap {
        pub(crate) use netstack3_base::socketmap::{IterShadows, SocketMap, Tagged};
    }
}

/// The device layer.
pub mod device {
    #[path = "."]
    pub(crate) mod integration {
        mod base;
        mod ethernet;
        mod loopback;
        mod pure_ip;
        mod socket;

        pub(crate) use base::{
            with_device_state, with_device_state_and_core_ctx, with_ip_device_state,
            with_ip_device_state_and_core_ctx,
        };
    }

    // TODO(https://fxbug.dev/342685842): Remove this re-export.
    pub(crate) use netstack3_device::*;

    // Re-exported types.
    pub use ethernet::{
        EthernetCreationProperties, EthernetDeviceId, EthernetLinkDevice, EthernetWeakDeviceId,
        MaxEthernetFrameSize, RecvEthernetFrameMeta,
    };
    pub use loopback::{LoopbackCreationProperties, LoopbackDevice, LoopbackDeviceId};
    pub use netstack3_device::{
        ArpConfiguration, ArpConfigurationUpdate, DeviceClassMatcher, DeviceConfiguration,
        DeviceConfigurationUpdate, DeviceConfigurationUpdateError, DeviceId,
        DeviceIdAndNameMatcher, DeviceLayerEventDispatcher, DeviceLayerStateTypes, DeviceProvider,
        DeviceSendFrameError, NdpConfiguration, NdpConfigurationUpdate, WeakDeviceId,
    };
    pub use pure_ip::{
        PureIpDevice, PureIpDeviceCreationProperties, PureIpDeviceId,
        PureIpDeviceReceiveFrameMetadata, PureIpHeaderParams, PureIpWeakDeviceId,
    };
    pub use queue::{
        ReceiveQueueBindingsContext, TransmitQueueBindingsContext, TransmitQueueConfiguration,
    };
}

/// Device socket API.
pub mod device_socket {
    pub use crate::device::socket::{
        DeviceSocketBindingsContext, DeviceSocketMetadata, DeviceSocketTypes, EthernetFrame,
        EthernetHeaderParams, Frame, IpFrame, Protocol, ReceivedFrame, SendFrameError, SentFrame,
        SocketId, SocketInfo, TargetDevice,
    };
    pub use netstack3_base::FrameDestination;
}

/// Generic netstack errors.
pub mod error {
    pub use netstack3_base::{
        AddressResolutionFailed, ExistsError, LocalAddressError, NotFoundError, NotSupportedError,
        RemoteAddressError, SocketError, ZonedAddressError,
    };
}

/// Framework for packet filtering.
pub mod filter {
    pub(crate) mod integration;

    pub use netstack3_filter::{
        Action, AddressMatcher, AddressMatcherType, FilterApi, FilterBindingsContext,
        FilterBindingsTypes, Hook, InterfaceMatcher, InterfaceProperties, IpRoutines, NatRoutines,
        PacketMatcher, PortMatcher, ProofOfEgressCheck, Routine, Routines, Rule, TransparentProxy,
        TransportProtocolMatcher, UninstalledRoutine, ValidationError,
    };
    pub(crate) use netstack3_filter::{
        FilterContext, FilterImpl, FilterIpContext, IpPacket, NatContext, State,
    };
}

/// Facilities for inspecting stack state for debugging.
pub mod inspect {
    pub use netstack3_base::{Inspectable, InspectableValue, Inspector, InspectorDeviceExt};
}

/// Methods for dealing with ICMP sockets.
pub mod icmp {
    pub use netstack3_ip::icmp::{IcmpEchoBindingsContext, IcmpEchoBindingsTypes, IcmpSocketId};
}

/// The Internet Protocol, versions 4 and 6.
pub mod ip {
    #[path = "."]
    pub(crate) mod integration {
        mod base;
        mod device;
        mod raw;

        pub(crate) use device::{CoreCtxWithIpDeviceConfiguration, IpAddrCtxSpec};
    }

    // TODO(https://fxbug.dev/342685842): Remove this re-export.
    pub(crate) use netstack3_ip::*;

    // Re-exported types.
    pub use device::{
        AddIpAddrSubnetError, AddrSubnetAndManualConfigEither, AddressRemovedReason,
        IpAddressState, IpDeviceConfiguration, IpDeviceConfigurationUpdate, IpDeviceEvent,
        Ipv4AddrConfig, Ipv4DeviceConfigurationAndFlags, Ipv4DeviceConfigurationUpdate,
        Ipv6AddrManualConfig, Ipv6DeviceConfiguration, Ipv6DeviceConfigurationAndFlags,
        Ipv6DeviceConfigurationUpdate, Lifetime, SetIpAddressPropertiesError, SlaacConfiguration,
        StableIidSecret, TemporarySlaacAddressConfiguration, UpdateIpConfigurationError,
    };
    pub use netstack3_ip::{IpLayerEvent, ResolveRouteError};
    pub use raw::{
        RawIpSocketId, RawIpSocketProtocol, RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes,
        WeakRawIpSocketId,
    };
    pub use socket::{IpSockCreateAndSendError, IpSockCreationError, IpSockSendError};
}

/// Types and utilities for dealing with neighbors.
pub mod neighbor {
    // Re-exported types.
    pub use netstack3_ip::nud::{
        Event, EventDynamicState, EventKind, EventState, LinkResolutionContext,
        LinkResolutionNotifier, LinkResolutionResult, NeighborRemovalError, NudUserConfig,
        NudUserConfigUpdate, StaticNeighborInsertionError, MAX_ENTRIES,
    };
}

/// Types and utilities for dealing with routes.
pub mod routes {
    // Re-exported types.
    pub use netstack3_ip::{
        AddRouteError, AddableEntry, AddableEntryEither, AddableMetric, Entry, EntryEither,
        Generation, Metric, NextHop, RawMetric, ResolvedRoute, RoutableIpAddr, WrapBroadcastMarker,
    };
}

/// Common types for dealing with sockets.
pub mod socket {
    pub(crate) use netstack3_ip::datagram;

    pub(crate) use netstack3_base::socket::{
        AddrEntry, AddrVec, Bound, ConnAddr, ConnInfoAddr, ConnIpAddr, FoundSockets,
        IncompatibleError, InsertError, Inserter, ListenerAddr, ListenerAddrInfo, ListenerIpAddr,
        MaybeDualStack, RemoveResult, SocketAddrType, SocketIpAddr, SocketMapAddrSpec,
        SocketMapAddrStateSpec, SocketMapConflictPolicy, SocketMapStateSpec,
    };

    pub use datagram::{
        ConnInfo, ConnectError, ExpectedConnError, ExpectedUnboundError, ListenerInfo,
        MulticastInterfaceSelector, MulticastMembershipInterfaceSelector, SendError, SendToError,
        SetMulticastMembershipError, SocketInfo,
    };

    pub use netstack3_base::socket::{
        AddrIsMappedError, NotDualStackCapableError, SetDualStackEnabledError, ShutdownType,
        StrictlyZonedAddr,
    };
}

/// Useful synchronization primitives.
pub mod sync {
    // We take all of our dependencies directly from base for symmetry with the
    // other crates. However, we want to explicitly have all the dependencies in
    // GN so we can assert the dependencies on the crate variants. This defeats
    // rustc's unused dependency check.
    use netstack3_sync as _;

    pub use netstack3_base::{
        sync::{
            DebugReferences, DynDebugReferences, LockGuard, MapRcNotifier, Mutex, PrimaryRc,
            RcNotifier, RwLock, RwLockReadGuard, RwLockWriteGuard, StrongRc, WeakRc,
        },
        RemoveResourceResult, RemoveResourceResultWithContext,
    };
}

/// Methods for dealing with TCP sockets.
pub mod tcp {
    pub use netstack3_tcp::{
        AcceptError, BindError, BoundInfo, Buffer, BufferLimits, BufferSizes, ConnectError,
        ConnectionError, ConnectionInfo, IntoBuffers, ListenError, ListenerNotifier, NoConnection,
        Payload, ReceiveBuffer, RingBuffer, SendBuffer, SendPayload, SetDeviceError,
        SetReuseAddrError, SocketAddr, SocketInfo, SocketOptions, Takeable, TcpBindingsTypes,
        TcpSocketId, UnboundInfo, DEFAULT_FIN_WAIT2_TIMEOUT,
    };
}

/// Miscellaneous and common types.
pub mod types {
    pub use netstack3_base::WorkQueueReport;
}

/// Methods for dealing with UDP sockets.
pub mod udp {
    pub use crate::transport::udp::{
        SendError, SendToError, UdpBindingsTypes, UdpReceiveBindingsContext, UdpRemotePort,
        UdpSocketId,
    };
}

pub use api::CoreApi;
pub use context::{
    CoreCtx, CtxPair, DeferredResourceRemovalContext, EventContext, InstantBindingsTypes,
    InstantContext, ReferenceNotifiers, RngContext, TimerBindingsTypes, TimerContext,
    TracingContext, UnlockedCoreCtx,
};
pub use inspect::Inspector;
pub use marker::{BindingsContext, BindingsTypes, CoreContext, IpBindingsContext, IpExt};
pub use state::{StackState, StackStateBuilder};
pub use time::{Instant, TimerId};

// Re-export useful macros.
pub use netstack3_device::for_any_device_id;
pub use netstack3_macros::context_ip_bounds;
