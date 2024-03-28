// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/syscalls/profile.h"

#include <fidl/fuchsia.scheduler.deprecated/cpp/wire.h>
#include <lib/fit/result.h>
#include <lib/profile/profile.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <string.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <algorithm>
#include <iterator>
#include <string>
#include <string_view>

#include "zircon/system/ulib/profile/config.h"

namespace {

constexpr char kConfigPath[] = "/config/profiles";

using zircon_profile::ConfiguredProfiles;
using zircon_profile::Role;

class ProfileProvider : public fidl::WireServer<fuchsia_scheduler_deprecated::ProfileProvider> {
 public:
  static zx::result<ProfileProvider*> Create(zx::resource profile_rsrc);

  void OnConnect(async_dispatcher_t* dispatcher,
                 fidl::ServerEnd<fuchsia_scheduler_deprecated::ProfileProvider> server_end) {
    bindings_.AddBinding(dispatcher, std::move(server_end), this, fidl::kIgnoreBindingClosure);
  }

 private:
  ProfileProvider(zx::resource profile_rsrc, ConfiguredProfiles profiles)
      : profile_rsrc_(std::move(profile_rsrc)), profiles_(std::move(profiles)) {}

  void GetProfile(GetProfileRequestView request, GetProfileCompleter::Sync& completer) override;

  void GetDeadlineProfile(GetDeadlineProfileRequestView request,
                          GetDeadlineProfileCompleter::Sync& completer) override;

  void GetCpuAffinityProfile(GetCpuAffinityProfileRequestView request,
                             GetCpuAffinityProfileCompleter::Sync& completer) override;

  void SetProfileByRole(SetProfileByRoleRequestView request,
                        SetProfileByRoleCompleter::Sync& completer) override;

