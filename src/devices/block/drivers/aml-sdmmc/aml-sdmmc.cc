// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-sdmmc.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/c/banjo.h>
#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>  // TODO(b/301003087): Needed for PDEV_DID_AMLOGIC_SDMMC_A, etc.
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/fit/defer.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/mmio/mmio.h>
#include <lib/sdmmc/hw.h>
#include <lib/sync/completion.h>
#include <lib/zx/interrupt.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <algorithm>
#include <string>

#include <bits/limits.h>
#include <fbl/algorithm.h>
#include <soc/aml-common/aml-power-domain.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "aml-sdmmc-regs.h"

namespace {

uint32_t log2_ceil(uint32_t blk_sz) {
  if (blk_sz == 1) {
    return 0;
  }
  return 32 - (__builtin_clz(blk_sz - 1));
}

zx_paddr_t PageMask() {
  static uintptr_t page_size = zx_system_get_page_size();
  return page_size - 1;
}

zx::result<pdev_device_info_t> FidlToBanjoDeviceInfo(
    const fuchsia_hardware_platform_device::wire::NodeDeviceInfo& wire_dev_info) {
  pdev_device_info_t banjo_dev_info = {};
  if (wire_dev_info.has_vid()) {
    banjo_dev_info.vid = wire_dev_info.vid();
  }
  if (wire_dev_info.has_pid()) {
    banjo_dev_info.pid = wire_dev_info.pid();
  }
  if (wire_dev_info.has_did()) {
    banjo_dev_info.did = wire_dev_info.did();
  }
  if (wire_dev_info.has_mmio_count()) {
    banjo_dev_info.mmio_count = wire_dev_info.mmio_count();
  }
  if (wire_dev_info.has_irq_count()) {
    banjo_dev_info.irq_count = wire_dev_info.irq_count();
  }
  if (wire_dev_info.has_bti_count()) {
    banjo_dev_info.bti_count = wire_dev_info.bti_count();
  }
  if (wire_dev_info.has_smc_count()) {
    banjo_dev_info.smc_count = wire_dev_info.smc_count();
  }
  if (wire_dev_info.has_metadata_count()) {
    banjo_dev_info.metadata_count = wire_dev_info.metadata_count();
  }
  if (wire_dev_info.has_name()) {
    std::string name = std::string(wire_dev_info.name().get());
    if (name.size() > sizeof(banjo_dev_info.name)) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }
    strncpy(banjo_dev_info.name, name.c_str(), sizeof(banjo_dev_info.name));
  }
  return zx::ok(banjo_dev_info);
}

}  // namespace

namespace aml_sdmmc {

zx::result<> AmlSdmmc::Start() {
  parent_.Bind(std::move(node()));

  // Initialize our compat server.
  {
    zx::result<> result = compat_server_.Initialize(
        incoming(), outgoing(), node_name(), name(),
        compat::ForwardMetadata::Some({DEVICE_METADATA_SDMMC, DEVICE_METADATA_GPT_INFO}),
        get_banjo_config());
    if (result.is_error()) {
      return result.take_error();
    }
  }

  {
    zx::result pdev = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
    if (pdev.is_error() || !pdev->is_valid()) {
      FDF_LOGL(ERROR, logger(), "Failed to connect to platform device: %s", pdev.status_string());
      return pdev.take_error();
    }

    if (zx::result status = InitResources(std::move(pdev.value())); status.is_error()) {
      return status.take_error();
    }
  }

  auto no_sync_calls_dispatcher =
      fdf::SynchronizedDispatcher::Create({}, "aml-sdmmc-worker", [](fdf_dispatcher_t*) {});
  if (no_sync_calls_dispatcher.is_error()) {
    FDF_LOGL(ERROR, logger(), "Failed to create dispatcher: %s",
             zx_status_get_string(no_sync_calls_dispatcher.status_value()));
    return zx::error(no_sync_calls_dispatcher.status_value());
  }
  worker_dispatcher_ = *std::move(no_sync_calls_dispatcher);

  {
    fuchsia_hardware_sdmmc::SdmmcService::InstanceHandler handler({
        .sdmmc = fit::bind_member<&AmlSdmmc::Serve>(this),
    });
    auto result = outgoing()->AddService<fuchsia_hardware_sdmmc::SdmmcService>(std::move(handler));
    if (result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to add service: %s", result.status_string());
      return result.take_error();
    }
  }

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to create controller endpoints: %s",
             controller_endpoints.status_string());
    return controller_endpoints.take_error();
  }

  controller_.Bind(std::move(controller_endpoints->client));

  fidl::Arena arena;
  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_sdmmc::SdmmcService>(arena));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name())
                        .offers2(arena, std::move(offers))
                        .Build();

  auto result = parent_->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }

  FDF_LOGL(INFO, logger(), "Completed start hook");

  return zx::ok();
}

zx::result<> AmlSdmmc::InitResources(
    fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev_client) {
  fidl::WireSyncClient pdev(std::move(pdev_client));

  {
    const auto result = pdev->GetMmioById(0);
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get MMIO failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get MMIO: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }

    const auto& mmio_params = result->value();
    if (!mmio_params->has_offset() || !mmio_params->has_size() || !mmio_params->has_vmo()) {
      FDF_LOGL(ERROR, logger(), "Platform device provided invalid MMIO");
      return zx::error(ZX_ERR_BAD_STATE);
    };

    auto mmio_result =
        fdf::MmioBuffer::Create(mmio_params->offset(), mmio_params->size(),
                                std::move(mmio_params->vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
    if (mmio_result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to map MMIO: %s", mmio_result.status_string());
      return mmio_result.take_error();
    }
    fbl::AutoLock lock(&lock_);
    mmio_ = std::move(mmio_result.value());
  }

  {
    const auto result = pdev->GetInterruptById(0, 0);
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get interrupt failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get interrupt: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
    irq_ = std::move(result->value()->irq);
  }

  {
    const auto result = pdev->GetBtiById(0);
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get BTI failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get BTI: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
    bti_ = std::move(result->value()->bti);
  }

  // Optional protocol.
  const char* kGpioFragmentName = "gpio-reset";
  zx::result gpio_result =
      incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(kGpioFragmentName);
  if (gpio_result.is_ok() && gpio_result->is_valid()) {
    auto gpio = fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>(std::move(gpio_result.value()));
    if (gpio->GetName().ok()) {
      reset_gpio_ = std::move(gpio);
    }
  }

  {
    auto buffer_factory = dma_buffer::CreateBufferFactory();
    fbl::AutoLock lock(&lock_);
    zx_status_t status = buffer_factory->CreateContiguous(
        bti_, kMaxDmaDescriptors * sizeof(aml_sdmmc_desc_t), 0, &descs_buffer_);
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "Failed to allocate dma descriptors");
      return zx::error(status);
    }
  }

  zx::result result = incoming()->Connect<fuchsia_hardware_clock::Service::Clock>("clock-gate");
  if (result.is_ok() && result->is_valid()) {
    auto clock = fidl::WireSyncClient<fuchsia_hardware_clock::Clock>(std::move(result.value()));
    const fidl::WireResult result = clock->Enable();
    if (result.ok()) {
      if (result->is_error()) {
        FDF_LOGL(ERROR, logger(), "Failed to enable clock: %s",
                 zx_status_get_string(result->error_value()));
        return zx::error(result->error_value());
      }
      clock_gate_ = std::move(clock);
    }
  }

  {
    const auto result = pdev->GetNodeDeviceInfo();
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get node device info failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get node device info: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }

    zx::result dev_info = FidlToBanjoDeviceInfo(*result->value());
    if (dev_info.is_error()) {
      return dev_info.take_error();
    }

    zx_status_t status = Init(dev_info.value());
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }

  return zx::success();
}

