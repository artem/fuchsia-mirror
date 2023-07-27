// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the implementation of FxBlob (Blobfs-on-Fxfs).

pub mod blob;
mod directory;
pub mod reader;
mod writer;

#[cfg(test)]
mod testing;

pub use crate::fxblob::{blob::init_vmex_resource, directory::BlobDirectory};
