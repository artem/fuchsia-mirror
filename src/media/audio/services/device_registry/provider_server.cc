// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/provider_server.h"

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <lib/fit/internal/result.h>

#include <utility>

#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

namespace fad = fuchsia_audio_device;

// static
std::shared_ptr<ProviderServer> ProviderServer::Create(
    std::shared_ptr<const FidlThread> thread, fidl::ServerEnd<fad::Provider> server_end,
    std::shared_ptr<AudioDeviceRegistry> parent) {
  ADR_LOG_STATIC(kLogProviderServerMethods);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), std::move(parent));
}

ProviderServer::ProviderServer(std::shared_ptr<AudioDeviceRegistry> parent)
    : parent_(std::move(parent)) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();
}

ProviderServer::~ProviderServer() {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

void ProviderServer::AddDevice(AddDeviceRequest& request, AddDeviceCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogProviderServerMethods);

  if (!request.device_name() || request.device_name()->empty()) {
    ADR_WARN_METHOD() << "device_name was absent/empty";
    completer.Reply(fit::error(fad::ProviderAddDeviceError::kInvalidName));
    return;
  }

  if (!request.device_type()) {
    ADR_WARN_METHOD() << "device_type was absent";
    completer.Reply(fit::error(fad::ProviderAddDeviceError::kInvalidType));
    return;
  }

  if (!request.driver_client()) {
    ADR_WARN_METHOD() << "driver_client was absent";
    completer.Reply(fit::error(fad::ProviderAddDeviceError::kInvalidDriverClient));
    return;
  }

  if (*request.device_type() == fad::DeviceType::Unknown()) {
    ADR_WARN_METHOD() << "unknown device_type";
    completer.Reply(fit::error(fad::ProviderAddDeviceError::kInvalidType));
    return;
  }

  if (!ClientIsValidForDeviceType(*request.device_type(), *request.driver_client())) {
    ADR_WARN_METHOD() << "driver_client did not match the specified device_type";
    completer.Reply(fit::error(fad::ProviderAddDeviceError::kWrongClientType));
    return;
  }

  // Remove this, when ADR supports the other driver_client types
  if (*request.device_type() == fad::DeviceType::kDai) {
    ADR_WARN_METHOD() << "AudioDeviceRegistry does not yet support this client type";
    completer.Reply(fit::error(fad::ProviderAddDeviceError::kWrongClientType));
    return;
  }

  ADR_LOG_METHOD(kLogDeviceDetection)
      << "request to add " << *request.device_type() << " '" << *request.device_name() << "'";

  // This kicks off device initialization, which notifies the parent when it completes.
  parent_->AddDevice(Device::Create(parent_, thread().dispatcher(), *request.device_name(),
                                    *request.device_type(), std::move(*request.driver_client())));
  completer.Reply(fit::success(fad::ProviderAddDeviceResponse{}));
}

}  // namespace media_audio
