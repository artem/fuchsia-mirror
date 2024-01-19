// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common device identifier types.

use alloc::sync::Arc;
use core::fmt::{self, Debug};
use core::hash::Hash;

use derivative::Derivative;

use crate::ip::device::state::DualStackIpDeviceState;
use crate::{
    device::{
        ethernet::EthernetLinkDevice,
        loopback::{LoopbackDevice, LoopbackDeviceId, LoopbackWeakDeviceId},
        state::{BaseDeviceState, DeviceStateSpec, IpLinkDeviceState, WeakCookie},
        DeviceLayerTypes,
    },
    sync::{PrimaryRc, StrongRc},
};

/// An identifier for a device.
pub trait Id: Clone + Debug + Eq + Hash + PartialEq + Send + Sync + 'static {
    /// Returns true if the device is a loopback device.
    fn is_loopback(&self) -> bool;
}

/// A marker for a Strong device reference.
///
/// Types marked with [`StrongId`] indicates that the referenced device is alive
/// while the type exists.
pub trait StrongId: Id {
    /// The weak version of this identifier.
    type Weak: WeakId<Strong = Self>;
}

/// A marker for a Weak device reference.
///
/// This is the weak marker equivalent of [`StrongId`].
pub trait WeakId: Id + PartialEq<Self::Strong> {
    /// The strong version of this identifier.
    type Strong: StrongId<Weak = Self>;
}

/// A weak ID identifying a device.
///
/// This device ID makes no claim about the live-ness of the underlying device.
/// See [`DeviceId`] for a device ID that acts as a witness to the live-ness of
/// a device.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[allow(missing_docs)]
pub enum WeakDeviceId<BT: DeviceLayerTypes> {
    Ethernet(EthernetWeakDeviceId<BT>),
    Loopback(LoopbackWeakDeviceId<BT>),
}

impl<BT: DeviceLayerTypes> PartialEq<DeviceId<BT>> for WeakDeviceId<BT> {
    fn eq(&self, other: &DeviceId<BT>) -> bool {
        <DeviceId<BT> as PartialEq<WeakDeviceId<BT>>>::eq(other, self)
    }
}

impl<BT: DeviceLayerTypes> From<EthernetWeakDeviceId<BT>> for WeakDeviceId<BT> {
    fn from(id: EthernetWeakDeviceId<BT>) -> WeakDeviceId<BT> {
        WeakDeviceId::Ethernet(id)
    }
}

impl<BT: DeviceLayerTypes> From<LoopbackWeakDeviceId<BT>> for WeakDeviceId<BT> {
    fn from(id: LoopbackWeakDeviceId<BT>) -> WeakDeviceId<BT> {
        WeakDeviceId::Loopback(id)
    }
}

impl<BT: DeviceLayerTypes> WeakDeviceId<BT> {
    /// Attempts to upgrade the ID.
    pub fn upgrade(&self) -> Option<DeviceId<BT>> {
        for_any_device_id!(WeakDeviceId, self, id => id.upgrade().map(Into::into))
    }

    /// Creates a [`DebugReferences`] instance for this device.
    pub fn debug_references(&self) -> DebugReferences<BT> {
        DebugReferences(for_any_device_id!(
            WeakDeviceId,
            self,
            BaseWeakDeviceId { cookie } => cookie.weak_ref.debug_references().into()
        ))
    }

    /// Returns the bindings identifier associated with the device.
    pub fn bindings_id(&self) -> &BT::DeviceIdentifier {
        for_any_device_id!(WeakDeviceId, self, id => id.bindings_id())
    }
}

enum DebugReferencesInner<BT: DeviceLayerTypes> {
    Loopback(crate::sync::DebugReferences<BaseDeviceState<LoopbackDevice, BT>>),
    Ethernet(crate::sync::DebugReferences<BaseDeviceState<EthernetLinkDevice, BT>>),
}

impl<BT: DeviceLayerTypes> From<crate::sync::DebugReferences<BaseDeviceState<LoopbackDevice, BT>>>
    for DebugReferencesInner<BT>
{
    fn from(inner: crate::sync::DebugReferences<BaseDeviceState<LoopbackDevice, BT>>) -> Self {
        DebugReferencesInner::Loopback(inner)
    }
}

