// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_

#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/async/cpp/irq.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/time.h>

namespace aml_i2c {

class AmlI2c : public fdf::DriverBase, public fdf::WireServer<fuchsia_hardware_i2cimpl::Device> {
 public:
  AmlI2c(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // I2cImpl protocol implementation
  void GetMaxTransferSize(fdf::Arena& arena, GetMaxTransferSizeCompleter::Sync& completer) override;
  void SetBitrate(SetBitrateRequestView request, fdf::Arena& arena,
                  SetBitrateCompleter::Sync& completer) override;
  void Transact(TransactRequestView request, fdf::Arena& arena,
                TransactCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_i2cimpl::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  void SetTimeout(zx::duration timeout) { timeout_ = timeout; }

 protected:
  // Visible for testing
  virtual zx::result<fdf::MmioBuffer> MapMmio(
      const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev);

 private:
  zx_status_t ServeI2cImpl();
  compat::DeviceServer::BanjoConfig CreateBanjoConfig();
  zx_status_t CreateChildNode();

  void SetTargetAddr(uint16_t addr) const;
  void StartXfer() const;
  zx_status_t WaitTransferComplete() const;

  zx_status_t Read(cpp20::span<uint8_t> dst, bool stop) const;
  zx_status_t Write(cpp20::span<uint8_t> src, bool stop) const;

  zx_status_t StartIrqThread();
  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);

  const fdf::MmioBuffer& regs_iobuff() const;

  zx::interrupt irq_;
  zx::event event_;
  std::optional<fdf::MmioBuffer> regs_iobuff_;
  zx::duration timeout_ = zx::sec(1);
  fdf::ServerBindingGroup<fuchsia_hardware_i2cimpl::Device> i2cimpl_bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> child_controller_;
  compat::SyncInitializedDeviceServer device_server_;
  // Only needed in order to set the role name for the code that waits for irq's.
  std::optional<fdf::Dispatcher> irq_dispatcher_;
  std::optional<fdf::PrepareStopCompleter> completer_;
  async::IrqMethod<AmlI2c, &AmlI2c::HandleIrq> irq_handler_{this};
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
