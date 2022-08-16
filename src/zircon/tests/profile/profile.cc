// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/scheduler/c/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/io.h>
#include <lib/zx/channel.h>
#include <lib/zx/object.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <threads.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <string>

#include <zxtest/zxtest.h>

namespace {

zx_status_t CreateProfile(const zx::channel& profile_provider, uint32_t priority,
                          const std::string& name, zx_status_t* server_status,
                          zx::profile* profile) {
  zx_handle_t raw_profile_handle;
  zx_status_t result = fuchsia_scheduler_ProfileProviderGetProfile(
      profile_provider.get(), priority, name.data(), name.length(), server_status,
      &raw_profile_handle);
  if (result != ZX_OK) {
    return result;
  }
  *profile = zx::profile(raw_profile_handle);
  return ZX_OK;
}

zx_status_t CreateDeadlineProfile(const zx::channel& profile_provider, zx_duration_t capacity,
                                  zx_duration_t relative_deadline, zx_duration_t period,
                                  const std::string& name, zx_status_t* server_status,
                                  zx::profile* profile) {
  zx_handle_t raw_profile_handle;
  zx_status_t result = fuchsia_scheduler_ProfileProviderGetDeadlineProfile(
      profile_provider.get(), capacity, relative_deadline, period, name.data(), name.length(),
      server_status, &raw_profile_handle);
  if (result != ZX_OK) {
    return result;
  }
  *profile = zx::profile(raw_profile_handle);
  return ZX_OK;
}

zx_status_t SetProfileByRole(const zx::channel& profile_provider, zx::unowned_thread thread,
                             const std::string& role, zx_status_t* server_status) {
  zx::thread duplicate;
  if (zx_status_t status =
          thread->duplicate(ZX_RIGHT_TRANSFER | ZX_RIGHT_MANAGE_THREAD, &duplicate);
      status != ZX_OK) {
    return status;
  }
  return fuchsia_scheduler_ProfileProviderSetProfileByRole(
      profile_provider.get(), duplicate.release(), role.data(), role.length(), server_status);
}

void GetProfileProvider(zx::channel* channel) {
  // Connect to ProfileProvider.
  zx::channel server_endpoint;
  ASSERT_OK(zx::channel::create(/*flags=*/0u, &server_endpoint, channel));
  ASSERT_OK(fdio_service_connect("/svc/" fuchsia_scheduler_ProfileProvider_Name,
                                 server_endpoint.release()),
            "Could not connect to ProfileProvider.");
  ASSERT_EQ(server_endpoint.get(), ZX_HANDLE_INVALID);
}

void CheckBasicDetails(const zx::profile& profile) {
  // Ensure basic details are correct.
  zx_info_handle_basic_t info;
  ASSERT_OK(profile.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr),
            "object_get_info for profile");
  EXPECT_NE(info.koid, 0, "no koid for profile");
  EXPECT_EQ(info.type, ZX_OBJ_TYPE_PROFILE, "incorrect type for profile");
}

// Test getting a profile via the GetProfile FIDL method.
TEST(Profile, GetProfile) {
  zx::channel provider;
  GetProfileProvider(&provider);
  ASSERT_FALSE(CURRENT_TEST_HAS_FAILURES());

  zx::profile profile;
  zx_status_t status;
  ASSERT_OK(CreateProfile(provider, /*priority=*/0u, "<test>", &status, &profile),
            "Error creating profile.");
  ASSERT_OK(status);

  CheckBasicDetails(profile);
}

// Test getting a profile via the GetDeadlineProfile FIDL method.
TEST(Profile, GetDeadlineProfile) {
  zx::channel provider;
  GetProfileProvider(&provider);
  ASSERT_FALSE(CURRENT_TEST_HAS_FAILURES());

  zx::profile profile;
  zx_status_t status;
  ASSERT_OK(
      CreateDeadlineProfile(provider, /*capacity=*/ZX_MSEC(2), /*relative_deadline=*/ZX_MSEC(10),
                            /*period=*/ZX_MSEC(10), "<test>", &status, &profile),
      "Error creating deadline profile.");
  ASSERT_OK(status);

  CheckBasicDetails(profile);
}

// Test getting a profile via the GetCpuAffinityProfile FIDL method.
TEST(Profile, GetCpuAffinityProfile) {
  zx::channel provider;
  GetProfileProvider(&provider);
  ASSERT_FALSE(CURRENT_TEST_HAS_FAILURES());

  fuchsia_scheduler_CpuSet cpu_set = {};
  cpu_set.mask[0] = 1;
  zx::profile profile;
  zx_status_t server_status;
  ASSERT_OK(fuchsia_scheduler_ProfileProviderGetCpuAffinityProfile(
      provider.get(), &cpu_set, &server_status, profile.reset_and_get_address()));
  ASSERT_OK(server_status);

  CheckBasicDetails(profile);
}

// Test setting a profile via the SetProfileByRole FIDL method.
TEST(Profile, SetProfileByRole) {
  zx::channel provider;
  GetProfileProvider(&provider);
  ASSERT_FALSE(CURRENT_TEST_HAS_FAILURES());

  zx_status_t server_status;
  zx_status_t status;

  status = SetProfileByRole(provider, zx::thread::self(), "fuchsia.test-role:ok", &server_status);
  EXPECT_OK(status);
  EXPECT_OK(server_status);

  status = SetProfileByRole(provider, zx::unowned_thread{}, "fuchsia.test-role:ok", &server_status);
  EXPECT_EQ(ZX_ERR_BAD_HANDLE, status);
  EXPECT_OK(server_status);

  status =
      SetProfileByRole(provider, zx::thread::self(), "fuchsia.test-role:not-found", &server_status);
  EXPECT_OK(status);
  EXPECT_EQ(ZX_ERR_NOT_FOUND, server_status);
}

}  // namespace