impl<BT: DeviceLayerTypes>
    From<crate::sync::DebugReferences<BaseDeviceState<EthernetLinkDevice, BT>>>
    for DebugReferencesInner<BT>
{
    fn from(inner: crate::sync::DebugReferences<BaseDeviceState<EthernetLinkDevice, BT>>) -> Self {
        DebugReferencesInner::Ethernet(inner)
    }
}

/// A type offering a [`Debug`] implementation that helps debug dangling device
/// references.
pub struct DebugReferences<BT: DeviceLayerTypes>(DebugReferencesInner<BT>);

impl<BT: DeviceLayerTypes> Debug for DebugReferences<BT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(inner) = self;
        match inner {
            DebugReferencesInner::Loopback(d) => write!(f, "Loopback({d:?})"),
            DebugReferencesInner::Ethernet(d) => write!(f, "Ethernet({d:?})"),
        }
    }
}

impl<BT: DeviceLayerTypes> Id for WeakDeviceId<BT> {
    fn is_loopback(&self) -> bool {
        match self {
            WeakDeviceId::Loopback(_) => true,
            WeakDeviceId::Ethernet(_) => false,
        }
    }
}

impl<BT: DeviceLayerTypes> WeakId for WeakDeviceId<BT> {
    type Strong = DeviceId<BT>;
}

impl<BT: DeviceLayerTypes> Debug for WeakDeviceId<BT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for_any_device_id!(WeakDeviceId, self, id => Debug::fmt(id, f))
    }
}

/// A strong ID identifying a device.
///
/// Holders may safely assume that the underlying device is "alive" in the sense
/// that the device is still recognized by the stack. That is, operations that
/// use this device ID will never fail as a result of "unrecognized device"-like
/// errors.
#[derive(Derivative)]
#[derivative(Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[allow(missing_docs)]
pub enum DeviceId<BT: DeviceLayerTypes> {
    Ethernet(EthernetDeviceId<BT>),
    Loopback(LoopbackDeviceId<BT>),
}

/// Evaluates the expression for the given device_id, regardless of its variant.
///
/// The first argument should be a device ID enum, either [`DeviceId`] or
/// [`WeakDeviceId`].
macro_rules! for_any_device_id {
    ($device_id_enum_type:ident, $device_id:expr, $variable:pat => $expression:expr) => {
        match $device_id {
            $device_id_enum_type::Loopback($variable) => $expression,
            $device_id_enum_type::Ethernet($variable) => $expression,
        }
    };
}
pub(crate) use for_any_device_id;

impl<BT: DeviceLayerTypes> Clone for DeviceId<BT> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    fn clone(&self) -> Self {
        for_any_device_id!(DeviceId, self, id => id.clone().into())
    }
}

impl<BT: DeviceLayerTypes> PartialEq<WeakDeviceId<BT>> for DeviceId<BT> {
    fn eq(&self, other: &WeakDeviceId<BT>) -> bool {
        match (self, other) {
            (DeviceId::Ethernet(strong), WeakDeviceId::Ethernet(weak)) => strong == weak,
            (DeviceId::Loopback(strong), WeakDeviceId::Loopback(weak)) => strong == weak,
            (DeviceId::Loopback(_), WeakDeviceId::Ethernet(_))
            | (DeviceId::Ethernet(_), WeakDeviceId::Loopback(_)) => false,
        }
    }
}

impl<BT: DeviceLayerTypes> PartialOrd for DeviceId<BT> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<BT: DeviceLayerTypes> Ord for DeviceId<BT> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        match (self, other) {
            (DeviceId::Ethernet(me), DeviceId::Ethernet(other)) => me.cmp(other),
            (DeviceId::Loopback(me), DeviceId::Loopback(other)) => me.cmp(other),
            (DeviceId::Loopback(_), DeviceId::Ethernet(_)) => core::cmp::Ordering::Less,
            (DeviceId::Ethernet(_), DeviceId::Loopback(_)) => core::cmp::Ordering::Greater,
        }
    }
}

impl<BT: DeviceLayerTypes> From<EthernetDeviceId<BT>> for DeviceId<BT> {
    fn from(id: EthernetDeviceId<BT>) -> DeviceId<BT> {
        DeviceId::Ethernet(id)
    }
}

impl<BT: DeviceLayerTypes> From<LoopbackDeviceId<BT>> for DeviceId<BT> {
    fn from(id: LoopbackDeviceId<BT>) -> DeviceId<BT> {
        DeviceId::Loopback(id)
    }
}

