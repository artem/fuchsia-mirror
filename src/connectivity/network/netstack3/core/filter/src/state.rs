// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod validation;

use alloc::{sync::Arc, vec::Vec};
use core::{
    fmt::Debug,
    hash::{Hash, Hasher},
};

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip};
use packet_formats::ip::IpExt;

use crate::{conntrack, matchers::PacketMatcher, ValidRoutines};

/// The action to take on a packet.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "DeviceClass: Clone, RuleInfo: Clone"),
    Debug(bound = "DeviceClass: Debug")
)]
pub enum Action<I: IpExt, DeviceClass, RuleInfo> {
    /// Accept the packet.
    ///
    /// This is a terminal action for the current *installed* routine, i.e. no
    /// further rules will be evaluated for this packet in the installed routine
    /// (or any subroutines) in which this rule is installed. Subsequent
    /// routines installed on the same hook will still be evaluated.
    Accept,
    /// Drop the packet.
    ///
    /// This is a terminal action for the current hook, i.e. no further rules
    /// will be evaluated for this packet, even in other routines on the same
    /// hook.
    Drop,
    /// Jump from the current routine to the specified uninstalled routine.
    Jump(UninstalledRoutine<I, DeviceClass, RuleInfo>),
    /// Stop evaluation of the current routine and return to the calling routine
    /// (the routine from which the current routine was jumped), continuing
    /// evaluation at the next rule.
    ///
    /// If invoked in an installed routine, equivalent to `Accept`, given
    /// packets are accepted by default in the absence of any matching rules.
    Return,
}

/// A handle to a [`Routine`] that is not installed in a particular hook, and
/// therefore is only run if jumped to from another routine.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct UninstalledRoutine<I: IpExt, DeviceClass, RuleInfo>(
    pub(crate) Arc<Routine<I, DeviceClass, RuleInfo>>,
);

impl<I: IpExt, DeviceClass, RuleInfo> UninstalledRoutine<I, DeviceClass, RuleInfo> {
    /// Creates a new uninstalled routine with the provided contents.
    pub fn new(rules: Vec<Rule<I, DeviceClass, RuleInfo>>) -> Self {
        Self(Arc::new(Routine { rules }))
    }

    /// Returns the inner routine.
    pub fn get(&self) -> &Routine<I, DeviceClass, RuleInfo> {
        let Self(inner) = self;
        &*inner
    }
}

impl<I: IpExt, DeviceClass, RuleInfo> PartialEq for UninstalledRoutine<I, DeviceClass, RuleInfo> {
    fn eq(&self, other: &Self) -> bool {
        let Self(lhs) = self;
        let Self(rhs) = other;
        Arc::ptr_eq(lhs, rhs)
    }
}

impl<I: IpExt, DeviceClass, RuleInfo> Eq for UninstalledRoutine<I, DeviceClass, RuleInfo> {}

impl<I: IpExt, DeviceClass, RuleInfo> Hash for UninstalledRoutine<I, DeviceClass, RuleInfo> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Self(inner) = self;
        Arc::as_ptr(inner).hash(state)
    }
}

/// A set of criteria (matchers) and a resultant action to take if a given
/// packet matches.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(
    Clone(bound = "DeviceClass: Clone, RuleInfo: Clone"),
    Debug(bound = "DeviceClass: Debug")
)]
pub struct Rule<I: IpExt, DeviceClass, RuleInfo> {
    /// The criteria that a packet must match for the action to be executed.
    pub matcher: PacketMatcher<I, DeviceClass>,
    /// The action to take on a matching packet.
    pub action: Action<I, DeviceClass, RuleInfo>,
    /// Opaque information about this rule for use when validating and
    /// converting state provided by Bindings into Core filtering state. This is
    /// only used when installing filtering state, and allows Core to report to
    /// Bindings which rule caused a particular error. It is zero-sized for
    /// validated state.
    #[derivative(Debug = "ignore")]
    pub validation_info: RuleInfo,
}

/// A sequence of [`Rule`]s.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(
    Clone(bound = "DeviceClass: Clone, RuleInfo: Clone"),
    Debug(bound = "DeviceClass: Debug")
)]
pub struct Routine<I: IpExt, DeviceClass, RuleInfo> {
    /// The rules to be executed in order.
    pub rules: Vec<Rule<I, DeviceClass, RuleInfo>>,
}

/// A particular entry point for packet processing in which filtering routines
/// are installed.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct Hook<I: IpExt, DeviceClass, RuleInfo> {
    /// The routines to be executed in order.
    pub routines: Vec<Routine<I, DeviceClass, RuleInfo>>,
}

/// Routines that perform ordinary IP filtering.
#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct IpRoutines<I: IpExt, DeviceClass, RuleInfo> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for incoming traffic that is destined for another node.
    pub forwarding: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass, RuleInfo>,
}

/// Routines that can perform NAT.
///
/// Note that NAT routines are only executed *once* for a given connection, for
/// the first packet in the flow.
#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct NatRoutines<I: IpExt, DeviceClass, RuleInfo> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass, RuleInfo>,
}

/// Data stored in [`conntrack::Connection`] that is only needed by filtering.
#[derive(Default)]
pub struct ConntrackExternalData {}

/// IP version-specific filtering routine state.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct Routines<I: IpExt, DeviceClass, RuleInfo> {
    /// Routines that perform IP filtering.
    pub ip: IpRoutines<I, DeviceClass, RuleInfo>,
    /// Routines that perform IP filtering and NAT.
    pub nat: NatRoutines<I, DeviceClass, RuleInfo>,
}

/// IP version-specific filtering state.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct State<I: IpExt, DeviceClass> {
    /// Routines used for filtering packets.
    pub routines: ValidRoutines<I, DeviceClass>,
    /// Connection tracking state.
    pub conntrack: conntrack::Table<I, ConntrackExternalData>,
}
