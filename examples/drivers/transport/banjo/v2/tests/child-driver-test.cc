// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/banjo/v2/child-driver.h"

#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

#include <ddktl/device.h>

namespace testing {

namespace {
constexpr uint32_t kTestHardwareId = 0x1234567;
constexpr uint32_t kTestMajorVersion = 0x9;
constexpr uint32_t kTestMinorVersion = 0x5;
}  // namespace

// A fake banjo server serving the Misc protocol.
class FakeParentBanjoServer : public ddk::MiscProtocol<FakeParentBanjoServer, ddk::base_protocol> {
 public:
  zx_status_t MiscGetHardwareId(uint32_t* out_response) {
    *out_response = kTestHardwareId;
    return ZX_OK;
  }

  zx_status_t MiscGetFirmwareVersion(uint32_t* out_major, uint32_t* out_minor) {
    *out_major = kTestMajorVersion;
    *out_minor = kTestMinorVersion;
    return ZX_OK;
  }

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_MISC};
    config.callbacks[ZX_PROTOCOL_MISC] = banjo_server_.callback();
    return config;
  }

 private:
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_MISC, this, &misc_protocol_ops_};
};

class BanjoTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    device_server_.Init("default", "", std::nullopt, banjo_server_.GetBanjoConfig());
    ZX_ASSERT(device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   &to_driver_vfs) == ZX_OK);
    return zx::ok();
  }

 private:
  compat::DeviceServer device_server_;
  FakeParentBanjoServer banjo_server_;
};

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = banjo_transport::ChildBanjoTransportDriver;
  using EnvironmentType = BanjoTestEnvironment;
};

class ChildBanjoTransportDriverTest : public fdf_testing::DriverTestFixture<FixtureConfig> {};

TEST_F(ChildBanjoTransportDriverTest, VerifyQueryValues) {
  // Verify that the queried values match the fake banjo server.
  EXPECT_EQ(kTestHardwareId, driver()->hardware_id());
  EXPECT_EQ(kTestMajorVersion, driver()->major_version());
  EXPECT_EQ(kTestMinorVersion, driver()->minor_version());
}

}  // namespace testing
