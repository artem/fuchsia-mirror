// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The `netstack3_fuchsia` crate encapsulates Fuchsia-specific functionality
//! that is more generic than what would normally go in Bindings. This allows it
//! to be used from, for example, Core unit tests.

#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

mod inspect;

pub use inspect::{FuchsiaInspector, InspectorDeviceIdProvider};
