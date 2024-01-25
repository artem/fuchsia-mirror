// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_NELSON_NELSON_H_
#define SRC_DEVICES_BOARD_DRIVERS_NELSON_NELSON_H_

#include <fidl/fuchsia.hardware.clockimpl/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fuchsia/hardware/iommu/cpp/banjo.h>
#include <lib/ddk/device.h>
#include <threads.h>

#include <optional>

#include <ddktl/device.h>
#include <fbl/macros.h>
#include <soc/aml-s905d2/s905d2-gpio.h>

#include "nelson-btis.h"
#include "sdk/lib/driver/outgoing/cpp/outgoing_directory.h"

namespace nelson {

// MAC address metadata indices
enum {
  MACADDR_WIFI = 0,
  MACADDR_BLUETOOTH = 1,
};

// These should match the mmio table defined in nelson-i2c.cc
enum {
  NELSON_I2C_A0_0,
  NELSON_I2C_2,
  NELSON_I2C_3,
};

// Nelson SPI bus arbiters (should match spi_channels[] in nelson-spi.cc).
enum {
  NELSON_SPICC0,
  NELSON_SPICC1,
};

// Nelson Board Revs
enum {
  BOARD_REV_P1 = 0,
  BOARD_REV_P2 = 1,
  BOARD_REV_P2_DOE = 2,
  BOARD_REV_PRE_EVT = 3,
  BOARD_REV_EVT = 4,
  BOARD_REV_DVT = 5,
  BOARD_REV_DVT2 = 6,

  MAX_SUPPORTED_REV,  // This must be last entry
};

// Nelson GPIO Pins used for board rev detection
constexpr uint32_t GPIO_HW_ID0 = (S905D2_GPIOZ(7));
constexpr uint32_t GPIO_HW_ID1 = (S905D2_GPIOZ(8));
constexpr uint32_t GPIO_HW_ID2 = (S905D2_GPIOZ(3));
constexpr uint32_t GPIO_HW_ID3 = (S905D2_GPIOZ(0));
constexpr uint32_t GPIO_HW_ID4 = (S905D2_GPIOAO(4));

/* Nelson I2C Devices */
constexpr uint8_t I2C_BACKLIGHT_ADDR = (0x2C);
constexpr uint8_t I2C_FOCALTECH_TOUCH_ADDR = (0x38);
constexpr uint8_t I2C_AMBIENTLIGHT_ADDR = (0x39);
constexpr uint8_t I2C_AUDIO_CODEC_ADDR = (0x2D);
constexpr uint8_t I2C_GOODIX_TOUCH_ADDR = (0x5d);
constexpr uint8_t I2C_TI_INA231_MLB_ADDR = (0x49);
constexpr uint8_t I2C_TI_INA231_MLB_ADDR_PROTO = (0x46);
constexpr uint8_t I2C_TI_INA231_SPEAKERS_ADDR = (0x40);
constexpr uint8_t I2C_SHTV3_ADDR = (0x70);

class Nelson;
using NelsonType = ddk::Device<Nelson>;

// This is the main class for the Nelson platform bus driver.
class Nelson : public NelsonType {
 public:
  explicit Nelson(zx_device_t* parent,
                  fdf::ClientEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus,
                  iommu_protocol_t* iommu)
      : NelsonType(parent),
        pbus_(std::move(pbus)),
        iommu_(iommu),
        outgoing_(fdf::Dispatcher::GetCurrent()->get()) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Device protocol implementation.
  void DdkRelease();

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(Nelson);

  void Serve(fdf::ServerEnd<fuchsia_hardware_platform_bus::PlatformBus> request) {
    device_connect_runtime_protocol(
        parent(), fuchsia_hardware_platform_bus::Service::PlatformBus::ServiceName,
        fuchsia_hardware_platform_bus::Service::PlatformBus::Name, request.TakeChannel().release());
  }

  zx::result<> AdcInit();
  zx_status_t AudioInit();
  zx_status_t BluetoothInit();
  zx_status_t ButtonsInit();
  zx_status_t CanvasInit();
  zx_status_t ClkInit();
  zx_status_t DsiInit();
  zx_status_t EmmcInit();
  zx_status_t GpioInit();
  zx_status_t I2cInit();
  zx_status_t LightInit();
  zx_status_t MaliInit();
  zx_status_t OtRadioInit();
  zx_status_t PowerInit();
  zx_status_t PwmInit();
  zx_status_t RegistersInit();
  zx_status_t SdioInit();
  zx_status_t Start();
  zx_status_t SecureMemInit();
  zx_status_t SpiInit();
  zx_status_t Spi0Init();
  zx_status_t Spi1Init();
  zx_status_t SysmemInit();
  zx_status_t TeeInit();
  zx_status_t ThermalInit();
  zx_status_t TouchInit();
  zx_status_t TouchInitP1();
  zx_status_t UsbInit();
  zx_status_t VideoInit();
  zx_status_t BacklightInit();
  zx_status_t CpuInit();
  zx_status_t NnaInit();
  zx_status_t RamCtlInit();
  zx_status_t ThermistorInit();
  zx_status_t AddPostInitDevice();
  int Thread();

  zx_status_t EnableWifi32K(void);
  zx_status_t SdEmmcConfigurePortB(void);

  static fuchsia_hardware_gpioimpl::wire::InitCall GpioConfigIn(
      fuchsia_hardware_gpio::GpioFlags flags) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithInputFlags(flags);
  }

  static fuchsia_hardware_gpioimpl::wire::InitCall GpioConfigOut(uint8_t initial_value) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithOutputValue(initial_value);
  }

  fuchsia_hardware_gpioimpl::wire::InitCall GpioSetAltFunction(uint64_t function) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithAltFunction(init_arena_, function);
  }

  fuchsia_hardware_gpioimpl::wire::InitCall GpioSetDriveStrength(uint64_t ds_ua) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithDriveStrengthUa(init_arena_, ds_ua);
  }

  // TODO(https://fxbug.dev/42059490): Switch to fdf::SyncClient when it is available.
  fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> pbus_;
  ddk::IommuProtocolClient iommu_;
  fidl::Arena<> init_arena_;
  std::vector<fuchsia_hardware_gpioimpl::wire::InitStep> gpio_init_steps_;
  std::vector<fuchsia_hardware_clockimpl::wire::InitStep> clock_init_steps_;

  thrd_t thread_;

  fdf::OutgoingDirectory outgoing_;
};

}  // namespace nelson

#endif  // SRC_DEVICES_BOARD_DRIVERS_NELSON_NELSON_H_
