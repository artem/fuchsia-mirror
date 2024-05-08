// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_TI_TCA6408A_TI_TCA6408A_H_
#define SRC_DEVICES_GPIO_DRIVERS_TI_TCA6408A_TI_TCA6408A_H_

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/driver/fidl.h>
#include <lib/device-protocol/i2c-channel.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/zx/result.h>

namespace gpio {

class TiTca6408aTest;

class TiTca6408a : public fdf::Server<fuchsia_hardware_gpioimpl::GpioImpl> {
 public:
  TiTca6408a(ddk::I2cChannel i2c) : i2c_(std::move(i2c)) {}

  void ConfigIn(ConfigInRequest& request, ConfigInCompleter::Sync& completer) override;
  void ConfigOut(ConfigOutRequest& request, ConfigOutCompleter::Sync& completer) override;
  void SetAltFunction(SetAltFunctionRequest& request,
                      SetAltFunctionCompleter::Sync& completer) override;
  void Read(ReadRequest& request, ReadCompleter::Sync& completer) override;
  void Write(WriteRequest& request, WriteCompleter::Sync& completer) override;
  void SetPolarity(SetPolarityRequest& request, SetPolarityCompleter::Sync& completer) override;
  void SetDriveStrength(SetDriveStrengthRequest& request,
                        SetDriveStrengthCompleter::Sync& completer) override;
  void GetDriveStrength(GetDriveStrengthRequest& request,
                        GetDriveStrengthCompleter::Sync& completer) override;
  void GetInterrupt(GetInterruptRequest& request, GetInterruptCompleter::Sync& completer) override;
  void ReleaseInterrupt(ReleaseInterruptRequest& request,
                        ReleaseInterruptCompleter::Sync& completer) override;
  void GetPins(GetPinsCompleter::Sync& completer) override;
  void GetInitSteps(GetInitStepsCompleter::Sync& completer) override;
  void GetControllerId(GetControllerIdCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_gpioimpl::GpioImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(ERROR, "Unknown method %lu", metadata.method_ordinal);
  }

  enum class Register : uint8_t {
    kInputPort = 0,
    kOutputPort = 1,
    kPolarityInversion = 2,
    kConfiguration = 3,
  };

 protected:
  friend class TiTca6408aTest;

 private:
  static constexpr uint32_t kPinCount = 8;

  zx_status_t Write(uint32_t index, uint8_t value);

  bool IsIndexInRange(uint32_t index) const { return index < kPinCount; }

  zx::result<uint8_t> ReadBit(Register reg, uint32_t index);
  zx::result<> SetBit(Register reg, uint32_t index);
  zx::result<> ClearBit(Register reg, uint32_t index);

  ddk::I2cChannel i2c_;
};

class TiTca6408aDevice : public fdf::DriverBase {
 private:
  static constexpr char kDeviceName[] = "ti-tca6408a";

 public:
  TiTca6408aDevice(fdf::DriverStartArgs start_args,
                   fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDeviceName, std::move(start_args), std::move(driver_dispatcher)) {}
  zx::result<> Start() override;
  void Stop() override;

 private:
  zx::result<> CreateNode();

  std::unique_ptr<TiTca6408a> device_;
  fdf::ServerBindingGroup<fuchsia_hardware_gpioimpl::GpioImpl> bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_TI_TCA6408A_TI_TCA6408A_H_
