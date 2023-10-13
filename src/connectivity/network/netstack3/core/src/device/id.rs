// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common device identifier types.

use alloc::sync::Arc;
use core::fmt::{self, Debug};
use core::hash::Hash;

use derivative::Derivative;

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
pub enum WeakDeviceId<C: DeviceLayerTypes> {
    Ethernet(EthernetWeakDeviceId<C>),
    Loopback(LoopbackWeakDeviceId<C>),
}

impl<C: DeviceLayerTypes> PartialEq<DeviceId<C>> for WeakDeviceId<C> {
    fn eq(&self, other: &DeviceId<C>) -> bool {
        <DeviceId<C> as PartialEq<WeakDeviceId<C>>>::eq(other, self)
    }
}

impl<C: DeviceLayerTypes> From<EthernetWeakDeviceId<C>> for WeakDeviceId<C> {
    fn from(id: EthernetWeakDeviceId<C>) -> WeakDeviceId<C> {
        WeakDeviceId::Ethernet(id)
    }
}

impl<C: DeviceLayerTypes> From<LoopbackWeakDeviceId<C>> for WeakDeviceId<C> {
    fn from(id: LoopbackWeakDeviceId<C>) -> WeakDeviceId<C> {
        WeakDeviceId::Loopback(id)
    }
}

impl<C: DeviceLayerTypes> WeakDeviceId<C> {
    /// Attempts to upgrade the ID.
    pub fn upgrade(&self) -> Option<DeviceId<C>> {
        match self {
            WeakDeviceId::Ethernet(id) => id.upgrade().map(Into::into),
            WeakDeviceId::Loopback(id) => id.upgrade().map(Into::into),
        }
    }

    /// Creates a [`DebugReferences`] instance for this device.
    pub fn debug_references(&self) -> DebugReferences<C> {
        DebugReferences(match self {
            Self::Loopback(LoopbackWeakDeviceId { cookie }) => {
                DebugReferencesInner::Loopback(cookie.weak_ref.debug_references())
            }
            Self::Ethernet(EthernetWeakDeviceId { cookie }) => {
                DebugReferencesInner::Ethernet(cookie.weak_ref.debug_references())
            }
        })
    }
}

enum DebugReferencesInner<C: DeviceLayerTypes> {
    Loopback(crate::sync::DebugReferences<BaseDeviceState<LoopbackDevice, C>>),
    Ethernet(crate::sync::DebugReferences<BaseDeviceState<EthernetLinkDevice, C>>),
}

/// A type offering a [`Debug`] implementation that helps debug dangling device
/// references.
pub struct DebugReferences<C: DeviceLayerTypes>(DebugReferencesInner<C>);

impl<C: DeviceLayerTypes> Debug for DebugReferences<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(inner) = self;
        match inner {
            DebugReferencesInner::Loopback(d) => write!(f, "Loopback({d:?})"),
            DebugReferencesInner::Ethernet(d) => write!(f, "Ethernet({d:?})"),
        }
    }
}

impl<C: DeviceLayerTypes> Id for WeakDeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self {
            WeakDeviceId::Loopback(_) => true,
            WeakDeviceId::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerTypes> WeakId for WeakDeviceId<C> {
    type Strong = DeviceId<C>;
}

impl<C: DeviceLayerTypes> Debug for WeakDeviceId<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WeakDeviceId::Ethernet(id) => Debug::fmt(id, f),
            WeakDeviceId::Loopback(id) => Debug::fmt(id, f),
        }
    }
}

/// A strong ID identifying a device.
///
/// Holders may safely assume that the underlying device is "alive" in the sense
/// that the device is still recognized by the stack. That is, operations that
/// use this device ID will never fail as a result of "unrecognized device"-like
/// errors.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[allow(missing_docs)]
pub enum DeviceId<C: DeviceLayerTypes> {
    Ethernet(EthernetDeviceId<C>),
    Loopback(LoopbackDeviceId<C>),
}

impl<C: DeviceLayerTypes> PartialEq<WeakDeviceId<C>> for DeviceId<C> {
    fn eq(&self, other: &WeakDeviceId<C>) -> bool {
        match (self, other) {
            (DeviceId::Ethernet(strong), WeakDeviceId::Ethernet(weak)) => strong == weak,
            (DeviceId::Loopback(strong), WeakDeviceId::Loopback(weak)) => strong == weak,
            (DeviceId::Loopback(_), WeakDeviceId::Ethernet(_))
            | (DeviceId::Ethernet(_), WeakDeviceId::Loopback(_)) => false,
        }
    }
}

