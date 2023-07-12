// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod child_moniker;
mod error;
mod extended_moniker;
mod moniker;
#[cfg(feature = "serde")]
mod serde_ext;

pub use self::{
    child_moniker::{ChildMoniker, ChildMonikerBase},
    error::MonikerError,
    extended_moniker::ExtendedMoniker,
    moniker::{Moniker, MonikerBase},
};
