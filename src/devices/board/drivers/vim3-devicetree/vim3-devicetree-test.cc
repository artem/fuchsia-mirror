
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/devicetree/testing/board-test-helper.h>

#include <zxtest/zxtest.h>

namespace vim3_dt {

namespace {

const zbi_platform_id_t kPlatformId = {
    .vid = PDEV_VID_KHADAS,
    .pid = PDEV_PID_VIM3,
    .board_name = "vim3-devicetree",
};

}

class Vim3DevicetreeTest : public zxtest::Test {
 public:
  Vim3DevicetreeTest()
      : board_test_("/pkg/test-data/khadas-vim3.dtb", kPlatformId, loop_.dispatcher()) {
    loop_.StartThread("test-realm");
    board_test_.SetupRealm();
  }

 protected:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  fdf_devicetree::testing::BoardTestHelper board_test_;
};

TEST_F(Vim3DevicetreeTest, DevicetreeEnumeration) {
  std::vector<std::string> device_node_paths = {
      "sys/platform/pt",
      "sys/platform/pt/dt-root",
      "sys/platform/pt/suspend",
      "sys/platform/pt/khadas-mcu-18",
      "sys/platform/interrupt-controller-ffc01000",
      "sys/platform/i2c-1c000",
      "sys/platform/i2c-5000",
      "sys/platform/clock-controller-ff63c000",
      "sys/platform/fuchsia-contiguous",
      "sys/platform/register-controller-1000",
      "sys/platform/nna-ff100000",
      "sys/platform/canvas-ff638000",
      "sys/platform/adc-9000",
      "sys/platform/gpio-controller-ff634400",
      "sys/platform/gpu-ffe40000",
      "sys/platform/arm-mali-ffe40000",
      "sys/platform/audio-controller-ff642000",
      "sys/platform/phy-ffe09000",
      "sys/platform/usb-ff500000",
      "sys/platform/usb-ff400000",
      "sys/platform/mmc-ffe05000",
      "sys/platform/mmc-ffe07000",
      "sys/platform/pwm-ffd1b000",
      "sys/platform/temperature-sensor-ff634800",
      "sys/platform/temperature-sensor-ff634c00",
  };
  ASSERT_TRUE(board_test_.StartRealm().is_ok());
  ASSERT_TRUE(board_test_.WaitOnDevices(device_node_paths).is_ok());
}

}  // namespace vim3_dt