void AmlSdmmc::Serve(fdf::ServerEnd<fuchsia_hardware_sdmmc::Sdmmc> request) {
  fdf::BindServer(worker_dispatcher_.get(), std::move(request), this);
}

zx_status_t AmlSdmmc::WaitForInterruptImpl() {
  zx::time timestamp;
  return irq_.wait(&timestamp);
}

void AmlSdmmc::ClearStatus() {
  AmlSdmmcStatus::Get()
      .ReadFrom(&*mmio_)
      .set_reg_value(AmlSdmmcStatus::kClearStatus)
      .WriteTo(&*mmio_);
}

void AmlSdmmc::Inspect::Init(const pdev_device_info_t& device_info, inspect::Node& parent,
                             bool is_power_suspended) {
  std::string root_name = "aml-sdmmc-port";
  if (device_info.did == PDEV_DID_AMLOGIC_SDMMC_A) {
    root_name += 'A';
  } else if (device_info.did == PDEV_DID_AMLOGIC_SDMMC_B) {
    root_name += 'B';
  } else if (device_info.did == PDEV_DID_AMLOGIC_SDMMC_C) {
    root_name += 'C';
  } else {
    root_name += "-unknown";
  }

  root = parent.CreateChild(root_name);

  bus_clock_frequency = root.CreateUint(
      "bus_clock_frequency", AmlSdmmcClock::kCtsOscinClkFreq / AmlSdmmcClock::kDefaultClkDiv);
  adj_delay = root.CreateUint("adj_delay", 0);
  delay_lines = root.CreateUint("delay_lines", 0);
  max_delay = root.CreateUint("max_delay", 0);
  longest_window_start = root.CreateUint("longest_window_start", 0);
  longest_window_size = root.CreateUint("longest_window_size", 0);
  longest_window_adj_delay = root.CreateUint("longest_window_adj_delay", 0);
  distance_to_failing_point = root.CreateUint("distance_to_failing_point", 0);
  power_suspended = root.CreateBool("power_suspended", is_power_suspended);
}

zx::result<std::array<uint32_t, AmlSdmmc::kResponseCount>> AmlSdmmc::WaitForInterrupt(
    const sdmmc_req_t& req) {
  zx_status_t status = WaitForInterruptImpl();

  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "WaitForInterruptImpl returned %s", zx_status_get_string(status));
    return zx::error(status);
  }

  const auto status_irq = AmlSdmmcStatus::Get().ReadFrom(&*mmio_);
  ClearStatus();

  // lock_ has already been acquired. AmlSdmmc::WaitForInterrupt() has the TA_REQ(lock_) annotation.
  auto on_bus_error = fit::defer([&]() __TA_NO_THREAD_SAFETY_ANALYSIS {
    AmlSdmmcStart::Get().ReadFrom(&*mmio_).set_desc_busy(0).WriteTo(&*mmio_);
  });

  if (status_irq.rxd_err()) {
    if (req.suppress_error_messages) {
      FDF_LOGL(TRACE, logger(), "RX Data CRC Error cmd%d, arg=0x%08x, status=0x%08x", req.cmd_idx,
               req.arg, status_irq.reg_value());
    } else {
      FDF_LOGL(WARNING, logger(),
               "RX Data CRC Error cmd%d, arg=0x%08x, status=0x%08x, consecutive=%lu", req.cmd_idx,
               req.arg, status_irq.reg_value(), ++consecutive_data_errors_);
    }
    return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
  }
  if (status_irq.txd_err()) {
    FDF_LOGL(WARNING, logger(),
             "TX Data CRC Error, cmd%d, arg=0x%08x, status=0x%08x, consecutive=%lu", req.cmd_idx,
             req.arg, status_irq.reg_value(), ++consecutive_data_errors_);
    return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
  }
  if (status_irq.desc_err()) {
    FDF_LOGL(ERROR, logger(),
             "Controller does not own the descriptor, cmd%d, arg=0x%08x, status=0x%08x",
             req.cmd_idx, req.arg, status_irq.reg_value());
    return zx::error(ZX_ERR_IO_INVALID);
  }
  if (status_irq.resp_err()) {
    if (req.suppress_error_messages) {
      FDF_LOGL(TRACE, logger(), "Response CRC Error, cmd%d, arg=0x%08x, status=0x%08x", req.cmd_idx,
               req.arg, status_irq.reg_value());
    } else {
      FDF_LOGL(WARNING, logger(),
               "Response CRC Error, cmd%d, arg=0x%08x, status=0x%08x, consecutive=%lu", req.cmd_idx,
               req.arg, status_irq.reg_value(), ++consecutive_cmd_errors_);
    }
    return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
  }
  // Could not find a way to avoid a timeout for MMC_SELECT_CARD (deselect).
  const bool is_mmc_cmd7_deselect =
      req.cmd_idx == MMC_SELECT_CARD && req.cmd_flags == MMC_SELECT_CARD_FLAGS && req.arg == 0;
  if (status_irq.resp_timeout() && !is_mmc_cmd7_deselect) {
    // A timeout is acceptable for SD_SEND_IF_COND but not for MMC_SEND_EXT_CSD.
    const bool is_sd_cmd8 =
        req.cmd_idx == SD_SEND_IF_COND && req.cmd_flags == SD_SEND_IF_COND_FLAGS;
    static_assert(SD_SEND_IF_COND == MMC_SEND_EXT_CSD &&
                  (SD_SEND_IF_COND_FLAGS) != (MMC_SEND_EXT_CSD_FLAGS));
    // When mmc dev_ice is being probed with SDIO command this is an expected failure.
    if (req.suppress_error_messages || is_sd_cmd8) {
      FDF_LOGL(TRACE, logger(), "Response timeout, cmd%d, arg=0x%08x, status=0x%08x", req.cmd_idx,
               req.arg, status_irq.reg_value());
    } else {
      FDF_LOGL(ERROR, logger(),
               "Response timeout, cmd%d, arg=0x%08x, status=0x%08x, consecutive=%lu", req.cmd_idx,
               req.arg, status_irq.reg_value(), ++consecutive_cmd_errors_);
    }
    return zx::error(ZX_ERR_TIMED_OUT);
  }
  if (status_irq.desc_timeout()) {
    FDF_LOGL(ERROR, logger(),
             "Descriptor timeout, cmd%d, arg=0x%08x, status=0x%08x, consecutive=%lu", req.cmd_idx,
             req.arg, status_irq.reg_value(), ++consecutive_data_errors_);
    return zx::error(ZX_ERR_TIMED_OUT);
  }

  if (!(status_irq.end_of_chain())) {
    FDF_LOGL(ERROR, logger(), "END OF CHAIN bit is not set, cmd%d, arg=0x%08x, status=0x%08x",
             req.cmd_idx, req.arg, status_irq.reg_value());
    return zx::error(ZX_ERR_IO_INVALID);
  }

  // At this point we have succeeded and don't need to perform our on-error call
  on_bus_error.cancel();

  consecutive_cmd_errors_ = 0;
  if (req.cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    consecutive_data_errors_ = 0;
  }

  std::array<uint32_t, AmlSdmmc::kResponseCount> response = {};
  if (req.cmd_flags & SDMMC_RESP_LEN_136) {
    response[0] = AmlSdmmcCmdResp::Get().ReadFrom(&*mmio_).reg_value();
    response[1] = AmlSdmmcCmdResp1::Get().ReadFrom(&*mmio_).reg_value();
    response[2] = AmlSdmmcCmdResp2::Get().ReadFrom(&*mmio_).reg_value();
    response[3] = AmlSdmmcCmdResp3::Get().ReadFrom(&*mmio_).reg_value();
  } else {
    response[0] = AmlSdmmcCmdResp::Get().ReadFrom(&*mmio_).reg_value();
  }

  return zx::ok(response);
}

