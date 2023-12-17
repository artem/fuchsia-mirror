// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/fs_test/json_filesystem.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <zircon/errors.h>

#include "src/storage/fs_test/crypt_service.h"
#include "src/storage/fs_test/fs_test.h"
#include "src/storage/lib/fs_management/cpp/mount.h"
#include "zircon/third_party/ulib/musl/include/stdlib.h"

namespace fs_test {

zx::result<std::unique_ptr<JsonFilesystem>> JsonFilesystem::NewFilesystem(
    const rapidjson::Document& config) {
  auto name = config["name"].GetString();

  auto iter = config.FindMember("binary_path");
  fs_management::DiskFormat format;
  if (iter == config.MemberEnd()) {
    format = static_cast<fs_management::DiskFormat>(config["disk_format"].GetInt64());
  } else {
    format = fs_management::CustomDiskFormat::Register(
        std::make_unique<fs_management::CustomDiskFormat>(name, config["binary_path"].GetString()));
  }
  iter = config.FindMember("sectors_per_cluster");
  const uint16_t sectors_per_cluster =
      iter == config.MemberEnd() ? 0 : static_cast<uint16_t>(iter->value.GetInt());
  return zx::ok(std::make_unique<JsonFilesystem>(
      Traits{
          .has_directory_size_limit =
              ConfigGetOrDefault<bool>(config, "has_directory_size_limit", false),
          .in_memory = ConfigGetOrDefault<bool>(config, "in_memory", false),
          .is_case_sensitive = ConfigGetOrDefault<bool>(config, "is_case_sensitive", true),
          .is_journaled = ConfigGetOrDefault<bool>(config, "is_journaled", true),
          .is_multi_volume = ConfigGetOrDefault<bool>(config, "is_multi_volume", false),
          .is_slow = ConfigGetOrDefault<bool>(config, "is_slow", false),
          .max_block_size = ConfigGetOrDefault<int64_t>(config, "max_block_size",
                                                        std::numeric_limits<int64_t>::max()),
          .max_file_size = ConfigGetOrDefault<int64_t>(config, "max_file_size",
                                                       std::numeric_limits<int64_t>::max()),
          .name = config["name"].GetString(),
          .supports_fsck_after_every_transaction =
              ConfigGetOrDefault<bool>(config, "supports_fsck_after_every_transaction", false),
          .supports_hard_links = ConfigGetOrDefault<bool>(config, "supports_hard_links", false),
          .supports_inspect = ConfigGetOrDefault<bool>(config, "supports_inspect", false),
          .supports_mmap = ConfigGetOrDefault<bool>(config, "supports_mmap", false),
          .supports_mmap_shared_write =
              ConfigGetOrDefault<bool>(config, "supports_mmap_shared_write", false),
          .supports_resize = ConfigGetOrDefault<bool>(config, "supports_resize", false),
          .supports_shutdown_on_no_connections =
              ConfigGetOrDefault<bool>(config, "supports_shutdown_on_no_connections", false),
          .supports_sparse_files = ConfigGetOrDefault<bool>(config, "supports_sparse_files", true),
          .supports_watch_event_deleted =
              ConfigGetOrDefault<bool>(config, "supports_watch_event_deleted", true),
          .timestamp_granularity = zx::nsec(config["timestamp_granularity"].GetInt64()),
          .uses_crypt = ConfigGetOrDefault<bool>(config, "uses_crypt", false),
      },
      format, sectors_per_cluster));
}

class JsonInstance : public FilesystemInstance {
 public:
  static fs_management::FsComponent GetComponent(const JsonFilesystem& filesystem) {
    const std::string& name = filesystem.GetTraits().name;
    return fs_management::FsComponent::FromUrl("#meta/" + name + ".cm",
                                               filesystem.GetTraits().is_multi_volume);
  }

  JsonInstance(const JsonFilesystem* filesystem, RamDevice device, std::string device_path)
      : filesystem_(*filesystem),
        component_(GetComponent(filesystem_)),
        device_(std::move(device)),
        device_path_(std::move(device_path)) {}

