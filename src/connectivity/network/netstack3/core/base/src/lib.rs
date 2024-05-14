// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Base type definitions for netstack3 core.
//!
//! This crate contains definitions common to the other netstack3 core crates
//! and is the base dependency for most of them.

#![no_std]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;
extern crate fakestd as std;

mod context;
mod convert;
mod counters;
mod data_structures;
mod device;
mod error;
mod event;
mod frame;
mod inspect;
mod port_alloc;
mod resource_references;
mod rng;
mod time;
mod trace;
mod uninstantiable;
mod work_queue;

pub use context::{BuildableCoreContext, ContextPair, ContextProvider, CtxPair};
pub use convert::{BidirectionalConverter, OwnedOrRefsBidirectionalConverter};
pub use counters::{Counter, CounterContext, ResourceCounterContext};
pub use data_structures::token_bucket::TokenBucket;
pub use device::{
    AnyDevice, Device, DeviceIdAnyCompatContext, DeviceIdContext, DeviceIdentifier,
    StrongDeviceIdentifier, WeakDeviceIdentifier,
};
pub use error::{
    AddressResolutionFailed, ExistsError, LocalAddressError, NotFoundError, NotSupportedError,
    RemoteAddressError, SocketError, ZonedAddressError,
};
pub use event::{CoreEventContext, EventContext};
pub use frame::{ReceivableFrameMeta, RecvFrameContext, SendFrameContext, SendableFrameMeta};
pub use inspect::{Inspectable, InspectableValue, Inspector, InspectorDeviceExt};
pub use port_alloc::{simple_randomized_port_alloc, EphemeralPort, PortAllocImpl};
pub use resource_references::{
    DeferredResourceRemovalContext, ReferenceNotifiers, RemoveResourceResult,
    RemoveResourceResultWithContext,
};
pub use rng::RngContext;
pub use time::{
    local_timer_heap::LocalTimerHeap, CoreTimerContext, HandleableTimer, Instant,
    InstantBindingsTypes, InstantContext, IntoCoreTimerCtx, NestedIntoCoreTimerCtx,
    TimerBindingsTypes, TimerContext, TimerHandler,
};
pub use trace::TracingContext;
pub use uninstantiable::{Uninstantiable, UninstantiableWrapper};
pub use work_queue::WorkQueueReport;

/// Reference counted hash map data structure.
pub mod ref_counted_hash_map {
    pub use crate::data_structures::ref_counted_hash_map::{
        InsertResult, RefCountedHashMap, RefCountedHashSet, RemoveResult,
    };
}

/// Defines generic data structures used to implement common application socket
/// functionality for multiple protocols.
pub mod socketmap {
    pub use crate::data_structures::socketmap::{
        Entry, IterShadows, OccupiedEntry, SocketMap, Tagged, VacantEntry,
    };
}

/// Sync utilities common to netstack3.
pub mod sync {
    // TODO(https://fxbug.dev/42062225): Support single-threaded variants of
    // types exported from this module.

    // Exclusively re-exports from the sync crate.
    pub use netstack3_sync::{
        rc::{
            DebugReferences, DynDebugReferences, MapNotifier as MapRcNotifier,
            Notifier as RcNotifier, Primary as PrimaryRc, Strong as StrongRc, Weak as WeakRc,
        },
        LockGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    };
}

/// Test utilities provided to all crates.
#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    mod addr;
    mod benchmarks;
    mod fake_bindings;
    mod fake_core;
    mod fake_network;
    mod misc;
    mod monotonic_id;

    pub use crate::device::testutil::{
        FakeDeviceId, FakeReferencyDeviceId, FakeStrongDeviceId, FakeWeakDeviceId,
        MultipleDevicesId,
    };
    pub use crate::event::testutil::FakeEventCtx;
    pub use crate::frame::testutil::{FakeFrameCtx, WithFakeFrameContext};
    pub use crate::rng::testutil::{new_rng, run_with_many_seeds, FakeCryptoRng};
    pub use crate::time::testutil::{
        FakeInstant, FakeInstantCtx, FakeTimerCtx, FakeTimerCtxExt, InstantAndData,
        WithFakeTimerContext,
    };
    pub use crate::trace::testutil::FakeTracingCtx;
    pub use addr::{TestAddrs, TestIpExt, TEST_ADDRS_V4, TEST_ADDRS_V6};
    pub use benchmarks::{Bencher, RealBencher, TestBencher};
    pub use fake_bindings::FakeBindingsCtx;
    pub use fake_core::FakeCoreCtx;
    pub use fake_network::{
        FakeNetwork, FakeNetworkLinks, FakeNetworkSpec, PendingFrame, PendingFrameData, StepResult,
    };
    pub use misc::{assert_empty, set_logger_for_test};
    pub use monotonic_id::MonotonicIdentifier;
}

/// Benchmarks defined in the base crate.
#[cfg(benchmark)]
pub mod benchmarks {
    /// Adds benchmarks defined in the base crate to the provided benchmarker.
    pub fn add_benches(b: criterion::Benchmark) -> criterion::Benchmark {
        crate::data_structures::token_bucket::benchmarks::add_benches(b)
    }
}
