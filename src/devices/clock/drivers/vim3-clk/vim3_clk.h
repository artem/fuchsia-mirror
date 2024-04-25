// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CLOCK_DRIVERS_VIM3_CLK_VIM3_CLK_H_
#define SRC_DEVICES_CLOCK_DRIVERS_VIM3_CLK_VIM3_CLK_H_

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.hardware.clockimpl/cpp/driver/fidl.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/zx/result.h>

#include <vector>

#include "meson_gate.h"
#include "meson_pll.h"

namespace vim3_clock {

constexpr size_t kHiuMmioIndex = 0;
constexpr size_t kDosMmioIndex = 1;

class Vim3Clock final : public fdf::DriverBase,
                        public fdf::WireServer<fuchsia_hardware_clockimpl::ClockImpl> {
 public:
  Vim3Clock(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  // Called by the Driver Framework to initialize the driver instance.
  zx::result<> Start() override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_clockimpl::ClockImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(ERROR, "Unexpected clockimpl FIDL call: 0x%lx", metadata.method_ordinal);
  }

  void Enable(fuchsia_hardware_clockimpl::wire::ClockImplEnableRequest* request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override;
  void Disable(fuchsia_hardware_clockimpl::wire::ClockImplDisableRequest* request,
               fdf::Arena& arena, DisableCompleter::Sync& completer) override;
  void IsEnabled(fuchsia_hardware_clockimpl::wire::ClockImplIsEnabledRequest* request,
                 fdf::Arena& arena, IsEnabledCompleter::Sync& completer) override;
  void SetRate(fuchsia_hardware_clockimpl::wire::ClockImplSetRateRequest* request,
               fdf::Arena& arena, SetRateCompleter::Sync& completer) override;
  void QuerySupportedRate(
      fuchsia_hardware_clockimpl::wire::ClockImplQuerySupportedRateRequest* request,
      fdf::Arena& arena, QuerySupportedRateCompleter::Sync& completer) override;
  void GetRate(fuchsia_hardware_clockimpl::wire::ClockImplGetRateRequest* request,
               fdf::Arena& arena, GetRateCompleter::Sync& completer) override;
  void SetInput(fuchsia_hardware_clockimpl::wire::ClockImplSetInputRequest* request,
                fdf::Arena& arena, SetInputCompleter::Sync& completer) override;
  void GetNumInputs(fuchsia_hardware_clockimpl::wire::ClockImplGetNumInputsRequest* request,
                    fdf::Arena& arena, GetNumInputsCompleter::Sync& completer) override;
  void GetInput(fuchsia_hardware_clockimpl::wire::ClockImplGetInputRequest* request,
                fdf::Arena& arena, GetInputCompleter::Sync& completer) override;

 private:
  zx_status_t ClkToggle(uint32_t clk, bool enable);
  zx_status_t ClkTogglePll(uint32_t clk, bool enable);

  zx_status_t GetMesonRateClock(uint32_t clkid, MesonRateClock** out);

  void InitGates();
  void InitHiu();
  void InitCpuClks();

  // Required for maintaining the topological path.
  compat::SyncInitializedDeviceServer compat_server_;

  // Client for controller the child node.
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> child_controller_;

  fdf::ServerBindingGroup<fuchsia_hardware_clockimpl::ClockImpl> bindings_;

  std::optional<fdf::MmioBuffer> hiu_mmio_;
  std::optional<fdf::MmioBuffer> dos_mmio_;
  std::optional<fdf::MmioBuffer> hiudev_;

  std::vector<MesonGate> gates_;
  std::vector<MesonPllClock> plls_;
  std::vector<MesonCpuClock> cpu_clks_;
};

}  // namespace vim3_clock

#endif  // SRC_DEVICES_CLOCK_DRIVERS_VIM3_CLK_VIM3_CLK_H_
