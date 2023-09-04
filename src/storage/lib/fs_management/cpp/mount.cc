// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/fs_management/cpp/mount.h"

#include <errno.h>
#include <fidl/fuchsia.component/cpp/wire.h>
#include <fidl/fuchsia.fs.startup/cpp/wire.h>
#include <fidl/fuchsia.fs/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/limits.h>
#include <lib/fdio/namespace.h>
#include <lib/fdio/vfs.h>
#include <lib/fit/defer.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <string.h>
#include <unistd.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

#include <iostream>
#include <memory>
#include <utility>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <pretty/hexdump.h>

#include "fidl/fuchsia.io/cpp/markers.h"
#include "src/storage/lib/fs_management/cpp/admin.h"
#include "src/storage/lib/fs_management/cpp/component.h"
#include "src/storage/lib/fs_management/cpp/format.h"
#include "src/storage/lib/fs_management/cpp/options.h"
#include "src/storage/lib/fs_management/cpp/volumes.h"

namespace fs_management {
namespace {

using Directory = fuchsia_io::Directory;

zx::result<> StartFsComponent(fidl::UnownedClientEnd<Directory> exposed_dir,
                              fidl::ClientEnd<fuchsia_hardware_block::Block> device,
                              const MountOptions& options) {
  auto startup_client_end = component::ConnectAt<fuchsia_fs_startup::Startup>(exposed_dir);
  if (startup_client_end.is_error())
    return startup_client_end.take_error();
  fidl::WireSyncClient startup_client{std::move(*startup_client_end)};

  auto start_options_or = options.as_start_options();
  if (start_options_or.is_error())
    return start_options_or.take_error();

  auto res = startup_client->Start(std::move(device), std::move(*start_options_or));
  if (!res.ok())
    return zx::error(res.status());
  if (res->is_error())
    return zx::error(res->error_value());

  return zx::ok();
}

zx::result<fidl::ClientEnd<Directory>> InitFsComponent(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device, FsComponent& component,
    const MountOptions& options) {
  auto exposed_dir = component.Connect();
  if (exposed_dir.is_error())
    return exposed_dir.take_error();
  if (zx::result<> start_status = StartFsComponent(*exposed_dir, std::move(device), options);
      start_status.is_error()) {
    return start_status.take_error();
  }
  return exposed_dir;
}

std::string StripTrailingSlash(const char* in) {
  if (!in)
    return std::string();
  std::string_view view(in, strlen(in));
  if (!view.empty() && view.back() == '/') {
    return std::string(view.substr(0, view.length() - 1));
  }
  return std::string(view);
}

}  // namespace

__EXPORT zx::result<NamespaceBinding> NamespaceBinding::Create(
    const char* path, fidl::ClientEnd<fuchsia_io::Directory> dir) {
  auto stripped_path = StripTrailingSlash(path);
  if (stripped_path.empty()) {
    return zx::ok(NamespaceBinding());
  }
  fdio_ns_t* ns;
  if (zx_status_t status = fdio_ns_get_installed(&ns); status != ZX_OK)
    return zx::error(status);
  if (zx_status_t status = fdio_ns_bind(ns, stripped_path.c_str(), dir.TakeHandle().release());
      status != ZX_OK)
    return zx::error(status);
  return zx::ok(NamespaceBinding(std::move(stripped_path)));
}

__EXPORT void NamespaceBinding::Reset() {
  if (!path_.empty()) {
    fdio_ns_t* ns;
    if (fdio_ns_get_installed(&ns) == ZX_OK)
      fdio_ns_unbind(ns, path_.c_str());
    path_.clear();
  }
}

__EXPORT NamespaceBinding::~NamespaceBinding() { Reset(); }

__EXPORT
StartedSingleVolumeFilesystem::~StartedSingleVolumeFilesystem() {
  [[maybe_unused]] auto res = Unmount();
}

__EXPORT
fidl::ClientEnd<fuchsia_io::Directory> StartedSingleVolumeFilesystem::Release() {
  return fidl::ClientEnd<fuchsia_io::Directory>(export_root_.TakeChannel());
}

__EXPORT
zx::result<> StartedSingleVolumeFilesystem::Unmount() {
  auto res = Shutdown(ExportRoot());
  export_root_.reset();
  return res;
}

__EXPORT
zx::result<fidl::ClientEnd<fuchsia_io::Directory>> StartedSingleVolumeFilesystem::DataRoot() const {
  return FsRootHandle(export_root_);
}

__EXPORT
fidl::ClientEnd<fuchsia_io::Directory> MountedVolume::Release() {
  return fidl::ClientEnd<fuchsia_io::Directory>(export_root_.TakeChannel());
}

__EXPORT
zx::result<fidl::ClientEnd<fuchsia_io::Directory>> MountedVolume::DataRoot() const {
  return FsRootHandle(export_root_);
}

__EXPORT
StartedMultiVolumeFilesystem::~StartedMultiVolumeFilesystem() {
  [[maybe_unused]] auto res = Unmount();
}

__EXPORT
std::pair<fidl::ClientEnd<fuchsia_io::Directory>,
          std::map<std::string, fidl::ClientEnd<fuchsia_io::Directory>>>
StartedMultiVolumeFilesystem::Release() {
  std::map<std::string, fidl::ClientEnd<fuchsia_io::Directory>> volumes;
  for (auto&& [k, v] : std::move(volumes_)) {
    volumes.insert(std::make_pair(k, v.Release()));
  }
  return std::make_pair(fidl::ClientEnd<fuchsia_io::Directory>(exposed_dir_.TakeChannel()),
                        std::move(volumes));
}

__EXPORT
zx::result<> StartedMultiVolumeFilesystem::Unmount() {
  volumes_.clear();
  auto res = Shutdown(exposed_dir_);
  exposed_dir_.reset();
  return res;
}

__EXPORT
zx::result<MountedVolume*> StartedMultiVolumeFilesystem::OpenVolume(
    std::string_view name, fuchsia_fxfs::wire::MountOptions options) {
  if (volumes_.find(name) != volumes_.end()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }
  auto endpoints_or = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints_or.is_error())
    return endpoints_or.take_error();
  auto [client, server] = std::move(*endpoints_or);
  auto res = fs_management::OpenVolume(exposed_dir_, name, std::move(server), std::move(options));
  if (res.is_error()) {
    return res.take_error();
  }
  auto [iter, inserted] = volumes_.emplace(std::string(name), MountedVolume(std::move(client)));
  return zx::ok(&iter->second);
}