impl<C: DeviceLayerTypes> PartialOrd for DeviceId<C> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<C: DeviceLayerTypes> Ord for DeviceId<C> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        match (self, other) {
            (DeviceId::Ethernet(me), DeviceId::Ethernet(other)) => me.cmp(other),
            (DeviceId::Loopback(me), DeviceId::Loopback(other)) => me.cmp(other),
            (DeviceId::Loopback(_), DeviceId::Ethernet(_)) => core::cmp::Ordering::Less,
            (DeviceId::Ethernet(_), DeviceId::Loopback(_)) => core::cmp::Ordering::Greater,
        }
    }
}

impl<C: DeviceLayerTypes> From<EthernetDeviceId<C>> for DeviceId<C> {
    fn from(id: EthernetDeviceId<C>) -> DeviceId<C> {
        DeviceId::Ethernet(id)
    }
}

impl<C: DeviceLayerTypes> From<LoopbackDeviceId<C>> for DeviceId<C> {
    fn from(id: LoopbackDeviceId<C>) -> DeviceId<C> {
        DeviceId::Loopback(id)
    }
}

impl<C: DeviceLayerTypes> DeviceId<C> {
    /// Downgrade to a [`WeakDeviceId`].
    pub fn downgrade(&self) -> WeakDeviceId<C> {
        match self {
            DeviceId::Ethernet(id) => id.downgrade().into(),
            DeviceId::Loopback(id) => id.downgrade().into(),
        }
    }
}

impl<C: DeviceLayerTypes> Id for DeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self {
            DeviceId::Loopback(_) => true,
            DeviceId::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerTypes> StrongId for DeviceId<C> {
    type Weak = WeakDeviceId<C>;
}

impl<C: DeviceLayerTypes> Debug for DeviceId<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceId::Ethernet(id) => Debug::fmt(id, f),
            DeviceId::Loopback(id) => Debug::fmt(id, f),
        }
    }
}

