// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.hardware.powersource/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/syslog/cpp/macros.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

using fuchsia_hardware_powersource::wire::PowerType;

class FakeBatteryRealmTest : public gtest::TestLoopFixture {
 public:
 protected:
  void SetUp() override {
    TestLoopFixture::SetUp();

    // Create and build the realm.
    auto realm_builder = component_testing::RealmBuilder::Create();
    driver_test_realm::Setup(realm_builder);
    realm_ = realm_builder.Build(dispatcher());

    // Start DriverTestRealm.
    zx::result dtr = realm_->component().Connect<fuchsia_driver_test::Realm>();
    fuchsia_driver_test::RealmArgs args{{
        .root_driver = "fuchsia-boot:///platform-bus#meta/platform-bus.cm",
        .use_driver_framework_v2 = true,
    }};
    fidl::Result result = fidl::Call(*dtr)->Start(std::move(args));
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  component_testing::RealmRoot& Realm() { return *realm_; }

 private:
  std::optional<component_testing::RealmRoot> realm_;
};

TEST_F(FakeBatteryRealmTest, DriversExist) {
  // Connect to dev.
  zx::result dev_class_dir = Realm().component().Connect<fuchsia_io::Directory>("dev-class");
  ASSERT_EQ(dev_class_dir.status_value(), ZX_OK);
  zx::result dir_result = component::ConnectAt<fuchsia_io::Directory>(*dev_class_dir, "power");
  ASSERT_EQ(dir_result.status_value(), ZX_OK);
  auto& dir = dir_result.value();

  auto watch_result = device_watcher::WatchDirectoryForItems<std::string>(
      dir, [](std::string_view name) -> std::optional<std::string> { return std::string(name); });
  ASSERT_EQ(watch_result.status_value(), ZX_OK);
  auto name = std::move(watch_result.value());
  auto client_end = component::ConnectAt<fuchsia_hardware_powersource::Source>(dir, name);
  ASSERT_EQ(client_end.status_value(), ZX_OK);
  fidl::WireSyncClient client(std::move(*client_end));

  // Send a FIDL request.
  fidl::WireResult result = client->GetPowerInfo();
  ASSERT_EQ(ZX_OK, result.status());
  const auto& info = result.value().info;
  ASSERT_EQ(info.type, PowerType::kBattery);
}
