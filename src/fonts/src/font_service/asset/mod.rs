// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(missing_docs)]

mod asset;
mod cache;
mod collection;
mod loader;

pub use {
    asset::{Asset, AssetId},
    collection::{AssetCollection, AssetCollectionBuilder},
    loader::{AssetLoader, AssetLoaderImpl},
};

#[cfg(test)]
pub(crate) use collection::AssetCollectionError;
