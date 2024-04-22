// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_MAGMA_LIB_MAGMA_CLIENT_TEST_UTIL_TEST_DEVICE_HELPER_H_
#define SRC_GRAPHICS_MAGMA_LIB_MAGMA_CLIENT_TEST_UTIL_TEST_DEVICE_HELPER_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/fidl.h>
#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/magma/magma.h>
#include <lib/zx/channel.h>
#include <lib/zx/clock.h>

#include <filesystem>

#include <gtest/gtest.h>

namespace magma {
class TestDeviceBase {
 public:
  explicit TestDeviceBase(std::string device_name) { InitializeFromFileName(device_name.c_str()); }

  explicit TestDeviceBase(uint64_t vendor_id) { InitializeFromVendorId(vendor_id); }

  TestDeviceBase() = default;

  void InitializeFromFileName(const char* device_name) {
    auto controller_client = component::Connect<fuchsia_device::Controller>(
        std::string(device_name) + "/device_controller");
    ASSERT_TRUE(controller_client.is_ok()) << controller_client.status_string();
    device_controller_ = std::move(*controller_client);
    auto magma_client = component::Connect<fuchsia_gpu_magma::TestDevice>(device_name);
    ASSERT_TRUE(magma_client.is_ok()) << magma_client.status_string();

    magma_channel_ = magma_client->borrow();

    EXPECT_EQ(MAGMA_STATUS_OK,
              magma_device_import(magma_client->TakeChannel().release(), &device_));
  }

  void InitializeFromVendorId(uint64_t id) {
    for (auto& p : std::filesystem::directory_iterator("/dev/class/gpu")) {
      InitializeFromFileName(p.path().c_str());
      uint64_t vendor_id;
      magma_status_t magma_status =
          magma_device_query(device_, MAGMA_QUERY_VENDOR_ID, NULL, &vendor_id);
      if (magma_status == MAGMA_STATUS_OK && vendor_id == id) {
        return;
      }

      magma_device_release(device_);
      device_ = 0;
    }
    GTEST_FAIL();
  }

  // Get a channel to the parent device, so we can rebind the driver to it. This
  // requires sandbox access to /dev/sys.
  fidl::ClientEnd<fuchsia_device::Controller> GetParentDevice() {
    char path[fuchsia_device::wire::kMaxDevicePathLen + 1];
    auto res = fidl::WireCall(device_controller_)->GetTopologicalPath();

    EXPECT_EQ(ZX_OK, res.status());
    EXPECT_TRUE(res->is_ok());

    auto& response = *res->value();
    EXPECT_LE(response.path.size(), fuchsia_device::wire::kMaxDevicePathLen);

    memcpy(path, response.path.data(), response.path.size());
    path[response.path.size()] = 0;
    // Remove everything after the final slash.
    *strrchr(path, '/') = 0;

    auto parent =
        component::Connect<fuchsia_device::Controller>(std::string(path) + "/device_controller");

    EXPECT_EQ(ZX_OK, parent.status_value());
    return std::move(*parent);
  }

  static fidl::ClientEnd<fuchsia_device::Controller> GetParentDeviceFromId(uint64_t id) {
    magma::TestDeviceBase test_base(id);
    return test_base.GetParentDevice();
  }

  static void RebindParentDeviceFromId(uint64_t id, const std::string& url_suffix = "") {
    fidl::ClientEnd parent = GetParentDeviceFromId(id);
    RebindDevice(parent, url_suffix);
  }

  static void RebindDevice(fidl::UnownedClientEnd<fuchsia_device::Controller> device,
                           const std::string& url_suffix = "") {
    fidl::WireResult result =
        fidl::WireCall(device)->Rebind(fidl::StringView::FromExternal(url_suffix));
    ASSERT_EQ(ZX_OK, result.status());
    ASSERT_TRUE(result->is_ok()) << zx_status_get_string(result->error_value());
  }

  static void RestartDFv2Driver(const std::string& driver_url, uint32_t gpu_vendor_id) {
    auto manager = component::Connect<fuchsia_driver_development::Manager>();

    fidl::WireSyncClient manager_client(*std::move(manager));
    auto test_device = magma::TestDeviceBase(gpu_vendor_id);
    auto restart_result = manager_client->RestartDriverHosts(
        fidl::StringView::FromExternal(driver_url),
        fuchsia_driver_development::wire::RestartRematchFlags::kRequested |
            fuchsia_driver_development::wire::RestartRematchFlags::kCompositeSpec);

    EXPECT_TRUE(restart_result.ok()) << restart_result.status_string();
    EXPECT_TRUE(restart_result->is_ok()) << restart_result->error_value();

    {
      auto channel = test_device.magma_channel();
      // Use the existing channel to wait for the device handle to close.
      EXPECT_EQ(ZX_OK,
                channel.handle()->wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), nullptr));
    }

    bool found_device = false;
    // Loop until a new device with the correct specs is found.
    auto deadline_time = zx::clock::get_monotonic() + zx::sec(5);
    while (!found_device && zx::clock::get_monotonic() < deadline_time) {
      for (auto& p : std::filesystem::directory_iterator("/dev/class/gpu")) {
        auto magma_client =
            component::Connect<fuchsia_gpu_magma::TestDevice>(static_cast<std::string>(p.path()));

        magma_device_t device;
        EXPECT_EQ(MAGMA_STATUS_OK,
                  magma_device_import(magma_client->TakeChannel().release(), &device));

        uint64_t vendor_id;
        magma_status_t magma_status =
            magma_device_query(device, MAGMA_QUERY_VENDOR_ID, NULL, &vendor_id);

        magma_device_release(device);
        if (magma_status == MAGMA_STATUS_OK && vendor_id == gpu_vendor_id) {
          found_device = true;
          break;
        }
      }
      zx::nanosleep(zx::deadline_after(zx::msec(10)));
    }
  }

  const fidl::ClientEnd<fuchsia_device::Controller>& channel() { return device_controller_; }
  const fidl::UnownedClientEnd<fuchsia_gpu_magma::TestDevice>& magma_channel() {
    return magma_channel_;
  }

  magma_device_t device() const { return device_; }

  uint32_t GetDeviceId() const {
    uint64_t value;
    magma_status_t status = magma_device_query(device_, MAGMA_QUERY_DEVICE_ID, nullptr, &value);
    if (status != MAGMA_STATUS_OK)
      return 0;
    return static_cast<uint32_t>(value);
  }

  uint32_t GetVendorId() const {
    uint64_t value;
    magma_status_t status = magma_device_query(device_, MAGMA_QUERY_VENDOR_ID, nullptr, &value);
    if (status != MAGMA_STATUS_OK)
      return 0;
    return static_cast<uint32_t>(value);
  }

  bool IsIntelGen12() {
    if (GetVendorId() != 0x8086)
      return false;

    switch (GetDeviceId()) {
      case 0x9A40:
      case 0x9A49:
        return true;
    }
    return false;
  }

  ~TestDeviceBase() {
    if (device_)
      magma_device_release(device_);
  }

 private:
  magma_device_t device_ = 0;
  fidl::ClientEnd<fuchsia_device::Controller> device_controller_;
  fidl::UnownedClientEnd<fuchsia_gpu_magma::TestDevice> magma_channel_{ZX_HANDLE_INVALID};
};

}  // namespace magma

#endif  // SRC_GRAPHICS_MAGMA_LIB_MAGMA_CLIENT_TEST_UTIL_TEST_DEVICE_HELPER_H_
