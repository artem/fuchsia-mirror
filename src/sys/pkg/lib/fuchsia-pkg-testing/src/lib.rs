// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test tools for building Fuchsia packages and TUF repositories.

#![allow(clippy::let_unit_value)]
#![deny(missing_docs)]

mod package;
pub use crate::package::{BlobContents, Package, PackageBuilder, PackageDir, VerificationError};

mod repo;
pub use crate::repo::{PackageEntry, Repository, RepositoryBuilder};
pub mod serve;

mod inspect;
pub use crate::inspect::get_inspect_hierarchy;

mod system_image;
pub use crate::system_image::SystemImageBuilder;

mod update_package;
pub use crate::update_package::{
    make_current_epoch_json, make_epoch_json, make_packages_json, FakeUpdatePackage, UpdatePackage,
    UpdatePackageBuilder, SOURCE_EPOCH,
};

pub mod blobfs;
