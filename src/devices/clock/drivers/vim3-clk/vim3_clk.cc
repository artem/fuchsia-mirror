// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vim3_clk.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/mmio/mmio-buffer.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <limits>
#include <memory>

#include <bind/fuchsia/test/cpp/bind.h>
#include <soc/aml-meson/aml-clk-common.h>
#include <soc/aml-meson/g12b-clk.h>
#include <src/devices/lib/mmio/include/lib/mmio/mmio-view.h>

namespace vim3_clock {

Vim3Clock::Vim3Clock(fdf::DriverStartArgs start_args,
                     fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("vim3_clk", std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> Vim3Clock::Start() {
  FDF_LOG(INFO, "Vim3Clock::Start()");

  zx::result pdev_client = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_client.is_error() || !pdev_client->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_client.status_string());
    return pdev_client.take_error();
  }

  zx::result pdev_result = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_result.is_error()) {
    FDF_LOG(ERROR, "Failed to open pdev service: %s", pdev_result.status_string());
    return pdev_result.take_error();
  }
  fidl::WireSyncClient pdev(std::move(pdev_result.value()));

  auto hiu_mmio = MapMmio(pdev, kHiuMmioIndex);
  if (hiu_mmio.is_error()) {
    FDF_LOG(ERROR, "Failed to map HIU mmio, st = %s", zx_status_get_string(hiu_mmio.error_value()));
    return hiu_mmio.take_error();
  }
  hiu_mmio_ = std::move(hiu_mmio.value());

  auto dos_mmio = MapMmio(pdev, kDosMmioIndex);
  if (dos_mmio.is_error()) {
    FDF_LOG(ERROR, "Failed to map DOS mmio, st = %s", zx_status_get_string(dos_mmio.error_value()));
    return dos_mmio.take_error();
  }
  dos_mmio_ = std::move(dos_mmio.value());

  auto child_name = "clocks";

  // Initialize our compat server.
  {
    zx::result<> result = compat_server_.Initialize(
        incoming(), outgoing(), node_name(), child_name,
        compat::ForwardMetadata::Some({DEVICE_METADATA_CLOCK_IDS, DEVICE_METADATA_CLOCK_INIT}));
    if (result.is_error()) {
      return result.take_error();
    }
  }

  auto add_service_result = outgoing()->AddService<fuchsia_hardware_clockimpl::Service>(
      fuchsia_hardware_clockimpl::Service::InstanceHandler({
          .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                            fidl::kIgnoreBindingClosure),
      }));
  if (add_service_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", add_service_result.status_string());
    return add_service_result.take_error();
  }

  // Add a child node.
  auto offers = compat_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_clockimpl::Service>());

  auto add_child_result = AddChild(child_name, {}, offers);
  if (add_service_result.is_error()) {
    return add_child_result.take_error();
  }

  child_controller_.Bind(std::move(add_child_result.value()));

  InitGates();

  InitHiu();

  InitCpuClks();

  return zx::ok();
}

void Vim3Clock::Enable(fuchsia_hardware_clockimpl::wire::ClockImplEnableRequest* request,
                       fdf::Arena& arena, EnableCompleter::Sync& completer) {
  FDF_LOG(TRACE, "Enable - clkid = %u", request->id);

  const uint32_t id = request->id;

  const aml_clk_common::aml_clk_type type = aml_clk_common::AmlClkType(id);
  const uint16_t clkid = aml_clk_common::AmlClkIndex(id);

  zx_status_t result;
  switch (type) {
    case aml_clk_common::aml_clk_type::kMesonGate:
      result = ClkToggle(clkid, true);
      break;
    case aml_clk_common::aml_clk_type::kMesonPll:
      result = ClkTogglePll(clkid, true);
      break;
    default:
      result = ZX_ERR_NOT_SUPPORTED;
  }

  completer.buffer(arena).Reply(zx::make_result(result));
}

void Vim3Clock::Disable(fuchsia_hardware_clockimpl::wire::ClockImplDisableRequest* request,
                        fdf::Arena& arena, DisableCompleter::Sync& completer) {
  FDF_LOG(TRACE, "Disable - clkid = %u", request->id);

  const uint32_t id = request->id;

  const aml_clk_common::aml_clk_type type = aml_clk_common::AmlClkType(id);
  const uint16_t clkid = aml_clk_common::AmlClkIndex(id);

  zx_status_t result;
  switch (type) {
    case aml_clk_common::aml_clk_type::kMesonGate:
      result = ClkToggle(clkid, false);
      break;
    case aml_clk_common::aml_clk_type::kMesonPll:
      result = ClkTogglePll(clkid, false);
      break;
    default:
      result = ZX_ERR_NOT_SUPPORTED;
  }

  completer.buffer(arena).Reply(zx::make_result(result));
}

