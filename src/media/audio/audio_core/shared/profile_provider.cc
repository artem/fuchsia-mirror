// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/shared/profile_provider.h"

#include <lib/syslog/cpp/macros.h>

#include <sstream>

#include "src/media/audio/audio_core/shared/mix_profile_config.h"

namespace {

// TODO(fxbug.dev/40858): Use embedded selectors to forward parameters to the system profile
// provider until the new role API using FIDL parameters is implemented.
std::string MakeRoleSelector(const std::string& role_name, const zx::duration capacity,
                             const zx::duration deadline) {
  // If the role name already contains selectors, append to the existing list.
  const char prefix = role_name.find(':') == std::string::npos ? ':' : ',';

  std::stringstream stream;
  stream << role_name << prefix << "realm=media";

  if (capacity > zx::duration{0} && deadline > zx::duration{0}) {
    stream << ",capacity=" << capacity.get();
    stream << ",deadline=" << deadline.get();
  }

  return stream.str();
}

}  // anonymous namespace

namespace media::audio {

fidl::InterfaceRequestHandler<fuchsia::media::ProfileProvider>
ProfileProvider::GetFidlRequestHandler() {
  return bindings_.GetHandler(this);
}

void ProfileProvider::RegisterHandlerWithCapacity(zx::thread thread_handle,
                                                  const std::string role_name, int64_t period,
                                                  float utilization,
                                                  RegisterHandlerWithCapacityCallback callback) {
  if (!profile_provider_) {
    profile_provider_ = context_.svc()->Connect<fuchsia::scheduler::ProfileProvider>();
  }

  const zx::duration interval = period ? zx::duration(period) : mix_profile_period_;
  const float scaled_interval = static_cast<float>(interval.to_nsecs()) * utilization;
  const zx::duration capacity(static_cast<zx_duration_t>(scaled_interval));

  const std::string role_selector = MakeRoleSelector(role_name, capacity, interval);

  profile_provider_->SetProfileByRole(
      std::move(thread_handle), role_selector,
      [interval, capacity, callback = std::move(callback), role_selector](zx_status_t status) {
        if (status != ZX_OK) {
          FX_PLOGS(WARNING, status) << "Failed to set role \"" << role_selector << "\" for thread";
          callback(0, 0);
        } else {
          callback(interval.get(), capacity.get());
        }
      });
}

void ProfileProvider::UnregisterHandler(zx::thread thread_handle, const std::string name,
                                        UnregisterHandlerCallback callback) {
  if (!profile_provider_) {
    profile_provider_ = context_.svc()->Connect<fuchsia::scheduler::ProfileProvider>();
  }

  const std::string role_name = "fuchsia.default";
  profile_provider_->SetProfileByRole(
      std::move(thread_handle), role_name,
      [callback = std::move(callback), &role_name, &name](zx_status_t status) {
        if (status != ZX_OK) {
          FX_PLOGS(WARNING, status)
              << "Failed to set role \"" << role_name << "\" for thread \"" << name << "\"";
        }
        callback();
      });
}

void ProfileProvider::RegisterMemoryRange(zx::vmar vmar_handle, std::string name,
                                          RegisterMemoryRangeCallback callback) {
  if (!profile_provider_) {
    profile_provider_ = context_.svc()->Connect<fuchsia::scheduler::ProfileProvider>();
  }

  profile_provider_->SetProfileByRole(
      std::move(vmar_handle), name, [callback = std::move(callback), &name](zx_status_t status) {
        if (status != ZX_OK) {
          FX_PLOGS(WARNING, status) << "Failed to set memory role \"" << name << "\"";
        }
        callback();
      });
}

void ProfileProvider::UnregisterMemoryRange(zx::vmar vmar_handle,
                                            UnregisterMemoryRangeCallback callback) {
  if (!profile_provider_) {
    profile_provider_ = context_.svc()->Connect<fuchsia::scheduler::ProfileProvider>();
  }

  const std::string role_name = "fuchsia.default";
  profile_provider_->SetProfileByRole(
      std::move(vmar_handle), role_name,
      [callback = std::move(callback), &role_name](zx_status_t status) {
        if (status != ZX_OK) {
          FX_PLOGS(WARNING, status) << "Failed to set memory role \"" << role_name << "\"";
        }
        callback();
      });
}

}  // namespace media::audio
