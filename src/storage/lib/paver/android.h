// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_ANDROID_H_
#define SRC_STORAGE_LIB_PAVER_ANDROID_H_

#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/gpt.h"

namespace paver {

// DevicePartitioner implementation for Android devices.
class AndroidDevicePartitioner : public DevicePartitioner {
 public:
  static zx::result<std::unique_ptr<DevicePartitioner>> Initialize(
      fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
      fidl::ClientEnd<fuchsia_device::Controller> block_device, std::shared_ptr<Context> context);

  bool IsFvmWithinFtl() const override { return false; }

  bool SupportsPartition(const PartitionSpec& spec) const override;

  zx::result<std::unique_ptr<PartitionClient>> AddPartition(
      const PartitionSpec& spec) const override;

  zx::result<std::unique_ptr<PartitionClient>> FindPartition(
      const PartitionSpec& spec) const override;

  zx::result<> FinalizePartition(const PartitionSpec& spec) const override;

  zx::result<> WipeFvm() const override;

  zx::result<> InitPartitionTables() const override;

  zx::result<> WipePartitionTables() const override;

  zx::result<> ValidatePayload(const PartitionSpec& spec,
                               cpp20::span<const uint8_t> data) const override;

  zx::result<> Flush() const override { return zx::ok(); }

  zx::result<> OnStop() const override;

 private:
  AndroidDevicePartitioner(Arch arch, std::unique_ptr<GptDevicePartitioner> gpt,
                           std::shared_ptr<Context> context)
      : gpt_(std::move(gpt)), arch_(arch), context_(context) {}

  std::unique_ptr<GptDevicePartitioner> gpt_;
  Arch arch_;
  std::shared_ptr<Context> context_;
};

class AndroidPartitionerFactory : public DevicePartitionerFactory {
 public:
  zx::result<std::unique_ptr<DevicePartitioner>> New(
      fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
      std::shared_ptr<Context> context,
      fidl::ClientEnd<fuchsia_device::Controller> block_device) final;
};

class AndroidAbrClientFactory : public abr::ClientFactory {
 public:
  zx::result<std::unique_ptr<abr::Client>> New(
      fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
      std::shared_ptr<paver::Context> context) final;
};

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_ANDROID_H_