__EXPORT
zx::result<MountedVolume*> StartedMultiVolumeFilesystem::CreateVolume(
    std::string_view name, fuchsia_fxfs::wire::MountOptions options) {
  if (volumes_.find(name) != volumes_.end()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }
  auto endpoints_or = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints_or.is_error())
    return endpoints_or.take_error();
  auto [client, server] = std::move(*endpoints_or);
  auto res = fs_management::CreateVolume(exposed_dir_, name, std::move(server), std::move(options));
  if (res.is_error()) {
    return res.take_error();
  }
  auto [iter, inserted] = volumes_.emplace(std::string(name), MountedVolume(std::move(client)));
  return zx::ok(&iter->second);
}

__EXPORT zx::result<> StartedMultiVolumeFilesystem::CheckVolume(std::string_view volume_name,
                                                                zx::channel crypt_client) {
  return fs_management::CheckVolume(exposed_dir_, volume_name, std::move(crypt_client));
}

__EXPORT bool StartedMultiVolumeFilesystem::HasVolume(std::string_view volume_name) {
  if (volumes_.find(volume_name) != volumes_.end()) {
    return true;
  }
  return fs_management::HasVolume(exposed_dir_, volume_name);
}

__EXPORT
StartedSingleVolumeMultiVolumeFilesystem::~StartedSingleVolumeMultiVolumeFilesystem() {
  [[maybe_unused]] auto res = Unmount();
}

__EXPORT
fidl::ClientEnd<fuchsia_io::Directory> StartedSingleVolumeMultiVolumeFilesystem::Release() {
  volume_.reset();
  return fidl::ClientEnd<fuchsia_io::Directory>(exposed_dir_.TakeChannel());
}

__EXPORT
zx::result<> StartedSingleVolumeMultiVolumeFilesystem::Unmount() {
  volume_.reset();
  auto res = Shutdown(exposed_dir_);
  exposed_dir_.reset();
  return res;
}

__EXPORT SingleVolumeFilesystemInterface::~SingleVolumeFilesystemInterface() = default;

__EXPORT
zx::result<StartedSingleVolumeFilesystem> Mount(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device, FsComponent& component,
    const MountOptions& options) {
  if (component.is_multi_volume()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  StartedSingleVolumeFilesystem fs;

  auto exposed_dir = InitFsComponent(std::move(device), component, options);
  if (exposed_dir.is_error()) {
    return exposed_dir.take_error();
  }

  return zx::ok(StartedSingleVolumeFilesystem(std::move(*exposed_dir)));
}

__EXPORT
zx::result<StartedMultiVolumeFilesystem> MountMultiVolume(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device, FsComponent& component,
    const MountOptions& options) {
  if (!component.is_multi_volume()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  auto outgoing_dir = InitFsComponent(std::move(device), component, options);
  if (outgoing_dir.is_error()) {
    return outgoing_dir.take_error();
  }
  return zx::ok(StartedMultiVolumeFilesystem(std::move(*outgoing_dir)));
}

__EXPORT
zx::result<StartedSingleVolumeMultiVolumeFilesystem> MountMultiVolumeWithDefault(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device, FsComponent& component,
    const MountOptions& options, const char* volume_name) {
  if (!component.is_multi_volume()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  auto outgoing_dir_or = InitFsComponent(std::move(device), component, options);
  if (outgoing_dir_or.is_error()) {
    return outgoing_dir_or.take_error();
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto [client, server] = std::move(*endpoints);

  auto volume = OpenVolume(
      *outgoing_dir_or, volume_name, std::move(server),
      options.crypt_client
          ? fuchsia_fxfs::wire::MountOptions{.crypt = fidl::ClientEnd<fuchsia_fxfs::Crypt>(
                                                 options.crypt_client())}
          : fuchsia_fxfs::wire::MountOptions());

  if (volume.is_error()) {
    std::cerr << "Volume status " << volume.status_string() << std::endl;
    return volume.take_error();
  }

  return zx::ok(StartedSingleVolumeMultiVolumeFilesystem(std::move(*outgoing_dir_or),
                                                         MountedVolume(std::move(client))));
}

__EXPORT
zx::result<> Shutdown(fidl::UnownedClientEnd<Directory> svc_dir) {
  auto admin_or = component::ConnectAt<fuchsia_fs::Admin>(svc_dir);
  if (admin_or.is_error()) {
    return admin_or.take_error();
  }

  auto resp = fidl::WireCall(*admin_or)->Shutdown();
  if (resp.status() != ZX_OK)
    return zx::error(resp.status());
  return zx::ok();
}

}  // namespace fs_management