void AmlSdmmc::HostInfo(fdf::Arena& arena, HostInfoCompleter::Sync& completer) {
  sdmmc_host_info_t info;
  zx_status_t status = SdmmcHostInfo(&info);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }

  fuchsia_hardware_sdmmc::wire::SdmmcHostInfo wire_info;
  wire_info.caps = info.caps;
  wire_info.max_transfer_size = info.max_transfer_size;
  wire_info.max_transfer_size_non_dma = info.max_transfer_size_non_dma;
  wire_info.max_buffer_regions = info.max_buffer_regions;
  completer.buffer(arena).ReplySuccess(wire_info);
}

zx_status_t AmlSdmmc::SdmmcHostInfo(sdmmc_host_info_t* info) {
  memcpy(info, &dev_info_, sizeof(dev_info_));
  return ZX_OK;
}

void AmlSdmmc::SetBusWidth(SetBusWidthRequestView request, fdf::Arena& arena,
                           SetBusWidthCompleter::Sync& completer) {
  sdmmc_bus_width_t bus_width;
  switch (request->bus_width) {
    case fuchsia_hardware_sdmmc::wire::SdmmcBusWidth::kEight:
      bus_width = SDMMC_BUS_WIDTH_EIGHT;
      break;
    case fuchsia_hardware_sdmmc::wire::SdmmcBusWidth::kFour:
      bus_width = SDMMC_BUS_WIDTH_FOUR;
      break;
    case fuchsia_hardware_sdmmc::wire::SdmmcBusWidth::kOne:
      bus_width = SDMMC_BUS_WIDTH_ONE;
      break;
    default:
      bus_width = SDMMC_BUS_WIDTH_MAX;
      break;
  }

  zx_status_t status = SdmmcSetBusWidth(bus_width);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) {
  fbl::AutoLock lock(&lock_);

  if (power_suspended_) {
    FDF_LOGL(ERROR, logger(), "Rejecting SetBusWidth while power is suspended.");
    return ZX_ERR_BAD_STATE;
  }

  uint32_t bus_width_val;
  switch (bus_width) {
    case SDMMC_BUS_WIDTH_EIGHT:
      bus_width_val = AmlSdmmcCfg::kBusWidth8Bit;
      break;
    case SDMMC_BUS_WIDTH_FOUR:
      bus_width_val = AmlSdmmcCfg::kBusWidth4Bit;
      break;
    case SDMMC_BUS_WIDTH_ONE:
      bus_width_val = AmlSdmmcCfg::kBusWidth1Bit;
      break;
    default:
      return ZX_ERR_OUT_OF_RANGE;
  }

  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(bus_width_val).WriteTo(&*mmio_);

  zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));
  return ZX_OK;
}

void AmlSdmmc::RegisterInBandInterrupt(RegisterInBandInterruptRequestView request,
                                       fdf::Arena& arena,
                                       RegisterInBandInterruptCompleter::Sync& completer) {
  // Mirroring AmlSdmmc::SdmmcRegisterInBandInterrupt(const in_band_interrupt_protocol_t*
  // interrupt_cb).
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t AmlSdmmc::SdmmcRegisterInBandInterrupt(
    const in_band_interrupt_protocol_t* interrupt_cb) {
  return ZX_ERR_NOT_SUPPORTED;
}

void AmlSdmmc::SetBusFreq(SetBusFreqRequestView request, fdf::Arena& arena,
                          SetBusFreqCompleter::Sync& completer) {
  zx_status_t status = SdmmcSetBusFreq(request->bus_freq);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::SdmmcSetBusFreq(uint32_t freq) {
  fbl::AutoLock lock(&lock_);

  if (power_suspended_) {
    FDF_LOGL(ERROR, logger(), "Rejecting SetBusFreq while power is suspended.");
    return ZX_ERR_BAD_STATE;
  }

  uint32_t clk = 0, clk_src = 0, clk_div = 0;
  if (freq == 0) {
    AmlSdmmcClock::Get().ReadFrom(&*mmio_).set_cfg_div(0).WriteTo(&*mmio_);
    inspect_.bus_clock_frequency.Set(0);
    return ZX_OK;
  }

  if (freq < AmlSdmmcClock::kFClkDiv2MinFreq) {
    clk_src = AmlSdmmcClock::kCtsOscinClkSrc;
    clk = AmlSdmmcClock::kCtsOscinClkFreq;
  } else {
    clk_src = AmlSdmmcClock::kFClkDiv2Src;
    clk = AmlSdmmcClock::kFClkDiv2Freq;
  }
  // Round the divider up so the frequency is rounded down.
  clk_div = (clk + freq - 1) / freq;
  AmlSdmmcClock::Get().ReadFrom(&*mmio_).set_cfg_div(clk_div).set_cfg_src(clk_src).WriteTo(&*mmio_);
  inspect_.bus_clock_frequency.Set(clk / clk_div);
  return ZX_OK;
}

zx_status_t AmlSdmmc::SuspendPower() {
  fbl::AutoLock lock(&lock_);

  if (power_suspended_ == true) {
    return ZX_OK;
  }

  // Disable the device clock.
  auto clk = AmlSdmmcClock::Get().ReadFrom(&*mmio_);
  clk_div_saved_ = clk.cfg_div();
  clk.set_cfg_div(0).WriteTo(&*mmio_);

  // Gate the core clock.
  if (clock_gate_.is_valid()) {
    const fidl::WireResult result = clock_gate_->Disable();
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to send request to disable clock gate: %s",
               result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      FDF_LOGL(ERROR, logger(), "Send request to disable clock gate error: %s",
               zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  power_suspended_ = true;
  inspect_.power_suspended.Set(power_suspended_);
  return ZX_OK;
}

zx_status_t AmlSdmmc::ResumePower() {
  fbl::AutoLock lock(&lock_);

  if (power_suspended_ == false) {
    return ZX_OK;
  }

  // Ungate the core clock.
  if (clock_gate_.is_valid()) {
    const fidl::WireResult result = clock_gate_->Enable();
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to send request to enable clock gate: %s",
               result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      FDF_LOGL(ERROR, logger(), "Send request to enable clock gate error: %s",
               zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // Re-enable the device clock.
  auto clk = AmlSdmmcClock::Get().ReadFrom(&*mmio_);
  clk.set_cfg_div(clk_div_saved_).WriteTo(&*mmio_);

  power_suspended_ = false;
  inspect_.power_suspended.Set(power_suspended_);
  return ZX_OK;
}

void AmlSdmmc::ConfigureDefaultRegs() {
  uint32_t clk_val = AmlSdmmcClock::Get()
                         .FromValue(0)
                         .set_cfg_div(AmlSdmmcClock::kDefaultClkDiv)
                         .set_cfg_src(AmlSdmmcClock::kDefaultClkSrc)
                         .set_cfg_co_phase(AmlSdmmcClock::kDefaultClkCorePhase)
                         .set_cfg_tx_phase(AmlSdmmcClock::kDefaultClkTxPhase)
                         .set_cfg_rx_phase(AmlSdmmcClock::kDefaultClkRxPhase)
                         .set_cfg_always_on(1)
                         .reg_value();
  AmlSdmmcClock::Get().ReadFrom(&*mmio_).set_reg_value(clk_val).WriteTo(&*mmio_);

  uint32_t config_val = AmlSdmmcCfg::Get()
                            .FromValue(0)
                            .set_blk_len(AmlSdmmcCfg::kDefaultBlkLen)
                            .set_resp_timeout(AmlSdmmcCfg::kDefaultRespTimeout)
                            .set_rc_cc(AmlSdmmcCfg::kDefaultRcCc)
                            .set_bus_width(AmlSdmmcCfg::kBusWidth1Bit)
                            .reg_value();
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_reg_value(config_val).WriteTo(&*mmio_);
  AmlSdmmcStatus::Get()
      .ReadFrom(&*mmio_)
      .set_reg_value(AmlSdmmcStatus::kClearStatus)
      .WriteTo(&*mmio_);
  AmlSdmmcIrqEn::Get()
      .ReadFrom(&*mmio_)
      .set_reg_value(AmlSdmmcStatus::kClearStatus)
      .WriteTo(&*mmio_);

  // Zero out any delay line or sampling settings that may have come from the bootloader.
  AmlSdmmcAdjust::Get().FromValue(0).WriteTo(&*mmio_);
  AmlSdmmcDelay1::Get().FromValue(0).WriteTo(&*mmio_);
  AmlSdmmcDelay2::Get().FromValue(0).WriteTo(&*mmio_);
}

void AmlSdmmc::HwReset(fdf::Arena& arena, HwResetCompleter::Sync& completer) {
  zx_status_t status = SdmmcHwReset();
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::SdmmcHwReset() {
  fbl::AutoLock lock(&lock_);

  // TODO(b/309152727): Explore allowing HwReset while power is suspended.
  if (power_suspended_) {
    FDF_LOGL(ERROR, logger(), "Rejecting HwReset while power is suspended.");
    return ZX_ERR_BAD_STATE;
  }

  if (reset_gpio_.is_valid()) {
    fidl::WireResult result1 = reset_gpio_->ConfigOut(0);
    if (!result1.ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to send ConfigOut request to reset gpio: %s",
               result1.status_string());
      return result1.status();
    }
    if (result1->is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to configure reset gpio to output low: %s",
               zx_status_get_string(result1->error_value()));
      return result1->error_value();
    }
    zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));
    fidl::WireResult result2 = reset_gpio_->ConfigOut(1);
    if (!result2.ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to send ConfigOut request to reset gpio: %s",
               result2.status_string());
      return result2.status();
    }
    if (result2->is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to configure reset gpio to output high: %s",
               zx_status_get_string(result2->error_value()));
      return result2->error_value();
    }
    zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));
  }
  ConfigureDefaultRegs();

  return ZX_OK;
}

