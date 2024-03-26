// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types to track and manage indices of packages.

mod package;
mod retained;
mod writing;

pub use package::{set_retained_index, PackageIndex};
