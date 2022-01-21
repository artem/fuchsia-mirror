// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! # On-disk versioning
//!
//! This module manages serialization and deserialization of FxFS structures to disk.
//!
//! In FxFS, on-disk layout is deferred to serialization libraries (i.e.
//! [bincode](https://github.com/bincode-org/bincode#is-bincode-suitable-for-storage), serde).
//! Stability of these layouts depends on both struct/enum stability and serialization libraries.
//!
//! This module provides struct/enum stability by maintaining a generation of types and a
//! means to upgrade from older versions to newer ones.
//!
//! The trait mechanism used is flexible enough to allow specific versions to use differing
//! serialization code if we ever require it.
//!
//! ## Traits
//!
//! All serialization is done with serde so [Serialize] and [Deserialize] traits must be derived
//! for all types and sub-types.
//!
//! All versioned, serializable struct/enum type should have the [Version] trait.
//! The most recent version of a type should also have the [VersionLatest] trait.
//! These traits are largely implemented for you via the `versioned_struct!` macro as follows:
//!
//! ```ignore
//! versioned_type! {
//!   1 => SuperBlockV1,
//!   2 => SuperBlockV2,
//!   3 => SuperBlockV3,
//! };
//!
//! // Note the reuse of SuperBlockRecordV1 for two versions.
//! versioned_type! {
//!   1 => SuperBlockRecordV1,
//!   2 => SuperBlockRecordV1,
//!   3 => SuperBlockRecordV2,
//! };
//! ```
//!
//! The user is required to implement [From] to migrate from one version to the next.
//! The above macros will implement further [From] traits allowing direct upgrade from any version
//! to the latest. [VersionLatest] provides a `deserialize_from_version` method that can be
//! used to deserialize any supported version and then upgrade it to the latest format.
//!
//! ## Conventions
//!
//! There are limits to how automated this process can be, so we rely heavily on conventions.
//!
//!  * Every versioned type should have a monotonically increasing `Vn` suffix for each generation.
//!  * Every versioned sub-type (e.g. child of a struct) should also have `Vn` suffix.
//!  * In type definitions, all versioned child types should be referred to explicitly with their
//!    `Vn` suffix. This prevents us from accidentally changing a version by changing a sub-type.

mod traits;

// Re-export the traits we need.
pub use serialize_macros::versioned_type;
pub use traits::*;

#[cfg(test)]
mod test_traits;

// For test use, we add [Version] and [VersionLatest] to primitive integer types.
#[cfg(test)]
pub use test_traits::*;

#[cfg(test)]
mod tests;

// TODO(ripper): Organise these better.
use crate::object_store::{
    AllocatorInfo, AllocatorKey, AllocatorValue, ExtentKey, ExtentValue, JournalRecord, ObjectKey,
    ObjectValue, StoreInfo, SuperBlock, SuperBlockRecord,
};
versioned_type! { 1 => AllocatorInfo }
versioned_type! { 1 => AllocatorKey }
versioned_type! { 1 => AllocatorValue }
versioned_type! { 1 => ExtentKey }
versioned_type! { 1 => ExtentValue }
versioned_type! { 1 => JournalRecord }
versioned_type! { 1 => ObjectKey }
versioned_type! { 1 => ObjectValue }
versioned_type! { 1 => StoreInfo }
versioned_type! { 1 => SuperBlock }
versioned_type! { 1 => SuperBlockRecord }