  fidl::ServerBindingGroup<fuchsia_scheduler_deprecated::ProfileProvider> bindings_;
  zx::resource profile_rsrc_;
  ConfiguredProfiles profiles_;
};

void ProfileProvider::GetProfile(GetProfileRequestView request,
                                 GetProfileCompleter::Sync& completer) {
  const std::string_view name{request->name.get()};
  FX_SLOG(INFO, "Priority requested", FX_KV("name", name), FX_KV("priority", request->priority),
          FX_KV("tag", "ProfileProvider"));

  zx_profile_info_t info = {
      .flags = ZX_PROFILE_INFO_FLAG_PRIORITY,
      .priority = static_cast<int32_t>(std::min<uint32_t>(
          std::max<uint32_t>(request->priority, ZX_PRIORITY_LOWEST), ZX_PRIORITY_HIGHEST)),
  };

  zx::profile profile;
  zx_status_t status = zx::profile::create(profile_rsrc_, 0u, &info, &profile);
  completer.Reply(status, std::move(profile));
}

void ProfileProvider::GetDeadlineProfile(GetDeadlineProfileRequestView request,
                                         GetDeadlineProfileCompleter::Sync& completer) {
  const std::string_view name{request->name.get()};
  const double utilization =
      static_cast<double>(request->capacity) / static_cast<double>(request->deadline);
  FX_SLOG(INFO, "Deadline requested", FX_KV("name", name), FX_KV("capacity", request->capacity),
          FX_KV("deadline", request->deadline), FX_KV("period", request->period),
          FX_KV("utilization", utilization), FX_KV("tag", "ProfileProvider"));

  zx_profile_info_t info = {
      .flags = ZX_PROFILE_INFO_FLAG_DEADLINE,
      .deadline_params =
          zx_sched_deadline_params_t{
              .capacity = static_cast<zx_duration_t>(request->capacity),
              .relative_deadline = static_cast<zx_duration_t>(request->deadline),
              .period = static_cast<zx_duration_t>(request->period),
          },
  };

  zx::profile profile;
  zx_status_t status = zx::profile::create(profile_rsrc_, 0u, &info, &profile);
  completer.Reply(status, std::move(profile));
}

void ProfileProvider::GetCpuAffinityProfile(GetCpuAffinityProfileRequestView request,
                                            GetCpuAffinityProfileCompleter::Sync& completer) {
  zx_profile_info_t info = {
      .flags = ZX_PROFILE_INFO_FLAG_CPU_MASK,
  };

  static_assert(sizeof(info.cpu_affinity_mask.mask) == sizeof(request->cpu_mask.mask));
  static_assert(std::size(info.cpu_affinity_mask.mask) ==
                std::size(decltype(request->cpu_mask.mask){}));
  memcpy(info.cpu_affinity_mask.mask, request->cpu_mask.mask.begin(),
         sizeof(request->cpu_mask.mask));

  zx::profile profile;
  zx_status_t status = zx::profile::create(profile_rsrc_, 0u, &info, &profile);
  completer.Reply(status, std::move(profile));
}

void ProfileProvider::SetProfileByRole(SetProfileByRoleRequestView request,
                                       SetProfileByRoleCompleter::Sync& completer) {
  // Log the requested role and PID:TID of the thread being assigned.
  zx_info_handle_basic_t handle_info{};
  zx_status_t status = request->handle.get_info(ZX_INFO_HANDLE_BASIC, &handle_info,
                                                sizeof(handle_info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_SLOG(WARNING, "Failed to get info for thread handle",
            FX_KV("status", zx_status_get_string(status)), FX_KV("tag", "ProfileProvider"));
    handle_info.koid = ZX_KOID_INVALID;
    handle_info.related_koid = ZX_KOID_INVALID;
  }
  if (handle_info.type != ZX_OBJ_TYPE_THREAD && handle_info.type != ZX_OBJ_TYPE_VMAR) {
    completer.Reply(ZX_ERR_WRONG_TYPE);
    return;
  }

  const std::string_view role_selector{request->role.get()};
  FX_SLOG(DEBUG, "Role requested:", FX_KV("role", role_selector),
          FX_KV("pid", handle_info.related_koid), FX_KV("tid", handle_info.koid),
          FX_KV("tag", "ProfileProvider"));

  const fit::result role = Role::Create(role_selector);
  if (role.is_error()) {
    completer.Reply(role.error_value());
    return;
  }

  const auto& profile_map =
      handle_info.type == ZX_OBJ_TYPE_THREAD ? profiles_.thread : profiles_.memory;

  // Handle the test role case specially.
  if (role->IsTestRole()) {
    if (role->HasSelector("not-found")) {
      completer.Reply(ZX_ERR_NOT_FOUND);
    } else if (role->HasSelector("ok")) {
      completer.Reply(ZX_OK);
    } else {
      completer.Reply(ZX_ERR_INVALID_ARGS);
    }
    return;
  }

  // The MediaProfileProvider will occasionally request roles with the selector `realm=media`
  // specified, but for which explicit roles have been configured. The configured roles should
  // override the requested one, so we search the profile map for a role that only contains the
  // role name and has no selectors.
  const fit::result role_without_selectors = Role::Create(role_selector, true);
  if (role_without_selectors.is_error()) {
    completer.Reply(role_without_selectors.error_value());
    return;
  }

  // Select the profile parameters based on the role name.
  if (auto search = profile_map.find(*role_without_selectors); search != profile_map.cend()) {
    status = zx_object_set_profile(request->handle.get(), search->second.profile.get(), 0);
    completer.Reply(status);
  } else if (const auto media_role = role->ToMediaRole(); media_role.is_ok()) {
    // Media role only applicable to threads.
    if (handle_info.type != ZX_OBJ_TYPE_THREAD) {
      completer.Reply(ZX_ERR_INVALID_ARGS);
      return;
    }
    // TODO(https://fxbug.dev/42116876): If a media profile is not found in the system config, use
    // the forwarded parameters. This can be removed once clients are migrated to use defined roles.
    // Skip media roles with invalid deadline parameters.
    if (media_role->capacity <= 0 || media_role->deadline <= 0 ||
        media_role->capacity > media_role->deadline) {
      FX_SLOG(WARNING, "Skipping media profile with no override and invalid selectors",
              FX_KV("capacity", media_role->capacity), FX_KV("deadline", media_role->deadline),
              FX_KV("role", role->name()), FX_KV("tag", "ProfileProvider"));
      completer.Reply(ZX_OK);
      return;
    }

    FX_SLOG(INFO, "Using selector parameters for media profile with no override",
            FX_KV("capacity", media_role->capacity), FX_KV("deadline", media_role->deadline),
            FX_KV("role", role->name()), FX_KV("tag", "ProfileProvider"));

    zx_profile_info_t info = {};
    info.flags = ZX_PROFILE_INFO_FLAG_DEADLINE;
    info.deadline_params.capacity = media_role->capacity;
    info.deadline_params.relative_deadline = media_role->deadline;
    info.deadline_params.period = media_role->deadline;

    zx::profile profile;
    status = zx::profile::create(profile_rsrc_, 0u, &info, &profile);
    if (status != ZX_OK) {
      FX_SLOG(ERROR,
              "Failed to create media profile:", FX_KV("status", zx_status_get_string(status)),
              FX_KV("tag", "ProfileProvider"));
      // Failing to create a profile is likely due to invalid profile parameters.
      completer.Reply(ZX_ERR_INTERNAL);
      return;
    }
    status = zx_object_set_profile(request->handle.get(), profile.get(), 0);
    completer.Reply(status);
  } else {
    FX_SLOG(DEBUG, "Requested role not found", FX_KV("role", role->name()),
            FX_KV("tag", "ProfileProvider"));
    completer.Reply(ZX_ERR_NOT_FOUND);
  }
}

constexpr const char* profile_svc_names[] = {
    fidl::DiscoverableProtocolName<fuchsia_scheduler_deprecated::ProfileProvider>,
    nullptr,
};

zx::result<ProfileProvider*> ProfileProvider::Create(zx::resource profile_rsrc) {
  auto result = zircon_profile::LoadConfigs(kConfigPath);
  if (result.is_error()) {
    FX_SLOG(ERROR, "Failed to load configs", FX_KV("error", result.error_value()),
            FX_KV("tag", "ProfileProvider"));
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto create = [&profile_rsrc](zircon_profile::ProfileMap& profiles) {
    // Create profiles for each configured role. If creating the profile fails, remove the role
    // entry.
    for (auto iter = profiles.begin(); iter != profiles.end();) {
      const zx_status_t status =
          zx::profile::create(profile_rsrc, 0, &iter->second.info, &iter->second.profile);
      if (status != ZX_OK) {
        FX_SLOG(ERROR, "Failed to create profile for role. Requests for this role will fail.",
                FX_KV("role", iter->first.name()), FX_KV("status", zx_status_get_string(status)));
        iter = profiles.erase(iter);
      } else {
        ++iter;
      }
    }
  };
  create(result->thread);
  create(result->memory);

  // Apply the dispatch role if defined.
  const std::string dispatch_role_name = "fuchsia.system.profile-provider.dispatch";
  const fit::result<zx_status_t, Role> dispatch_role = Role::Create(dispatch_role_name);
  if (dispatch_role.is_error()) {
    FX_SLOG(ERROR, "Failed to parse dispatch role.",
            FX_KV("error", zx_status_get_string(dispatch_role.error_value())),
            FX_KV("tag", "ProfileProvider"));
  }
  const auto search = result->thread.find(*dispatch_role);
  if (search != result->thread.end()) {
    const zx_status_t status = zx::thread::self()->set_profile(search->second.profile, 0);
    if (status != ZX_OK) {
      FX_SLOG(ERROR, "Failed to set profile", FX_KV("error", zx_status_get_string(status)),
              FX_KV("tag", "ProfileProvider"));
    }
  }

  return zx::ok(new ProfileProvider{std::move(profile_rsrc), std::move(result.value())});
}

zx_status_t init(void** out_ctx) {
  auto profile_rsrc = zx::resource{static_cast<zx_handle_t>(reinterpret_cast<uintptr_t>(*out_ctx))};
  zx::result provider = ProfileProvider::Create(std::move(profile_rsrc));
  if (!provider.is_ok()) {
    return provider.status_value();
  }
  *out_ctx = provider.value();
  return ZX_OK;
}

zx_status_t connect(void* ctx, async_dispatcher_t* dispatcher, const char* service_name,
                    zx_handle_t request) {
  if (std::string_view{service_name} ==
      fidl::DiscoverableProtocolName<fuchsia_scheduler_deprecated::ProfileProvider>) {
    static_cast<ProfileProvider*>(ctx)->OnConnect(
        dispatcher,
        fidl::ServerEnd<fuchsia_scheduler_deprecated::ProfileProvider>{zx::channel{request}});
    return ZX_OK;
  }

  zx_handle_close(request);
  return ZX_ERR_NOT_SUPPORTED;
}

constexpr zx_service_ops_t service_ops = {
    .init = init,
    .connect = connect,
    .release = nullptr,
};

constexpr zx_service_provider_t profile_service_provider = {
    .version = SERVICE_PROVIDER_VERSION,
    .services = profile_svc_names,
    .ops = &service_ops,
};

}  // namespace

const zx_service_provider_t* profile_get_service_provider() { return &profile_service_provider; }
