// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/virtualization/bin/guest_manager/guest_manager.h"

#include <fuchsia/virtualization/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>

#include <unordered_set>

#include <src/lib/files/file.h>

GuestManager::GuestManager(async_dispatcher_t* dispatcher, sys::ComponentContext* context,
                           std::string config_pkg_dir_path, std::string config_path,
                           bool use_legacy_vsock_device)
    : context_(context),
      config_pkg_dir_path_(std::move(config_pkg_dir_path)),
      config_path_(std::move(config_path)),
      use_legacy_vsock_device_(use_legacy_vsock_device),
      host_vsock_endpoint_(dispatcher, fit::bind_member(this, &GuestManager::GetAcceptor)) {
  context_->outgoing()->AddPublicService(manager_bindings_.GetHandler(this));
  context_->outgoing()->AddPublicService(guest_config_bindings_.GetHandler(this));
}

fuchsia::virtualization::GuestVsockAcceptor* GuestManager::GetAcceptor(uint32_t cid) {
  GuestVsockEndpoint* res = nullptr;
  if (cid == fuchsia::virtualization::DEFAULT_GUEST_CID) {
    res = local_guest_endpoint_.get();
  }
  return res;
}

// |fuchsia::virtualization::GuestManager|
void GuestManager::LaunchGuest(
    fuchsia::virtualization::GuestConfig guest_config,
    fidl::InterfaceRequest<fuchsia::virtualization::Guest> controller,
    fuchsia::virtualization::GuestManager::LaunchGuestCallback callback) {
  if (guest_started_) {
    callback(fpromise::error(ZX_ERR_ALREADY_EXISTS));
    return;
  }
  guest_started_ = true;
  guest_config_ = std::move(guest_config);
  // Reads guest config from [zircon|termina|debina]_guest package provided as child in
  // [zircon|termina|debian]_guest_manager component hierarchy. Applies overrides from the
  // user_guest_config_ which was provided by LaunchGuest function
  //
  auto block_devices = std::move(*guest_config_.mutable_block_devices());

  const std::string config_path = config_pkg_dir_path_ + config_path_;

  auto open_at = [&](const std::string& path, fidl::InterfaceRequest<fuchsia::io::File> file) {
    return fdio_open((config_pkg_dir_path_ + path).c_str(),
                     static_cast<uint32_t>(fuchsia::io::OpenFlags::RIGHT_READABLE),
                     file.TakeChannel().release());
  };

  std::string content;
  bool readFileSuccess = files::ReadFileToString(config_path, &content);
  if (!readFileSuccess) {
    FX_LOGS(ERROR) << "Failed to read guest configuration " << config_path;
    callback(fpromise::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  zx_status_t status = guest_config::ParseConfig(content, std::move(open_at), &guest_config_);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to parse guest configuration " << config_path;
    callback(fpromise::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  // Make sure that block devices provided by the configuration in the guest's
  // package take precedence, as the order matters.
  for (auto& block_device : block_devices) {
    guest_config_.mutable_block_devices()->emplace_back(std::move(block_device));
  }
  // Merge the command-line additions into the main kernel command-line field.
  for (auto& cmdline : *guest_config_.mutable_cmdline_add()) {
    guest_config_.mutable_cmdline()->append(" " + cmdline);
  }
  guest_config_.clear_cmdline_add();
  // Set any defaults, before returning the configuration.
  guest_config::SetDefaults(&guest_config_);

  // If there are any initial vsock listeners, they must be bound to unique host ports.
  if (guest_config_.vsock_listeners().size() > 1) {
    std::unordered_set<uint32_t> ports;
    for (auto& listener : guest_config_.vsock_listeners()) {
      if (!ports.insert(listener.port).second) {
        callback(fpromise::error(ZX_ERR_INVALID_ARGS));
        return;
      }
    }
  }

  if (use_legacy_vsock_device_) {
    // Initial vsock listeners aren't passed to the VMM via config when using the legacy vsock
    // device, as they are instead handled in the guest manager.
    for (auto& listener : *guest_config_.mutable_vsock_listeners()) {
      zx_status_t status;
      bool callback_complete = false;
      host_vsock_endpoint_.Listen(
          listener.port, std::move(listener.acceptor),
          [&status, &callback_complete](
              fuchsia::virtualization::HostVsockEndpoint_Listen_Result result) mutable {
            callback_complete = true;
            status = result.is_err() ? result.err() : ZX_OK;
          });

      FX_CHECK(callback_complete) << "Expected Listen to complete synchronously";
      if (status != ZX_OK) {
        callback(fpromise::error(status));
        return;
      }
    }

    // Clear any listeners and reset to the vector to a default value. The VMM will check that
    // this vector is empty when using the legacy vsock device.
    guest_config_.clear_vsock_listeners();
    guest_config_.mutable_vsock_listeners();
  }

  // Connect call will cause componont framework to start VMM and execute VMM's main which will
  // bring up all virtio devices and query guest_config via the
  // fuchsia::virtualization::GuestConfigProvider::Get. Note that this must happen after the
  // config is finalized.
  context_->svc()->Connect(std::move(controller));

  if (use_legacy_vsock_device_) {
    fuchsia::virtualization::GuestVsockEndpointPtr vm_guest_endpoint;
    context_->svc()->Connect(vm_guest_endpoint.NewRequest());
    local_guest_endpoint_ =
        std::make_unique<GuestVsockEndpoint>(fuchsia::virtualization::DEFAULT_GUEST_CID,
                                             std::move(vm_guest_endpoint), &host_vsock_endpoint_);
  }

  callback(fpromise::ok());
}

void GuestManager::ConnectToGuest(
    fidl::InterfaceRequest<fuchsia::virtualization::Guest> controller,
    fuchsia::virtualization::GuestManager::ConnectToGuestCallback callback) {
  if (guest_started_) {
    context_->svc()->Connect(std::move(controller));
    callback(fuchsia::virtualization::GuestManager_ConnectToGuest_Result::WithResponse({}));
  } else {
    FX_LOGS(ERROR) << "Failed to connect to guest. Guest is not running";
    callback(fpromise::error(ZX_ERR_UNAVAILABLE));
  }
}

void GuestManager::ConnectToBalloon(
    fidl::InterfaceRequest<fuchsia::virtualization::BalloonController> controller) {
  context_->svc()->Connect(std::move(controller));
}

void GuestManager::GetHostVsockEndpoint(
    fidl::InterfaceRequest<fuchsia::virtualization::HostVsockEndpoint> endpoint) {
  if (use_legacy_vsock_device_) {
    host_vsock_endpoint_.AddBinding(std::move(endpoint));
  } else {
    context_->svc()->Connect(std::move(endpoint));
  }
}

void GuestManager::GetGuestInfo(GetGuestInfoCallback callback) {
  fuchsia::virtualization::GuestInfo info;
  if (guest_started_) {
    info.guest_status = ::fuchsia::virtualization::GuestStatus::STARTED;
  } else {
    info.guest_status = ::fuchsia::virtualization::GuestStatus::NOT_STARTED;
  }

  callback(std::move(info));
}

// |fuchsia::virtualization::GuestConfigProvider|
void GuestManager::Get(GetCallback callback) {
  // This function is called by VMM as part of its main() to configure itself
  //
  // GuestConfigProvider::Get is expected to be called only once per LaunchGuest
  // TODO(fxbug.dev/103621) Restructure VMM's Guest to have an explicit Start and Stop function.
  // This will remove the need for the fuchsia::virtualization::GuestConfigProvider.
  //  GuestManager::LaunchGuest could simply connect to VMM's Guest protocol and call Start
  callback(std::move(guest_config_));
}