/// A base weak device identifier.
///
/// Allows multiple device implementations to share the same shape for
/// maintaining reference identifiers.
#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub struct BaseWeakDeviceId<T: DeviceStateSpec, C: DeviceLayerTypes> {
    // NB: This is not a tuple struct because regular structs play nicer with
    // type aliases, which is how we use BaseDeviceId.
    cookie: Arc<WeakCookie<T, C>>,
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> PartialEq for BaseWeakDeviceId<T, C> {
    fn eq(&self, other: &Self) -> bool {
        self.cookie.weak_ref.ptr_eq(&other.cookie.weak_ref)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Eq for BaseWeakDeviceId<T, C> {}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Hash for BaseWeakDeviceId<T, C> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.cookie.weak_ref.hash(state)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> PartialEq<BaseDeviceId<T, C>>
    for BaseWeakDeviceId<T, C>
{
    fn eq(&self, other: &BaseDeviceId<T, C>) -> bool {
        <BaseDeviceId<T, C> as PartialEq<BaseWeakDeviceId<T, C>>>::eq(other, self)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Debug for BaseWeakDeviceId<T, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { cookie } = self;
        write!(f, "Weak{}({:?})", T::DEBUG_TYPE, &cookie.bindings_id)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Id for BaseWeakDeviceId<T, C> {
    fn is_loopback(&self) -> bool {
        T::IS_LOOPBACK
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> WeakId for BaseWeakDeviceId<T, C> {
    type Strong = BaseDeviceId<T, C>;
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> BaseWeakDeviceId<T, C> {
    /// Attempts to upgrade the ID to a strong ID, failing if the
    /// device no longer exists.
    pub fn upgrade(&self) -> Option<BaseDeviceId<T, C>> {
        let Self { cookie } = self;
        cookie.weak_ref.upgrade().map(|rc| BaseDeviceId { rc })
    }
}

/// A base device identifier.
///
/// Allows multiple device implementations to share the same shape for
/// maintaining reference identifiers.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""), Eq(bound = ""), PartialEq(bound = ""))]
pub struct BaseDeviceId<T: DeviceStateSpec, C: DeviceLayerTypes> {
    // NB: This is not a tuple struct because regular structs play nicer with
    // type aliases, which is how we use BaseDeviceId.
    rc: StrongRc<BaseDeviceState<T, C>>,
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> PartialEq<BaseWeakDeviceId<T, C>>
    for BaseDeviceId<T, C>
{
    fn eq(&self, BaseWeakDeviceId { cookie }: &BaseWeakDeviceId<T, C>) -> bool {
        let Self { rc: me_rc } = self;
        StrongRc::weak_ptr_eq(me_rc, &cookie.weak_ref)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> PartialEq<BasePrimaryDeviceId<T, C>>
    for BaseDeviceId<T, C>
{
    fn eq(&self, BasePrimaryDeviceId { rc: other_rc }: &BasePrimaryDeviceId<T, C>) -> bool {
        let Self { rc: me_rc } = self;
        PrimaryRc::ptr_eq(other_rc, me_rc)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> PartialOrd for BaseDeviceId<T, C> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Ord for BaseDeviceId<T, C> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let Self { rc: me } = self;
        let Self { rc: other } = other;

        StrongRc::ptr_cmp(me, other)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Debug for BaseDeviceId<T, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { rc } = self;
        write!(f, "{}({:?})", T::DEBUG_TYPE, &rc.weak_cookie.bindings_id)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Id for BaseDeviceId<T, C> {
    fn is_loopback(&self) -> bool {
        T::IS_LOOPBACK
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> StrongId for BaseDeviceId<T, C> {
    type Weak = BaseWeakDeviceId<T, C>;
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> BaseDeviceId<T, C> {
    pub(crate) fn device_state(&self) -> &IpLinkDeviceState<T, C> {
        &self.rc.ip
    }
    /// Returns a reference to the external state for the device.
    pub fn external_state(&self) -> &T::External<C> {
        &self.rc.external_state
    }

    /// Downgrades the ID to an [`EthernetWeakDeviceId`].
    pub fn downgrade(&self) -> BaseWeakDeviceId<T, C> {
        let Self { rc } = self;
        BaseWeakDeviceId { cookie: Arc::clone(&rc.weak_cookie) }
    }
}

/// The primary reference to a device.
pub(crate) struct BasePrimaryDeviceId<T: DeviceStateSpec, C: DeviceLayerTypes> {
    // NB: This is not a tuple struct because regular structs play nicer with
    // type aliases, which is how we use BaseDeviceId.
    rc: PrimaryRc<BaseDeviceState<T, C>>,
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> Debug for BasePrimaryDeviceId<T, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { rc } = self;
        write!(f, "Primary{}({:?})", T::DEBUG_TYPE, &rc.weak_cookie.bindings_id)
    }
}

impl<T: DeviceStateSpec, C: DeviceLayerTypes> BasePrimaryDeviceId<T, C> {
    pub(crate) fn clone_strong(&self) -> BaseDeviceId<T, C> {
        let Self { rc } = self;
        BaseDeviceId { rc: PrimaryRc::clone_strong(rc) }
    }

    pub(crate) fn new(
        ip: IpLinkDeviceState<T, C>,
        external_state: T::External<C>,
        bindings_id: C::DeviceIdentifier,
    ) -> Self {
        Self {
            rc: PrimaryRc::new_cyclic(move |weak_ref| BaseDeviceState {
                ip,
                external_state,
                weak_cookie: Arc::new(WeakCookie { bindings_id, weak_ref }),
            }),
        }
    }

    pub(crate) fn into_inner(self) -> PrimaryRc<BaseDeviceState<T, C>> {
        self.rc
    }
}

/// A strong device ID identifying an ethernet device.
///
/// This device ID is like [`DeviceId`] but specifically for ethernet devices.
pub type EthernetDeviceId<C> = BaseDeviceId<EthernetLinkDevice, C>;
/// A weak device ID identifying an ethernet device.
pub type EthernetWeakDeviceId<C> = BaseWeakDeviceId<EthernetLinkDevice, C>;
/// The primary Ethernet device reference.
pub(crate) type EthernetPrimaryDeviceId<C> = BasePrimaryDeviceId<EthernetLinkDevice, C>;