void AmlSdmmc::SetTiming(SetTimingRequestView request, fdf::Arena& arena,
                         SetTimingCompleter::Sync& completer) {
  sdmmc_timing_t timing;
  // Only handling the cases for AmlSdmmc::SetTiming(sdmmc_timing_t timing) to work correctly.
  switch (request->timing) {
    case fuchsia_hardware_sdmmc::wire::SdmmcTiming::kHs400:
      timing = SDMMC_TIMING_HS400;
      break;
    case fuchsia_hardware_sdmmc::wire::SdmmcTiming::kHsddr:
      timing = SDMMC_TIMING_HSDDR;
      break;
    case fuchsia_hardware_sdmmc::wire::SdmmcTiming::kDdr50:
      timing = SDMMC_TIMING_DDR50;
      break;
    default:
      timing = SDMMC_TIMING_MAX;
      break;
  }

  zx_status_t status = SdmmcSetTiming(timing);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::SdmmcSetTiming(sdmmc_timing_t timing) {
  fbl::AutoLock lock(&lock_);

  if (power_suspended_) {
    FDF_LOGL(ERROR, logger(), "Rejecting SetTiming while power is suspended.");
    return ZX_ERR_BAD_STATE;
  }

  auto config = AmlSdmmcCfg::Get().ReadFrom(&*mmio_);
  if (timing == SDMMC_TIMING_HS400 || timing == SDMMC_TIMING_HSDDR ||
      timing == SDMMC_TIMING_DDR50) {
    if (timing == SDMMC_TIMING_HS400) {
      config.set_chk_ds(1);
    } else {
      config.set_chk_ds(0);
    }
    config.set_ddr(1);
    auto clk = AmlSdmmcClock::Get().ReadFrom(&*mmio_);
    uint32_t clk_div = clk.cfg_div();
    if (clk_div & 0x01) {
      clk_div++;
    }
    clk_div /= 2;
    clk.set_cfg_div(clk_div).WriteTo(&*mmio_);
  } else {
    config.set_ddr(0);
  }

  config.WriteTo(&*mmio_);
  return ZX_OK;
}

void AmlSdmmc::SetSignalVoltage(SetSignalVoltageRequestView request, fdf::Arena& arena,
                                SetSignalVoltageCompleter::Sync& completer) {
  // Mirroring AmlSdmmc::SdmmcSetSignalVoltage(sdmmc_voltage_t voltage).
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::SdmmcSetSignalVoltage(sdmmc_voltage_t voltage) {
  // Amlogic controller does not allow to modify voltage
  // We do not return an error here since things work fine without switching the voltage.
  return ZX_OK;
}

aml_sdmmc_desc_t* AmlSdmmc::SetupCmdDesc(const sdmmc_req_t& req) {
  aml_sdmmc_desc_t* const desc = reinterpret_cast<aml_sdmmc_desc_t*>(descs_buffer_->virt());
  auto cmd_cfg = AmlSdmmcCmdCfg::Get().FromValue(0);
  if (req.cmd_flags == 0) {
    cmd_cfg.set_no_resp(1);
  } else {
    if (req.cmd_flags & SDMMC_RESP_LEN_136) {
      cmd_cfg.set_resp_128(1);
    }

    if (!(req.cmd_flags & SDMMC_RESP_CRC_CHECK)) {
      cmd_cfg.set_resp_no_crc(1);
    }

    if (req.cmd_flags & SDMMC_RESP_LEN_48B) {
      cmd_cfg.set_r1b(1);
    }

    cmd_cfg.set_resp_num(1);
  }
  cmd_cfg.set_cmd_idx(req.cmd_idx)
      .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
      .set_error(0)
      .set_owner(1)
      .set_end_of_chain(0);

  desc->cmd_info = cmd_cfg.reg_value();
  desc->cmd_arg = req.arg;
  desc->data_addr = 0;
  desc->resp_addr = 0;
  return desc;
}

zx::result<std::pair<aml_sdmmc_desc_t*, std::vector<fzl::PinnedVmo>>> AmlSdmmc::SetupDataDescs(
    const sdmmc_req_t& req, aml_sdmmc_desc_t* const cur_desc) {
  const uint32_t req_blk_len = log2_ceil(req.blocksize);
  if (req_blk_len > AmlSdmmcCfg::kMaxBlkLen) {
    FDF_LOGL(ERROR, logger(), "blocksize %u is greater than the max (%u)", 1 << req_blk_len,
             1 << AmlSdmmcCfg::kMaxBlkLen);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_blk_len(req_blk_len).WriteTo(&*mmio_);

  std::vector<fzl::PinnedVmo> pinned_vmos;
  pinned_vmos.reserve(req.buffers_count);

  aml_sdmmc_desc_t* desc = cur_desc;
  SdmmcVmoStore& vmos = registered_vmos_[req.client_id];
  for (size_t i = 0; i < req.buffers_count; i++) {
    if (req.buffers_list[i].type == SDMMC_BUFFER_TYPE_VMO_HANDLE) {
      auto status = SetupUnownedVmoDescs(req, req.buffers_list[i], desc);
      if (!status.is_ok()) {
        return zx::error(status.error_value());
      }

      pinned_vmos.push_back(std::move(std::get<1>(status.value())));
      desc = std::get<0>(status.value());
    } else {
      vmo_store::StoredVmo<OwnedVmoInfo>* const stored_vmo =
          vmos.GetVmo(req.buffers_list[i].buffer.vmo_id);
      if (stored_vmo == nullptr) {
        FDF_LOGL(ERROR, logger(), "no VMO %u for client %u", req.buffers_list[i].buffer.vmo_id,
                 req.client_id);
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      auto status = SetupOwnedVmoDescs(req, req.buffers_list[i], *stored_vmo, desc);
      if (status.is_error()) {
        return zx::error(status.error_value());
      }
      desc = status.value();
    }
  }

  if (desc == cur_desc) {
    FDF_LOGL(ERROR, logger(), "empty descriptor list!");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::ok(std::pair{desc - 1, std::move(pinned_vmos)});
}

zx::result<aml_sdmmc_desc_t*> AmlSdmmc::SetupOwnedVmoDescs(const sdmmc_req_t& req,
                                                           const sdmmc_buffer_region_t& buffer,
                                                           vmo_store::StoredVmo<OwnedVmoInfo>& vmo,
                                                           aml_sdmmc_desc_t* const cur_desc) {
  if (!(req.cmd_flags & SDMMC_CMD_READ) && !(vmo.meta().rights & SDMMC_VMO_RIGHT_READ)) {
    FDF_LOGL(ERROR, logger(), "Request would read from write-only VMO");
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }
  if ((req.cmd_flags & SDMMC_CMD_READ) && !(vmo.meta().rights & SDMMC_VMO_RIGHT_WRITE)) {
    FDF_LOGL(ERROR, logger(), "Request would write to read-only VMO");
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  if (buffer.offset + buffer.size > vmo.meta().size) {
    FDF_LOGL(ERROR, logger(), "buffer reads past vmo end: offset %zu, size %zu, vmo size %zu",
             buffer.offset + vmo.meta().offset, buffer.size, vmo.meta().size);
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  fzl::PinnedVmo::Region regions[SDMMC_PAGES_COUNT];
  size_t offset = buffer.offset;
  size_t remaining = buffer.size;
  aml_sdmmc_desc_t* desc = cur_desc;
  while (remaining > 0) {
    size_t region_count = 0;
    zx_status_t status = vmo.GetPinnedRegions(offset + vmo.meta().offset, buffer.size, regions,
                                              std::size(regions), &region_count);
    if (status != ZX_OK && status != ZX_ERR_BUFFER_TOO_SMALL) {
      FDF_LOGL(ERROR, logger(), "failed to get pinned regions: %s", zx_status_get_string(status));
      return zx::error(status);
    }

    const size_t last_offset = offset;
    for (size_t i = 0; i < region_count; i++) {
      zx::result<aml_sdmmc_desc_t*> next_desc = PopulateDescriptors(req, desc, regions[i]);
      if (next_desc.is_error()) {
        return next_desc;
      }

      desc = next_desc.value();
      offset += regions[i].size;
      remaining -= regions[i].size;
    }

    if (offset == last_offset) {
      FDF_LOGL(ERROR, logger(), "didn't get any pinned regions");
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  return zx::ok(desc);
}

zx::result<std::pair<aml_sdmmc_desc_t*, fzl::PinnedVmo>> AmlSdmmc::SetupUnownedVmoDescs(
    const sdmmc_req_t& req, const sdmmc_buffer_region_t& buffer, aml_sdmmc_desc_t* const cur_desc) {
  const bool is_read = req.cmd_flags & SDMMC_CMD_READ;
  const uint64_t pagecount =
      ((buffer.offset & PageMask()) + buffer.size + PageMask()) / zx_system_get_page_size();

  const zx::unowned_vmo vmo(buffer.buffer.vmo);
  const uint32_t options = is_read ? ZX_BTI_PERM_WRITE : ZX_BTI_PERM_READ;

  fzl::PinnedVmo pinned_vmo;
  zx_status_t status = pinned_vmo.PinRange(
      buffer.offset & ~PageMask(), pagecount * zx_system_get_page_size(), *vmo, bti_, options);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "bti-pin failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // We don't own this VMO, and therefore cannot make any assumptions about the state of the
  // cache. The cache must be clean and invalidated for reads so that the final clean + invalidate
  // doesn't overwrite main memory with stale data from the cache, and must be clean for writes so
  // that main memory has the latest data.
  if (req.cmd_flags & SDMMC_CMD_READ) {
    status =
        vmo->op_range(ZX_VMO_OP_CACHE_CLEAN_INVALIDATE, buffer.offset, buffer.size, nullptr, 0);
  } else {
    status = vmo->op_range(ZX_VMO_OP_CACHE_CLEAN, buffer.offset, buffer.size, nullptr, 0);
  }

  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Cache op on unowned VMO failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  aml_sdmmc_desc_t* desc = cur_desc;
  for (uint32_t i = 0; i < pinned_vmo.region_count(); i++) {
    fzl::PinnedVmo::Region region = pinned_vmo.region(i);
    if (i == 0) {
      region.phys_addr += buffer.offset & PageMask();
      region.size -= buffer.offset & PageMask();
    }
    if (i == pinned_vmo.region_count() - 1) {
      const size_t end_offset =
          (pagecount * zx_system_get_page_size()) - buffer.size - (buffer.offset & PageMask());
      region.size -= end_offset;
    }

    zx::result<aml_sdmmc_desc_t*> next_desc = PopulateDescriptors(req, desc, region);
    if (next_desc.is_error()) {
      return zx::error(next_desc.error_value());
    }
    desc = next_desc.value();
  }

  return zx::ok(std::pair{desc, std::move(pinned_vmo)});
}

zx::result<aml_sdmmc_desc_t*> AmlSdmmc::PopulateDescriptors(const sdmmc_req_t& req,
                                                            aml_sdmmc_desc_t* const cur_desc,
                                                            fzl::PinnedVmo::Region region) {
  if (region.phys_addr > UINT32_MAX || (region.phys_addr + region.size) > UINT32_MAX) {
    FDF_LOGL(ERROR, logger(), "DMA goes out of accessible range: 0x%0zx, %zu", region.phys_addr,
             region.size);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  const bool use_block_mode = (1 << log2_ceil(req.blocksize)) == req.blocksize;
  const aml_sdmmc_desc_t* const descs_end =
      descs() + (descs_buffer_->size() / sizeof(aml_sdmmc_desc_t));

  const size_t max_desc_size =
      use_block_mode ? req.blocksize * AmlSdmmcCmdCfg::kMaxBlockCount : req.blocksize;

  aml_sdmmc_desc_t* desc = cur_desc;
  while (region.size > 0) {
    const size_t desc_size = std::min(region.size, max_desc_size);

    if (desc >= descs_end) {
      FDF_LOGL(ERROR, logger(), "request with more than %zu chunks is unsupported",
               kMaxDmaDescriptors);
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    if (region.phys_addr % AmlSdmmcCmdCfg::kDataAddrAlignment != 0) {
      // The last two bits must be zero to indicate DDR/big-endian.
      FDF_LOGL(ERROR, logger(), "DMA start address must be 4-byte aligned");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    if (desc_size % req.blocksize != 0) {
      FDF_LOGL(ERROR, logger(), "DMA length %zu is not multiple of block size %u", desc_size,
               req.blocksize);
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }

    auto cmd = AmlSdmmcCmdCfg::Get().FromValue(desc->cmd_info);
    if (desc != descs()) {
      cmd = AmlSdmmcCmdCfg::Get().FromValue(0);
      cmd.set_no_resp(1).set_no_cmd(1);
      desc->cmd_arg = 0;
      desc->resp_addr = 0;
    }

    cmd.set_data_io(1);
    if (!(req.cmd_flags & SDMMC_CMD_READ)) {
      cmd.set_data_wr(1);
    }
    cmd.set_owner(1).set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout).set_error(0);

    const size_t blockcount = desc_size / req.blocksize;
    if (use_block_mode) {
      cmd.set_block_mode(1).set_len(static_cast<uint32_t>(blockcount));
    } else if (blockcount == 1) {
      cmd.set_length(req.blocksize);
    } else {
      FDF_LOGL(ERROR, logger(), "can't send more than one block of size %u", req.blocksize);
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }

    desc->cmd_info = cmd.reg_value();
    desc->data_addr = static_cast<uint32_t>(region.phys_addr);
    desc++;

    region.phys_addr += desc_size;
    region.size -= desc_size;
  }

  return zx::ok(desc);
}

zx_status_t AmlSdmmc::FinishReq(const sdmmc_req_t& req) {
  if ((req.cmd_flags & SDMMC_RESP_DATA_PRESENT) && (req.cmd_flags & SDMMC_CMD_READ)) {
    const cpp20::span<const sdmmc_buffer_region_t> regions{req.buffers_list, req.buffers_count};
    for (const auto& region : regions) {
      if (region.type != SDMMC_BUFFER_TYPE_VMO_HANDLE) {
        continue;
      }

      // Invalidate the cache so that the next CPU read will pick up data that was written to main
      // memory by the controller.
      zx_status_t status = zx_vmo_op_range(region.buffer.vmo, ZX_VMO_OP_CACHE_CLEAN_INVALIDATE,
                                           region.offset, region.size, nullptr, 0);
      if (status != ZX_OK) {
        FDF_LOGL(ERROR, logger(), "Failed to clean/invalidate cache: %s",
                 zx_status_get_string(status));
        return status;
      }
    }
  }

  return ZX_OK;
}

void AmlSdmmc::WaitForBus() const {
  while (!AmlSdmmcStatus::Get().ReadFrom(&*mmio_).cmd_i()) {
    zx::nanosleep(zx::deadline_after(zx::usec(10)));
  }
}

zx_status_t AmlSdmmc::TuningDoTransfer(const TuneContext& context) {
  fbl::AutoLock lock(&lock_);

  if (power_suspended_) {
    FDF_LOGL(ERROR, logger(), "Rejecting TuningDoTransfer while power is suspended.");
    return ZX_ERR_BAD_STATE;
  }

  SetTuneSettings(context.new_settings);

  const sdmmc_buffer_region_t buffer = {
      .buffer = {.vmo = context.vmo->get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = context.expected_block.size(),
  };
  const sdmmc_req_t tuning_req{
      .cmd_idx = context.cmd,
      .cmd_flags = MMC_SEND_TUNING_BLOCK_FLAGS,
      .arg = 0,
      .blocksize = static_cast<uint32_t>(context.expected_block.size()),
      .suppress_error_messages = true,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  uint32_t unused_response[4];
  zx_status_t status = SdmmcRequestLocked(&tuning_req, unused_response);

  // Restore the original tuning settings so that client transfers can still go through.
  SetTuneSettings(context.original_settings);

  return status;
}

bool AmlSdmmc::TuningTestSettings(const TuneContext& context) {
  zx_status_t status = ZX_OK;
  size_t n;
  for (n = 0; n < AML_SDMMC_TUNING_TEST_ATTEMPTS; n++) {
    status = TuningDoTransfer(context);
    if (status != ZX_OK) {
      break;
    }

    uint8_t tuning_res[512] = {0};
    if ((status = context.vmo->read(tuning_res, 0, context.expected_block.size())) != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "Failed to read VMO: %s", zx_status_get_string(status));
      break;
    }
    if (memcmp(context.expected_block.data(), tuning_res, context.expected_block.size()) != 0) {
      break;
    }
  }
  return (n == AML_SDMMC_TUNING_TEST_ATTEMPTS);
}

AmlSdmmc::TuneWindow AmlSdmmc::GetFailingWindow(TuneResults results) {
  TuneWindow largest_window, current_window;

  for (uint32_t delay = 0; delay <= AmlSdmmcClock::kMaxDelay; delay++, results.results >>= 1) {
    if (results.results & 1) {
      if (current_window.size > largest_window.size) {
        largest_window = current_window;
      }

      current_window = {.start = delay + 1, .size = 0};
    } else {
      current_window.size++;
    }
  }

  if (current_window.start == 0) {
    // The best window will not have been set if no values failed. If that happens the current
    // window start will still be set to zero -- check for that case and update the best window.
    largest_window = {.start = 0, .size = AmlSdmmcClock::kMaxDelay + 1};
  } else if (current_window.size > largest_window.size) {
    // If the final value passed then the last (and current) window was never checked against the
    // best window. Make the last window the best window if it is larger than the previous best.
    largest_window = current_window;
  }

  return largest_window;
}

AmlSdmmc::TuneResults AmlSdmmc::TuneDelayLines(const TuneContext& context) {
  TuneResults results = {};
  TuneContext local_context = context;
  for (uint32_t i = 0; i <= AmlSdmmcClock::kMaxDelay; i++) {
    local_context.new_settings.delay = i;
    if (TuningTestSettings(local_context)) {
      results.results |= 1ULL << i;
    }
  }
  return results;
}

void AmlSdmmc::SetTuneSettings(const TuneSettings& settings) {
  AmlSdmmcAdjust::Get()
      .ReadFrom(&*mmio_)
      .set_adj_delay(settings.adj_delay)
      .set_adj_fixed(1)
      .WriteTo(&*mmio_);
  AmlSdmmcDelay1::Get()
      .ReadFrom(&*mmio_)
      .set_dly_0(settings.delay)
      .set_dly_1(settings.delay)
      .set_dly_2(settings.delay)
      .set_dly_3(settings.delay)
      .set_dly_4(settings.delay)
      .WriteTo(&*mmio_);
  AmlSdmmcDelay2::Get()
      .ReadFrom(&*mmio_)
      .set_dly_5(settings.delay)
      .set_dly_6(settings.delay)
      .set_dly_7(settings.delay)
      .set_dly_8(settings.delay)
      .set_dly_9(settings.delay)
      .WriteTo(&*mmio_);
}

AmlSdmmc::TuneSettings AmlSdmmc::GetTuneSettings() {
  TuneSettings settings{};
  settings.adj_delay = AmlSdmmcAdjust::Get().ReadFrom(&*mmio_).adj_delay();
  settings.delay = AmlSdmmcDelay1::Get().ReadFrom(&*mmio_).dly_0();
  return settings;
}

inline uint32_t AbsDifference(uint32_t a, uint32_t b) { return a > b ? a - b : b - a; }

uint32_t AmlSdmmc::DistanceToFailingPoint(TuneSettings point,
                                          cpp20::span<const TuneResults> adj_delay_results) {
  uint64_t results = adj_delay_results[point.adj_delay].results;
  uint32_t min_distance = AmlSdmmcClock::kMaxDelay;
  for (uint32_t i = 0; i <= AmlSdmmcClock::kMaxDelay; i++, results >>= 1) {
    if ((results & 1) == 0) {
      const uint32_t distance = AbsDifference(i, point.delay);
      if (distance < min_distance) {
        min_distance = distance;
      }
    }
  }

  return min_distance;
}

void AmlSdmmc::PerformTuning(PerformTuningRequestView request, fdf::Arena& arena,
                             PerformTuningCompleter::Sync& completer) {
  zx_status_t status = SdmmcPerformTuning(request->cmd_idx);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::SdmmcPerformTuning(uint32_t tuning_cmd_idx) {
  fbl::AutoLock tuning_lock(&tuning_lock_);

  // Using a lambda for the constness of the resulting variables.
  const auto result = [this]() -> zx::result<std::tuple<uint32_t, uint32_t, TuneSettings>> {
    fbl::AutoLock lock(&lock_);

    if (power_suspended_) {
      FDF_LOGL(ERROR, logger(), "Rejecting PerformTuning while power is suspended.");
      return zx::error(ZX_ERR_BAD_STATE);
    }

    const uint32_t bw = AmlSdmmcCfg::Get().ReadFrom(&*mmio_).bus_width();
    const uint32_t clk_div = AmlSdmmcClock::Get().ReadFrom(&*mmio_).cfg_div();
    return zx::ok(std::tuple{bw, clk_div, GetTuneSettings()});
  }();

  if (result.is_error()) {
    return result.status_value();
  }
  const auto bw = std::get<0>(*result);
  const auto clk_div = std::get<1>(*result);
  const auto settings = std::get<2>(*result);

  TuneContext context{.original_settings = settings};

  if (bw == AmlSdmmcCfg::kBusWidth4Bit) {
    context.expected_block = cpp20::span<const uint8_t>(aml_sdmmc_tuning_blk_pattern_4bit,
                                                        sizeof(aml_sdmmc_tuning_blk_pattern_4bit));
  } else if (bw == AmlSdmmcCfg::kBusWidth8Bit) {
    context.expected_block = cpp20::span<const uint8_t>(aml_sdmmc_tuning_blk_pattern_8bit,
                                                        sizeof(aml_sdmmc_tuning_blk_pattern_8bit));
  } else {
    FDF_LOGL(ERROR, logger(), "Tuning at wrong buswidth: %d", bw);
    return ZX_ERR_INTERNAL;
  }

  zx::vmo received_block;
  zx_status_t status = zx::vmo::create(context.expected_block.size(), 0, &received_block);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to create VMO: %s", zx_status_get_string(status));
    return status;
  }

  context.vmo = received_block.borrow();
  context.cmd = tuning_cmd_idx;

  TuneResults adj_delay_results[AmlSdmmcClock::kMaxClkDiv] = {};
  for (uint32_t i = 0; i < clk_div; i++) {
    char property_name[28];  // strlen("tuning_results_adj_delay_63")
    snprintf(property_name, sizeof(property_name), "tuning_results_adj_delay_%u", i);

    context.new_settings.adj_delay = i;
    adj_delay_results[i] = TuneDelayLines(context);

    const std::string results = adj_delay_results[i].ToString(AmlSdmmcClock::kMaxDelay);

    inspect::Node node = inspect_.root.CreateChild(property_name);
    inspect_.tuning_results.push_back(node.CreateString("tuning_results", results));
    inspect_.tuning_results_nodes.push_back(std::move(node));

    // Add a leading zero so that fx iquery show-file sorts the results properly.
    FDF_LOGL(INFO, logger(), "Tuning results [%02u]: %s", i, results.c_str());
  }

  zx::result<TuneSettings> tuning_settings =
      PerformTuning({adj_delay_results, adj_delay_results + clk_div});
  if (tuning_settings.is_error()) {
    return tuning_settings.status_value();
  }

  {
    fbl::AutoLock lock(&lock_);

    if (power_suspended_) {
      FDF_LOGL(ERROR, logger(), "Rejecting PerformTuning while power is suspended.");
      return ZX_ERR_BAD_STATE;
    }

    SetTuneSettings(*tuning_settings);
  }

  inspect_.adj_delay.Set(tuning_settings->adj_delay);
  inspect_.delay_lines.Set(tuning_settings->delay);
  inspect_.distance_to_failing_point.Set(
      DistanceToFailingPoint(*tuning_settings, adj_delay_results));

  FDF_LOGL(INFO, logger(), "Clock divider %u, adj delay %u, delay %u", clk_div,
           tuning_settings->adj_delay, tuning_settings->delay);
  return ZX_OK;
}

zx::result<AmlSdmmc::TuneSettings> AmlSdmmc::PerformTuning(
    cpp20::span<const TuneResults> adj_delay_results) {
  ZX_DEBUG_ASSERT(adj_delay_results.size() <= UINT32_MAX);
  const auto clk_div = static_cast<uint32_t>(adj_delay_results.size());

  TuneWindow largest_failing_window = {};
  uint32_t failing_adj_delay = 0;
  for (uint32_t i = 0; i < clk_div; i++) {
    const TuneWindow failing_window = GetFailingWindow(adj_delay_results[i]);
    if (failing_window.size > largest_failing_window.size) {
      largest_failing_window = failing_window;
      failing_adj_delay = i;
    }
  }

  if (largest_failing_window.size == 0) {
    FDF_LOGL(INFO, logger(), "No transfers failed, using default settings");
    return zx::ok(TuneSettings{0, 0});
  }

  const uint32_t best_adj_delay = (failing_adj_delay + (clk_div / 2)) % clk_div;

  // For even dividers adj_delay will be exactly 180 degrees phase shifted from the chosen point,
  // so set the delay lines to the middle of the largest failing window. For odd dividers just
  // choose the first failing delay value, and set adj_delay to as close as 180 degrees shifted as
  // possible (rounding down).
  const uint32_t best_delay =
      (clk_div % 2 == 0) ? largest_failing_window.middle() : largest_failing_window.start;

  const TuneSettings results{.adj_delay = best_adj_delay, .delay = best_delay};

  inspect_.longest_window_start.Set(largest_failing_window.start);
  inspect_.longest_window_size.Set(largest_failing_window.size);
  inspect_.longest_window_adj_delay.Set(failing_adj_delay);

  FDF_LOGL(INFO, logger(),
           "Largest failing window: adj_delay %u, delay start %u, size %u, middle %u",
           failing_adj_delay, largest_failing_window.start, largest_failing_window.size,
           largest_failing_window.middle());
  return zx::ok(results);
}

void AmlSdmmc::RegisterVmo(RegisterVmoRequestView request, fdf::Arena& arena,
                           RegisterVmoCompleter::Sync& completer) {
  zx_status_t status =
      SdmmcRegisterVmo(request->vmo_id, request->client_id, std::move(request->vmo),
                       request->offset, request->size, request->vmo_rights);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::SdmmcRegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo,
                                       uint64_t offset, uint64_t size, uint32_t vmo_rights) {
  if (client_id > SDMMC_MAX_CLIENT_ID) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (vmo_rights == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  vmo_store::StoredVmo<OwnedVmoInfo> stored_vmo(std::move(vmo), OwnedVmoInfo{
                                                                    .offset = offset,
                                                                    .size = size,
                                                                    .rights = vmo_rights,
                                                                });
  const uint32_t read_perm = (vmo_rights & SDMMC_VMO_RIGHT_READ) ? ZX_BTI_PERM_READ : 0;
  const uint32_t write_perm = (vmo_rights & SDMMC_VMO_RIGHT_WRITE) ? ZX_BTI_PERM_WRITE : 0;
  zx_status_t status = stored_vmo.Pin(bti_, read_perm | write_perm, true);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to pin VMO %u for client %u: %s", vmo_id, client_id,
             zx_status_get_string(status));
    return status;
  }

  fbl::AutoLock lock(&lock_);
  return registered_vmos_[client_id].RegisterWithKey(vmo_id, std::move(stored_vmo));
}

void AmlSdmmc::UnregisterVmo(UnregisterVmoRequestView request, fdf::Arena& arena,
                             UnregisterVmoCompleter::Sync& completer) {
  zx::vmo vmo;
  zx_status_t status = SdmmcUnregisterVmo(request->vmo_id, request->client_id, &vmo);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess(std::move(vmo));
}

zx_status_t AmlSdmmc::SdmmcUnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo) {
  if (client_id > SDMMC_MAX_CLIENT_ID) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AutoLock lock(&lock_);

  vmo_store::StoredVmo<OwnedVmoInfo>* const vmo_info = registered_vmos_[client_id].GetVmo(vmo_id);
  if (!vmo_info) {
    return ZX_ERR_NOT_FOUND;
  }

  zx_status_t status = vmo_info->vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, out_vmo);
  if (status != ZX_OK) {
    return status;
  }

  return registered_vmos_[client_id].Unregister(vmo_id).status_value();
}

void AmlSdmmc::Request(RequestRequestView request, fdf::Arena& arena,
                       RequestCompleter::Sync& completer) {
  fidl::Array<uint32_t, 4> response;
  for (const auto& req : request->reqs) {
    std::vector<sdmmc_buffer_region_t> buffer_regions;
    buffer_regions.reserve(req.buffers.count());
    for (const auto& buffer : req.buffers) {
      sdmmc_buffer_region_t region;

      if (buffer.buffer.is_vmo_id()) {
        if (buffer.type != fuchsia_hardware_sdmmc::wire::SdmmcBufferType::kVmoId) {
          completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
          return;
        }
        region.buffer.vmo_id = buffer.buffer.vmo_id();
        region.type = SDMMC_BUFFER_TYPE_VMO_ID;
      } else {
        if (!buffer.buffer.is_vmo() ||
            buffer.type != fuchsia_hardware_sdmmc::wire::SdmmcBufferType::kVmoHandle) {
          completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
          return;
        }
        region.buffer.vmo = buffer.buffer.vmo().get();
        region.type = SDMMC_BUFFER_TYPE_VMO_HANDLE;
      }

      region.offset = buffer.offset;
      region.size = buffer.size;
      buffer_regions.push_back(region);
    }

    sdmmc_req_t sdmmc_req = {
        .cmd_idx = req.cmd_idx,
        .cmd_flags = req.cmd_flags,
        .arg = req.arg,
        .blocksize = req.blocksize,
        .suppress_error_messages = req.suppress_error_messages,
        .client_id = req.client_id,
        .buffers_list = buffer_regions.data(),
        .buffers_count = buffer_regions.size(),
    };
    zx_status_t status = SdmmcRequest(&sdmmc_req, response.data());
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
  }
  completer.buffer(arena).ReplySuccess(response);
}

zx_status_t AmlSdmmc::SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) {
  if (req->client_id > SDMMC_MAX_CLIENT_ID) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AutoLock lock(&lock_);
  return SdmmcRequestLocked(req, out_response);
}

zx_status_t AmlSdmmc::SdmmcRequestLocked(const sdmmc_req_t* req, uint32_t out_response[4]) {
  if (shutdown_) {
    return ZX_ERR_CANCELED;
  }

  if (power_suspended_) {
    FDF_LOGL(ERROR, logger(), "Rejecting SdmmcRequestLocked while power is suspended.");
    return ZX_ERR_BAD_STATE;
  }

  // Wait for the bus to become idle before issuing the next request. This could be necessary if the
  // card is driving CMD low after a voltage switch.
  WaitForBus();

  // stop executing
  AmlSdmmcStart::Get().ReadFrom(&*mmio_).set_desc_busy(0).WriteTo(&*mmio_);

  std::optional<std::vector<fzl::PinnedVmo>> pinned_vmos;

  aml_sdmmc_desc_t* desc = SetupCmdDesc(*req);
  aml_sdmmc_desc_t* last_desc = desc;
  if (req->cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    auto status = SetupDataDescs(*req, desc);
    if (status.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to setup data descriptors");
      return status.error_value();
    }
    last_desc = std::get<0>(status.value());
    pinned_vmos.emplace(std::move(std::get<1>(status.value())));
  }

  auto cmd_info = AmlSdmmcCmdCfg::Get().FromValue(last_desc->cmd_info);
  cmd_info.set_end_of_chain(1);
  last_desc->cmd_info = cmd_info.reg_value();
  FDF_LOGL(TRACE, logger(), "SUBMIT req:%p cmd_idx: %d cmd_cfg: 0x%x cmd_dat: 0x%x cmd_arg: 0x%x",
           req, req->cmd_idx, desc->cmd_info, desc->data_addr, desc->cmd_arg);

  zx_paddr_t desc_phys;

  auto start_reg = AmlSdmmcStart::Get().ReadFrom(&*mmio_);
  desc_phys = descs_buffer_->phys();
  zx_cache_flush(descs_buffer_->virt(), descs_buffer_->size(), ZX_CACHE_FLUSH_DATA);
  // Read desc from external DDR
  start_reg.set_desc_int(0);

  ClearStatus();

  start_reg.set_desc_busy(1)
      .set_desc_addr((static_cast<uint32_t>(desc_phys)) >> 2)
      .WriteTo(&*mmio_);

  zx::result<std::array<uint32_t, AmlSdmmc::kResponseCount>> response = WaitForInterrupt(*req);
  if (response.is_ok()) {
    memcpy(out_response, response.value().data(), sizeof(uint32_t) * AmlSdmmc::kResponseCount);
  }

  if (zx_status_t status = FinishReq(*req); status != ZX_OK) {
    return status;
  }

  return response.status_value();
}

zx_status_t AmlSdmmc::Init(const pdev_device_info_t& device_info) {
  fbl::AutoLock lock(&lock_);

  // The core clock must be enabled before attempting to access the start register.
  ConfigureDefaultRegs();

  // Stop processing DMA descriptors before releasing quarantine.
  AmlSdmmcStart::Get().ReadFrom(&*mmio_).set_desc_busy(0).WriteTo(&*mmio_);
  zx_status_t status = bti_.release_quarantine();
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to release quarantined pages");
    return status;
  }

  dev_info_.caps = SDMMC_HOST_CAP_BUS_WIDTH_8 | SDMMC_HOST_CAP_VOLTAGE_330 | SDMMC_HOST_CAP_SDR104 |
                   SDMMC_HOST_CAP_SDR50 | SDMMC_HOST_CAP_DDR50 | SDMMC_HOST_CAP_DMA;

  dev_info_.max_transfer_size = kMaxDmaDescriptors * zx_system_get_page_size();
  dev_info_.max_transfer_size_non_dma = AML_SDMMC_MAX_PIO_DATA_SIZE;

  inspect_.Init(device_info, inspector().root(), power_suspended_);
  inspect_.max_delay.Set(AmlSdmmcClock::kMaxDelay + 1);

  return ZX_OK;
}

void AmlSdmmc::PrepareStop(fdf::PrepareStopCompleter completer) {
  // If there's a pending request, wait for it to complete (and any pages to be unpinned).
  {
    fbl::AutoLock lock(&lock_);
    shutdown_ = true;

    descs_buffer_.reset();
  }

  completer(zx::ok());
}

}  // namespace aml_sdmmc
