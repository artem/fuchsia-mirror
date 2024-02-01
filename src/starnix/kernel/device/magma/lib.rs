// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod ffi;
mod file;
mod image_file;
mod init;
#[allow(clippy::module_inception)]
mod magma;
mod vulkan;

pub use file::MagmaFile;
pub use init::magma_device_init;