void Vim3Clock::IsEnabled(fuchsia_hardware_clockimpl::wire::ClockImplIsEnabledRequest* request,
                          fdf::Arena& arena, IsEnabledCompleter::Sync& completer) {
  FDF_LOG(TRACE, "IsEnabled - clkid = %u", request->id);

  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Vim3Clock::SetRate(fuchsia_hardware_clockimpl::wire::ClockImplSetRateRequest* request,
                        fdf::Arena& arena, SetRateCompleter::Sync& completer) {
  FDF_LOG(TRACE, "SetRate clkid = %u, hz = %lu", request->id, request->hz);

  MesonRateClock* target;
  zx_status_t result = GetMesonRateClock(request->id, &target);
  if (result != ZX_OK) {
    completer.buffer(arena).ReplyError(result);
    FDF_LOG(ERROR, "Failed to get Rate clock, clkid = %u", request->id);
    return;
  }

  if (request->hz > std::numeric_limits<uint32_t>::max()) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  result = target->SetRate(static_cast<uint32_t>(request->hz));

  completer.buffer(arena).Reply(zx::make_result(result));
}

void Vim3Clock::QuerySupportedRate(
    fuchsia_hardware_clockimpl::wire::ClockImplQuerySupportedRateRequest* request,
    fdf::Arena& arena, QuerySupportedRateCompleter::Sync& completer) {
  FDF_LOG(TRACE, "QuerySupportedRate clkid = %u, hz = %lu", request->id, request->hz);

  MesonRateClock* target;
  zx_status_t st = GetMesonRateClock(request->id, &target);
  if (st != ZX_OK) {
    completer.buffer(arena).ReplyError(st);
    FDF_LOG(ERROR, "Failed to get Rate clock, clkid = %u", request->id);
    return;
  }

  uint64_t supported_rate;
  st = target->QuerySupportedRate(request->hz, &supported_rate);

  if (st != ZX_OK) {
    completer.buffer(arena).ReplyError(st);
  } else {
    completer.buffer(arena).ReplySuccess(supported_rate);
  }
}

void Vim3Clock::GetRate(fuchsia_hardware_clockimpl::wire::ClockImplGetRateRequest* request,
                        fdf::Arena& arena, GetRateCompleter::Sync& completer) {
  FDF_LOG(TRACE, "GetRate clkid = %u", request->id);

  MesonRateClock* target;
  zx_status_t st = GetMesonRateClock(request->id, &target);
  if (st != ZX_OK) {
    completer.buffer(arena).ReplyError(st);
    FDF_LOG(ERROR, "Failed to get Rate clock, clkid = %u", request->id);
    return;
  }

  uint64_t rate;
  st = target->GetRate(&rate);

  if (st != ZX_OK) {
    completer.buffer(arena).ReplyError(st);
  } else {
    completer.buffer(arena).ReplySuccess(rate);
  }
}

void Vim3Clock::SetInput(fuchsia_hardware_clockimpl::wire::ClockImplSetInputRequest* request,
                         fdf::Arena& arena, SetInputCompleter::Sync& completer) {
  FDF_LOG(TRACE, "SetInput clkid = %u", request->id);

  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Vim3Clock::GetNumInputs(
    fuchsia_hardware_clockimpl::wire::ClockImplGetNumInputsRequest* request, fdf::Arena& arena,
    GetNumInputsCompleter::Sync& completer) {
  FDF_LOG(TRACE, "GetNumInputs clkid = %u", request->id);

  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Vim3Clock::GetInput(fuchsia_hardware_clockimpl::wire::ClockImplGetInputRequest* request,
                         fdf::Arena& arena, GetInputCompleter::Sync& completer) {
  FDF_LOG(TRACE, "GetInput clkid = %u", request->id);

  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t Vim3Clock::ClkToggle(uint32_t clk, bool enable) {
  if (clk >= gates_.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (enable) {
    gates_.at(clk).Enable();
  } else {
    gates_.at(clk).Disable();
  }

  return ZX_OK;
}

zx_status_t Vim3Clock::ClkTogglePll(uint32_t clk, bool enable) {
  if (clk >= plls_.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  return plls_.at(clk).Toggle(enable);
}

zx_status_t Vim3Clock::GetMesonRateClock(uint32_t clk, MesonRateClock** out) {
  aml_clk_common::aml_clk_type type = aml_clk_common::AmlClkType(clk);
  const uint16_t clkid = aml_clk_common::AmlClkIndex(clk);

  switch (type) {
    case aml_clk_common::aml_clk_type::kMesonPll:
      if (clkid >= plls_.size()) {
        FDF_LOG(ERROR, "HIU PLL out of range, clkid = %hu.", clkid);
        return ZX_ERR_INVALID_ARGS;
      }

      *out = &plls_[clkid];
      return ZX_OK;
    case aml_clk_common::aml_clk_type::kMesonCpuClk:
      if (clkid >= cpu_clks_.size()) {
        FDF_LOG(ERROR, "cpu clk out of range, clkid = %hu.", clkid);
        return ZX_ERR_INVALID_ARGS;
      }

      *out = &cpu_clks_[clkid];
      return ZX_OK;
    default:
      FDF_LOG(ERROR, "Unsupported clock type, type = 0x%hx\n", static_cast<unsigned short>(type));
      return ZX_ERR_NOT_SUPPORTED;
  }

  __UNREACHABLE;
}

void Vim3Clock::InitGates() {
  ZX_ASSERT_MSG(gates_.empty(), "Gates has already been initialized");

  for (const meson_gate_descriptor_t& desc : kGateDescriptors) {
    switch (desc.bank) {
      case RegisterBank::Hiu:
        gates_.emplace_back(desc.id, desc.offset, desc.mask, hiu_mmio_->View(0));
        break;
      case vim3_clock::RegisterBank::Dos:
        gates_.emplace_back(desc.id, desc.offset, desc.mask, dos_mmio_->View(0));
        break;
    }
  }

  FDF_LOG(INFO, "vim3 clock gates initialized with %lu entries", gates_.size());
}

void Vim3Clock::InitHiu() {
  plls_.reserve(HIU_PLL_COUNT);
  s905d2_hiu_init_etc(&*hiudev_, hiu_mmio_->View(0));
  for (unsigned int pllnum = 0; pllnum < HIU_PLL_COUNT; pllnum++) {
    const hhi_plls_t pll = static_cast<hhi_plls_t>(pllnum);
    auto& newpll = plls_.emplace_back(pll, &*hiudev_);
    newpll.Init();
  }

  FDF_LOG(INFO, "vim3 hiu plls initialized with %lu entries", plls_.size());
}

void Vim3Clock::InitCpuClks() {
  constexpr size_t kNumCpuClks = std::size(kG12bCpuClks);
  cpu_clks_.reserve(kNumCpuClks);

  for (size_t i = 0; i < kNumCpuClks; i++) {
    cpu_clks_.emplace_back(&*hiu_mmio_, kG12bCpuClks[i].reg, &plls_[kG12bCpuClks[i].pll],
                           kG12bCpuClks[i].initial_hz);
  }

  FDF_LOG(INFO, "vim3 cpu plls initialized with %lu entries", cpu_clks_.size());
}

zx::result<fdf::MmioBuffer> Vim3Clock::MapMmio(
    const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t idx) {
  auto mmio = pdev->GetMmioById(idx);
  if (!mmio.ok()) {
    FDF_LOG(ERROR, "Call to GetMmioById(%d) failed: %s", idx, mmio.FormatDescription().c_str());
    return zx::error(mmio.status());
  }
  if (mmio->is_error()) {
    FDF_LOG(ERROR, "GetMmioById(%d) failed: %s", idx, zx_status_get_string(mmio->error_value()));
    return mmio->take_error();
  }

  if (!mmio->value()->has_vmo() || !mmio->value()->has_size() || !mmio->value()->has_offset()) {
    FDF_LOG(ERROR, "GetMmioById(%d) returned invalid MMIO", idx);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  zx::result mmio_buffer =
      fdf::MmioBuffer::Create(mmio->value()->offset(), mmio->value()->size(),
                              std::move(mmio->value()->vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (mmio_buffer.is_error()) {
    FDF_LOG(ERROR, "Failed to map MMIO: %s", mmio_buffer.status_string());
    return zx::error(mmio_buffer.error_value());
  }

  return mmio_buffer.take_value();
}

}  // namespace vim3_clock

FUCHSIA_DRIVER_EXPORT(vim3_clock::Vim3Clock);