impl<BT: DeviceLayerTypes> DeviceId<BT> {
    /// Downgrade to a [`WeakDeviceId`].
    pub fn downgrade(&self) -> WeakDeviceId<BT> {
        for_any_device_id!(DeviceId, self, id => id.downgrade().into())
    }

    /// Returns the bindings identifier associated with the device.
    pub fn bindings_id(&self) -> &BT::DeviceIdentifier {
        for_any_device_id!(DeviceId, self, id => id.bindings_id())
    }

    #[cfg(test)]
    pub(crate) fn unwrap_ethernet(self) -> EthernetDeviceId<BT> {
        assert_matches::assert_matches!(self, DeviceId::Ethernet(e) => e)
    }
}

impl<BT: DeviceLayerTypes> Id for DeviceId<BT> {
    fn is_loopback(&self) -> bool {
        match self {
            DeviceId::Loopback(_) => true,
            DeviceId::Ethernet(_) => false,
        }
    }
}

impl<BT: DeviceLayerTypes> StrongId for DeviceId<BT> {
    type Weak = WeakDeviceId<BT>;
}

impl<BT: DeviceLayerTypes> Debug for DeviceId<BT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for_any_device_id!(DeviceId, self, id => Debug::fmt(id, f))
    }
}

/// A base weak device identifier.
///
/// Allows multiple device implementations to share the same shape for
/// maintaining reference identifiers.
#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub struct BaseWeakDeviceId<T: DeviceStateSpec, BT: DeviceLayerTypes> {
    // NB: This is not a tuple struct because regular structs play nicer with
    // type aliases, which is how we use BaseDeviceId.
    cookie: Arc<WeakCookie<T, BT>>,
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> PartialEq for BaseWeakDeviceId<T, BT> {
    fn eq(&self, other: &Self) -> bool {
        self.cookie.weak_ref.ptr_eq(&other.cookie.weak_ref)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Eq for BaseWeakDeviceId<T, BT> {}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Hash for BaseWeakDeviceId<T, BT> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.cookie.weak_ref.hash(state)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> PartialEq<BaseDeviceId<T, BT>>
    for BaseWeakDeviceId<T, BT>
{
    fn eq(&self, other: &BaseDeviceId<T, BT>) -> bool {
        <BaseDeviceId<T, BT> as PartialEq<BaseWeakDeviceId<T, BT>>>::eq(other, self)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Debug for BaseWeakDeviceId<T, BT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { cookie } = self;
        write!(f, "Weak{}({:?})", T::DEBUG_TYPE, &cookie.bindings_id)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Id for BaseWeakDeviceId<T, BT> {
    fn is_loopback(&self) -> bool {
        T::IS_LOOPBACK
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> WeakId for BaseWeakDeviceId<T, BT> {
    type Strong = BaseDeviceId<T, BT>;
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> BaseWeakDeviceId<T, BT> {
    /// Attempts to upgrade the ID to a strong ID, failing if the
    /// device no longer exists.
    pub fn upgrade(&self) -> Option<BaseDeviceId<T, BT>> {
        let Self { cookie } = self;
        cookie.weak_ref.upgrade().map(|rc| BaseDeviceId { rc })
    }

    /// Returns the bindings identifier associated with the device.
    pub fn bindings_id(&self) -> &BT::DeviceIdentifier {
        &self.cookie.bindings_id
    }
}

/// A base device identifier.
///
/// Allows multiple device implementations to share the same shape for
/// maintaining reference identifiers.
#[derive(Derivative)]
#[derivative(Hash(bound = ""), Eq(bound = ""), PartialEq(bound = ""))]
pub struct BaseDeviceId<T: DeviceStateSpec, BT: DeviceLayerTypes> {
    // NB: This is not a tuple struct because regular structs play nicer with
    // type aliases, which is how we use BaseDeviceId.
    rc: StrongRc<BaseDeviceState<T, BT>>,
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Clone for BaseDeviceId<T, BT> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    fn clone(&self) -> Self {
        let Self { rc } = self;
        Self { rc: StrongRc::clone(rc) }
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> PartialEq<BaseWeakDeviceId<T, BT>>
    for BaseDeviceId<T, BT>
{
    fn eq(&self, BaseWeakDeviceId { cookie }: &BaseWeakDeviceId<T, BT>) -> bool {
        let Self { rc: me_rc } = self;
        StrongRc::weak_ptr_eq(me_rc, &cookie.weak_ref)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> PartialEq<BasePrimaryDeviceId<T, BT>>
    for BaseDeviceId<T, BT>
{
    fn eq(&self, BasePrimaryDeviceId { rc: other_rc }: &BasePrimaryDeviceId<T, BT>) -> bool {
        let Self { rc: me_rc } = self;
        PrimaryRc::ptr_eq(other_rc, me_rc)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> PartialOrd for BaseDeviceId<T, BT> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Ord for BaseDeviceId<T, BT> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let Self { rc: me } = self;
        let Self { rc: other } = other;

        StrongRc::ptr_cmp(me, other)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Debug for BaseDeviceId<T, BT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { rc } = self;
        write!(f, "{}({:?})", T::DEBUG_TYPE, &rc.weak_cookie.bindings_id)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Id for BaseDeviceId<T, BT> {
    fn is_loopback(&self) -> bool {
        T::IS_LOOPBACK
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> StrongId for BaseDeviceId<T, BT> {
    type Weak = BaseWeakDeviceId<T, BT>;
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> BaseDeviceId<T, BT> {
    pub(crate) fn device_state(&self) -> &IpLinkDeviceState<T, BT> {
        &self.rc.ip
    }

    pub(crate) fn ip_device_state(&self) -> &DualStackIpDeviceState<BT::Instant> {
        &self.rc.ip.ip
    }

    /// Returns a reference to the external state for the device.
    pub fn external_state(&self) -> &T::External<BT> {
        &self.rc.external_state
    }

    /// Returns the bindings identifier associated with the device.
    pub fn bindings_id(&self) -> &BT::DeviceIdentifier {
        &self.rc.weak_cookie.bindings_id
    }

    /// Downgrades the ID to an [`EthernetWeakDeviceId`].
    pub fn downgrade(&self) -> BaseWeakDeviceId<T, BT> {
        let Self { rc } = self;
        BaseWeakDeviceId { cookie: Arc::clone(&rc.weak_cookie) }
    }
}

/// The primary reference to a device.
pub struct BasePrimaryDeviceId<T: DeviceStateSpec, BT: DeviceLayerTypes> {
    // NB: This is not a tuple struct because regular structs play nicer with
    // type aliases, which is how we use BaseDeviceId.
    rc: PrimaryRc<BaseDeviceState<T, BT>>,
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> Debug for BasePrimaryDeviceId<T, BT> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { rc } = self;
        write!(f, "Primary{}({:?})", T::DEBUG_TYPE, &rc.weak_cookie.bindings_id)
    }
}

impl<T: DeviceStateSpec, BT: DeviceLayerTypes> BasePrimaryDeviceId<T, BT> {
    #[cfg_attr(feature = "instrumented", track_caller)]
    pub(crate) fn clone_strong(&self) -> BaseDeviceId<T, BT> {
        let Self { rc } = self;
        BaseDeviceId { rc: PrimaryRc::clone_strong(rc) }
    }

    pub(crate) fn new(
        ip: IpLinkDeviceState<T, BT>,
        external_state: T::External<BT>,
        bindings_id: BT::DeviceIdentifier,
    ) -> Self {
        Self {
            rc: PrimaryRc::new_cyclic(move |weak_ref| BaseDeviceState {
                ip,
                external_state,
                weak_cookie: Arc::new(WeakCookie { bindings_id, weak_ref }),
            }),
        }
    }

    pub(crate) fn into_inner(self) -> PrimaryRc<BaseDeviceState<T, BT>> {
        self.rc
    }
}

/// A strong device ID identifying an ethernet device.
///
/// This device ID is like [`DeviceId`] but specifically for ethernet devices.
pub type EthernetDeviceId<BT> = BaseDeviceId<EthernetLinkDevice, BT>;
/// A weak device ID identifying an ethernet device.
pub type EthernetWeakDeviceId<BT> = BaseWeakDeviceId<EthernetLinkDevice, BT>;
/// The primary Ethernet device reference.
pub(crate) type EthernetPrimaryDeviceId<BT> = BasePrimaryDeviceId<EthernetLinkDevice, BT>;