  zx::result<> Format(const TestFilesystemOptions& options) override {
    fs_management::MkfsOptions mkfs_options;
    mkfs_options.sectors_per_cluster = filesystem_.sectors_per_cluster();
    return FsFormat(device_path_, component_, mkfs_options,
                    filesystem_.GetTraits().is_multi_volume);
  }

  zx::result<> Mount(const std::string& mount_path,
                     const fs_management::MountOptions& options) override {
    fs_management::MountOptions mount_options = options;
    auto fs = FsMount(device_path_, mount_path, component_, mount_options);
    if (fs.is_error()) {
      // We can't reuse the component in the face of errors.
      component_ = GetComponent(filesystem_);
      return fs.take_error();
    }
    fs_ = std::move(fs->first);
    binding_ = std::move(fs->second);
    return zx::ok();
  }

  zx::result<> Unmount(const std::string& mount_path) override {
    zx::result result = FilesystemInstance::Unmount(mount_path);
    // Most components don't support reuse, so after unmounting, use a new component.
    component_ = GetComponent(filesystem_);
    return result;
  }

  zx::result<> Fsck() override {
    fs_management::FsckOptions options{
        .verbose = false,
        .never_modify = true,
        .always_modify = false,
        .force = true,
    };

    auto status = zx::make_result(fs_management::Fsck(device_path_.c_str(), component_, options));
    if (status.is_error()) {
      return status.take_error();
    }
    if (!filesystem_.GetTraits().is_multi_volume) {
      return zx::ok();
    }
    // Also check the volume, which requires re-mounting.
    fs_management::MountOptions mount_options{.readonly = true};
    if (filesystem_.GetTraits().uses_crypt) {
      mount_options.crypt_client = []() { return *GetCryptService(); };
    }
    zx::result device = component::Connect<fuchsia_hardware_block::Block>(device_path_);
    if (device.is_error()) {
      return device.take_error();
    }
    auto fs = fs_management::MountMultiVolume(std::move(device.value()), component_, mount_options);
    if (fs.is_error()) {
      return fs.take_error();
    }
    return fs->CheckVolume(kDefaultVolumeName, mount_options.crypt_client());
  }

  zx::result<std::string> DevicePath() const override { return zx::ok(std::string(device_path_)); }

  storage::RamDisk* GetRamDisk() override { return std::get_if<storage::RamDisk>(&device_); }

  ramdevice_client::RamNand* GetRamNand() override {
    return std::get_if<ramdevice_client::RamNand>(&device_);
  }

  fs_management::SingleVolumeFilesystemInterface* fs() override { return fs_.get(); }

  fidl::UnownedClientEnd<fuchsia_io::Directory> ServiceDirectory() const override {
    return fs_->ExportRoot();
  }

  void Reset() override {
    binding_.Reset();
    fs_.reset();
  }

  std::string GetMoniker() const override {
    return component_.collection_name().has_value()
               ? *component_.collection_name() + ":" + component_.child_name()
               : component_.child_name();
  }

 private:
  const JsonFilesystem& filesystem_;
  fs_management::FsComponent component_;
  RamDevice device_;
  std::string device_path_;
  std::unique_ptr<fs_management::SingleVolumeFilesystemInterface> fs_;
  fs_management::NamespaceBinding binding_;
};

std::unique_ptr<FilesystemInstance> JsonFilesystem::Create(RamDevice device,
                                                           std::string device_path) const {
  return std::make_unique<JsonInstance>(this, std::move(device), std::move(device_path));
}

zx::result<std::unique_ptr<FilesystemInstance>> JsonFilesystem::Open(
    const TestFilesystemOptions& options) const {
  auto result = OpenRamDevice(options);
  if (result.is_error()) {
    return result.take_error();
  }
  auto [ram_device, device_path] = std::move(result).value();
  return zx::ok(
      std::make_unique<JsonInstance>(this, std::move(ram_device), std::move(device_path)));
}

}  // namespace fs_test
