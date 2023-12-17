// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/fs_management/cpp/mkfs_with_default.h"

#include <lib/component/incoming/cpp/protocol.h>

#include <iostream>

#include <fbl/unique_fd.h>

#include "fidl/fuchsia.fxfs/cpp/wire_types.h"
#include "src/storage/lib/fs_management/cpp/admin.h"
#include "src/storage/lib/fs_management/cpp/mount.h"

namespace fs_management {

zx::result<> MkfsWithDefault(const char* device_path, FsComponent& component,
                             const MkfsOptions& options, zx::channel crypt_client) {
  auto status = zx::make_result(Mkfs(device_path, component, options));
  if (status.is_error())
    return status.take_error();
  zx::result device = component::Connect<fuchsia_hardware_block::Block>(device_path);
  if (device.is_error()) {
    return device.take_error();
  }
  auto fs = MountMultiVolume(std::move(device.value()), component, {});
  if (fs.is_error()) {
    std::cerr << "Could not mount to create default volume: " << fs.status_string() << std::endl;
    return fs.take_error();
  }
  auto volume = fs->CreateVolume(
      "default", fuchsia_fxfs::wire::MountOptions{
                     .crypt = fidl::ClientEnd<fuchsia_fxfs::Crypt>(std::move(crypt_client)),
                     .as_blob = false});
  if (volume.is_error()) {
    std::cerr << "Failed to create default volume: " << volume.status_string() << std::endl;
    return volume.take_error();
  }
  return zx::ok();
}

}  // namespace fs_management
