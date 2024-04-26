// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-sdmmc.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <inttypes.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>  // TODO(b/301003087): Needed for PDEV_DID_AMLOGIC_SDMMC_A, etc.
#include <lib/driver/power/cpp/power-support.h>
#include <lib/fit/defer.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/mmio/mmio.h>
#include <lib/sdmmc/hw.h>
#include <lib/sync/completion.h>
#include <lib/trace/event.h>
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

}  // namespace

namespace aml_sdmmc {

zx_status_t AmlSdmmc::AcquireLease(
    const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client,
    fidl::ClientEnd<fuchsia_power_broker::LeaseControl>& lease_control_client_end) {
  if (lease_control_client_end.is_valid()) {
    return ZX_ERR_ALREADY_BOUND;
  }

  const fidl::WireResult result = lessor_client->Lease(AmlSdmmc::kPowerLevelOn);
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Call to Lease failed: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    switch (result->error_value()) {
      case fuchsia_power_broker::LeaseError::kInternal:
        FDF_LOGL(ERROR, logger(), "Lease returned internal error.");
        break;
      case fuchsia_power_broker::LeaseError::kNotAuthorized:
        FDF_LOGL(ERROR, logger(), "Lease returned not authorized error.");
        break;
      default:
        FDF_LOGL(ERROR, logger(), "Lease returned unknown error.");
        break;
    }
    return ZX_ERR_INTERNAL;
  }
  if (!result->value()->lease_control.is_valid()) {
    FDF_LOGL(ERROR, logger(), "Lease returned invalid lease control client end.");
    return ZX_ERR_BAD_STATE;
  }
  lease_control_client_end = std::move(result->value()->lease_control);
  return ZX_OK;
}

void AmlSdmmc::UpdatePowerLevel(
    const fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>& current_level_client,
    fuchsia_power_broker::PowerLevel power_level) {
  const fidl::WireResult result = current_level_client->Update(power_level);
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Call to Update failed: %s", result.status_string());
  } else if (result->is_error()) {
    FDF_LOGL(ERROR, logger(), "Update returned failure.");
  }
}

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

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  controller_.Bind(std::move(controller_client_end));

  fidl::Arena arena;
  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_sdmmc::SdmmcService>(arena));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name())
                        .offers2(arena, std::move(offers))
                        .Build();

  auto result = parent_->AddChild(args, std::move(controller_server_end), {});
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
    std::lock_guard<std::mutex> lock(lock_);
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
    std::lock_guard<std::mutex> lock(lock_);
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

    zx_status_t status = Init(*result->value());
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }

  {
    auto result = ConfigurePowerManagement(pdev);
    if (!result.is_ok()) {
      return result.take_error();
    }
  }

  return zx::success();
}

zx::result<> AmlSdmmc::ConfigurePowerManagement(
    fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev) {
  // Get power configs from the board driver.
  const auto result = pdev->GetPowerConfiguration();
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Call to get power config failed: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOGL(INFO, logger(), "Was not able to get power config: %s",
             zx_status_get_string(result->error_value()));
    // Some boards don't have power configs. Do not fail driver initialization in this case.
    return zx::success();
  }

  if (result->value()->config.count() == 0) {
    FDF_LOGL(INFO, logger(), "No power configs found.");
    // Do not fail driver initialization if there aren't any power configs.
    return zx::success();
  }

  auto power_broker = incoming()->Connect<fuchsia_power_broker::Topology>();
  if (power_broker.is_error() || !power_broker->is_valid()) {
    FDF_LOGL(ERROR, logger(), "Failed to connect to power broker: %s",
             power_broker.status_string());
    return power_broker.take_error();
  }

  // Register power configs with the Power Broker.
  for (const auto& config : result->value()->config) {
    auto tokens = fdf_power::GetDependencyTokens(*incoming(), config);
    if (tokens.is_error()) {
      // TODO(b/309152899): Use fuchsia.power.SuspendEnabled config cap to determine whether to
      // expect Power Framework.
      FDF_LOGL(WARNING, logger(),
               "Failed to get power dependency tokens: %u. Perhaps the product does not have Power "
               "Framework?",
               static_cast<uint8_t>(tokens.error_value()));
      return zx::success();
    }

    zx::event active_power_dep_token;
    zx::event passive_power_dep_token;
    zx::event::create(0, &active_power_dep_token);
    zx::event::create(0, &passive_power_dep_token);

    auto [current_level_client_end, current_level_server_end] =
        fidl::Endpoints<fuchsia_power_broker::CurrentLevel>::Create();
    auto [required_level_client_end, required_level_server_end] =
        fidl::Endpoints<fuchsia_power_broker::RequiredLevel>::Create();
    auto [lessor_client_end, lessor_server_end] =
        fidl::Endpoints<fuchsia_power_broker::Lessor>::Create();

    auto result = fdf_power::AddElement(
        power_broker.value(), config, std::move(tokens.value()),
        zx::unowned_event(active_power_dep_token), zx::unowned_event(passive_power_dep_token),
        std::make_pair(std::move(current_level_server_end), std::move(required_level_server_end)),
        std::move(lessor_server_end));
    if (result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to add power element: %u",
               static_cast<uint8_t>(result.error_value()));
      return zx::error(ZX_ERR_INTERNAL);
    }

    active_power_dep_tokens_.push_back(std::move(active_power_dep_token));
    passive_power_dep_tokens_.push_back(std::move(passive_power_dep_token));

    if (config.element().name().get() == kHardwarePowerElementName) {
      hardware_power_element_control_client_end_ =
          std::move(result.value().element_control_channel());
      hardware_power_lessor_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::Lessor>(std::move(lessor_client_end));
      hardware_power_current_level_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>(
              std::move(current_level_client_end));
      // TODO(b/330223394): Update power level when ordered to do so by Power Broker via
      // RequiredLevel.
      hardware_power_required_level_client_ = fidl::WireClient<fuchsia_power_broker::RequiredLevel>(
          std::move(required_level_client_end), dispatcher());
    } else if (config.element().name().get() == kSystemWakeOnRequestPowerElementName) {
      wake_on_request_element_control_client_end_ =
          std::move(result.value().element_control_channel());
      wake_on_request_lessor_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::Lessor>(std::move(lessor_client_end));
      wake_on_request_current_level_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>(
              std::move(current_level_client_end));
      // TODO(b/330223394): Update power level when ordered to do so by Power Broker via
      // RequiredLevel.
      wake_on_request_required_level_client_ =
          fidl::WireClient<fuchsia_power_broker::RequiredLevel>(
              std::move(required_level_client_end), dispatcher());
    } else {
      FDF_LOGL(ERROR, logger(), "Unexpected power element: %s",
               std::string(config.element().name().get()).c_str());
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control_client_end;
  zx_status_t status = AcquireLease(hardware_power_lessor_client_, lease_control_client_end);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to acquire lease on hardware power: %s",
             zx_status_get_string(status));
    return zx::error(status);
  }
  hardware_power_lease_control_client_ = fidl::WireClient<fuchsia_power_broker::LeaseControl>(
      std::move(lease_control_client_end), dispatcher());

  // Start continuous monitoring of the lease status and adjusting of the hardware's power level.
  AdjustHardwarePowerLevel();

  return zx::success();
}

void AmlSdmmc::AdjustHardwarePowerLevel() {
  static fuchsia_power_broker::LeaseStatus last_lease_status =
      fuchsia_power_broker::LeaseStatus::kUnknown;

  // TODO(b/330223394): Update power level when ordered to do so by Power Broker via RequiredLevel
  // (instead of monitoring LeaseStatus via the WatchStatus() call).
  if (!hardware_power_lease_control_client_.is_valid()) {
    FDF_LOGL(ERROR, logger(),
             "Invalid hardware power lease control client. Stop monitoring lease status.");
    return;
  }
  fidl::Arena<> arena;
  hardware_power_lease_control_client_.buffer(arena)
      ->WatchStatus(last_lease_status)
      .Then([this](
                fidl::WireUnownedResult<fuchsia_power_broker::LeaseControl::WatchStatus>& result) {
        auto defer = fit::defer([&]() {
          if (result.status() == ZX_ERR_CANCELED) {
            FDF_LOGL(WARNING, logger(),
                     "WatchStatus returned canceled error. Stop monitoring lease status.");
          } else {
            // Recursively call self. The WatchStatus() call blocks until the current status differs
            // from last_status (unless last_status is UNKNOWN).
            AdjustHardwarePowerLevel();
          }
        });

        if (!result.ok()) {
          FDF_LOGL(ERROR, logger(), "Call to WatchStatus failed: %s", result.status_string());
          return;
        }

        const fuchsia_power_broker::LeaseStatus lease_status = result->status;
        switch (lease_status) {
          case fuchsia_power_broker::LeaseStatus::kSatisfied: {
            const zx::time start = zx::clock::get_monotonic();

            // If tuning was delayed, will do it now.
            std::lock_guard<std::mutex> tuning_lock(tuning_lock_);
            std::lock_guard<std::mutex> lock(lock_);
            // Actually raise the hardware's power level.
            zx_status_t status = ResumePower();
            if (status != ZX_OK) {
              FDF_LOGL(ERROR, logger(), "Failed to resume power: %s", zx_status_get_string(status));
              return;
            }

            last_lease_status = lease_status;
            // Communicate to Power Broker that the hardware power level has been raised.
            UpdatePowerLevel(hardware_power_current_level_client_, kPowerLevelOn);

            const zx::duration duration = zx::clock::get_monotonic() - start;
            inspect_.wake_on_request_latency_us.Insert(duration.to_usecs());

            // Serve delayed requests that were received during power suspension, if any.
            if (delayed_requests_.size()) {
              for (auto& request : delayed_requests_) {
                SdmmcRequestInfo* request_info = std::get_if<SdmmcRequestInfo>(&request);
                if (request_info != nullptr) {
                  DoRequestAndComplete(request_info->request, request_info->arena,
                                       request_info->completer);
                  continue;
                }
                SdmmcTaskInfo* task_info = std::get_if<SdmmcTaskInfo>(&request);
                if (task_info != nullptr) {
                  auto* set_bus_width_completer =
                      std::get_if<SetBusWidthCompleter::Async>(&task_info->completer);
                  if (set_bus_width_completer != nullptr) {
                    DoTaskAndComplete(std::move(task_info->task), task_info->arena,
                                      *set_bus_width_completer);
                    continue;
                  }
                  auto* set_bus_freq_completer =
                      std::get_if<SetBusFreqCompleter::Async>(&task_info->completer);
                  if (set_bus_freq_completer != nullptr) {
                    DoTaskAndComplete(std::move(task_info->task), task_info->arena,
                                      *set_bus_freq_completer);
                    continue;
                  }
                  auto* set_timing_completer =
                      std::get_if<SetTimingCompleter::Async>(&task_info->completer);
                  if (set_timing_completer != nullptr) {
                    DoTaskAndComplete(std::move(task_info->task), task_info->arena,
                                      *set_timing_completer);
                    continue;
                  }
                  auto* hw_reset_completer =
                      std::get_if<HwResetCompleter::Async>(&task_info->completer);
                  if (hw_reset_completer != nullptr) {
                    DoTaskAndComplete(std::move(task_info->task), task_info->arena,
                                      *hw_reset_completer);
                    continue;
                  }
                  auto* perform_tuning_completer =
                      std::get_if<PerformTuningCompleter::Async>(&task_info->completer);
                  if (perform_tuning_completer != nullptr) {
                    lock_.unlock();  // Tuning acquires lock_.
                    DoTaskAndComplete(std::move(task_info->task), task_info->arena,
                                      *perform_tuning_completer);
                    lock_.lock();
                    continue;
                  }
                }
              }
              delayed_requests_.clear();

              // Drop lease on wake-on-request power element. This lets the hardware power
              // element's lease status revert to pending, unless there are other entities that
              // are raising SAG's Execution State.
              ZX_ASSERT_MSG(wake_on_request_lease_control_client_end_.is_valid(),
                            "Requests delayed without leasing wake-on-request power element.");
              wake_on_request_lease_control_client_end_.channel().reset();
              ZX_ASSERT(!wake_on_request_lease_control_client_end_.is_valid());
              // TODO(b/330223394): Update power level when ordered to do so by Power Broker
              // via RequiredLevel.
              UpdatePowerLevel(wake_on_request_current_level_client_, kPowerLevelOff);
            }
            break;
          }
          case fuchsia_power_broker::LeaseStatus::kPending: {
            // Complete any ongoing tuning first.
            std::lock_guard<std::mutex> tuning_lock(tuning_lock_);
            std::lock_guard<std::mutex> lock(lock_);
            // Actually lower the hardware's power level.
            zx_status_t status = SuspendPower();
            if (status != ZX_OK) {
              FDF_LOGL(ERROR, logger(), "Failed to suspend power: %s",
                       zx_status_get_string(status));
              return;
            }

            last_lease_status = lease_status;
            // Communicate to Power Broker that the hardware power level has been lowered.
            UpdatePowerLevel(hardware_power_current_level_client_, kPowerLevelOff);

            // TODO(b/330223394): Revert the following behaviour of releasing and reacquiring the
            // lease.
            // Release and reacquire lease on the hardware power element to unblock SAG's Execution
            // State from going to inactive.
            hardware_power_lease_control_client_ =
                fidl::WireClient<fuchsia_power_broker::LeaseControl>();

            fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control_client_end;
            status = AcquireLease(hardware_power_lessor_client_, lease_control_client_end);
            if (status != ZX_OK) {
              FDF_LOGL(ERROR, logger(), "Failed to reacquire lease on hardware power: %s",
                       zx_status_get_string(status));
              return;
            }
            hardware_power_lease_control_client_ =
                fidl::WireClient<fuchsia_power_broker::LeaseControl>(
                    std::move(lease_control_client_end), dispatcher());
            break;
          }
          default:
            FDF_LOGL(ERROR, logger(), "Unknown hardware power element lease status.");
            break;
        }
      });
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

void AmlSdmmc::Inspect::Init(
    const fuchsia_hardware_platform_device::wire::NodeDeviceInfo& device_info,
    inspect::Node& parent, bool is_power_suspended) {
  std::string root_name = "aml-sdmmc-port";
  if (device_info.has_did()) {
    if (device_info.did() == PDEV_DID_AMLOGIC_SDMMC_A) {
      root_name += 'A';
    } else if (device_info.did() == PDEV_DID_AMLOGIC_SDMMC_B) {
      root_name += 'B';
    } else if (device_info.did() == PDEV_DID_AMLOGIC_SDMMC_C) {
      root_name += 'C';
    }
  }
  if (root_name == "aml-sdmmc-port") {
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
  wake_on_request_count = root.CreateUint("wake_on_request_count", 0);
  // 14 buckets spanning from 1us to ~8ms.
  wake_on_request_latency_us = root.CreateExponentialUintHistogram(
      "wake_on_request_latency_us", /*floor=*/1, /*initial_step=*/1, /*step_multiplier=*/2,
      /*buckets=*/14);
}

zx::result<std::array<uint32_t, AmlSdmmc::kResponseCount>> AmlSdmmc::WaitForInterrupt(
    const fuchsia_hardware_sdmmc::wire::SdmmcReq& req) {
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
  completer.buffer(arena).ReplySuccess(dev_info_);
}

void AmlSdmmc::SetBusWidth(SetBusWidthRequestView request, fdf::Arena& arena,
                           SetBusWidthCompleter::Sync& completer) {
  // This function is run after acquiring lock_, but the compiler doesn't recognize this.
  fit::function<zx_status_t()> task =
      [this, bus_width = request->bus_width]()
          __TA_NO_THREAD_SAFETY_ANALYSIS { return SetBusWidthImpl(bus_width); };

  std::lock_guard<std::mutex> lock(lock_);

  if (power_suspended_) {
    zx_status_t status = ActivateWakeOnRequest();
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }

    // Delay this request until power has been resumed.
    delayed_requests_.emplace_back(
        SdmmcTaskInfo{std::move(task), std::move(arena), completer.ToAsync()});
    return;
  }

  DoTaskAndComplete(std::move(task), arena, completer);
}

zx_status_t AmlSdmmc::SetBusWidthImpl(fuchsia_hardware_sdmmc::wire::SdmmcBusWidth bus_width) {
  uint32_t bus_width_val;
  switch (bus_width) {
    case fuchsia_hardware_sdmmc::wire::SdmmcBusWidth::kEight:
      bus_width_val = AmlSdmmcCfg::kBusWidth8Bit;
      break;
    case fuchsia_hardware_sdmmc::wire::SdmmcBusWidth::kFour:
      bus_width_val = AmlSdmmcCfg::kBusWidth4Bit;
      break;
    case fuchsia_hardware_sdmmc::wire::SdmmcBusWidth::kOne:
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
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void AmlSdmmc::SetBusFreq(SetBusFreqRequestView request, fdf::Arena& arena,
                          SetBusFreqCompleter::Sync& completer) {
  // This function is run after acquiring lock_, but the compiler doesn't recognize this.
  fit::function<zx_status_t()> task =
      [this, bus_freq = request->bus_freq]()
          __TA_NO_THREAD_SAFETY_ANALYSIS { return SetBusFreqImpl(bus_freq); };

  std::lock_guard<std::mutex> lock(lock_);

  if (power_suspended_) {
    zx_status_t status = ActivateWakeOnRequest();
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }

    // Delay this request until power has been resumed.
    delayed_requests_.emplace_back(
        SdmmcTaskInfo{std::move(task), std::move(arena), completer.ToAsync()});
    return;
  }

  DoTaskAndComplete(std::move(task), arena, completer);
}

zx_status_t AmlSdmmc::SetBusFreqImpl(uint32_t freq) {
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

  trace_async_id_ = TRACE_NONCE();
  TRACE_ASYNC_BEGIN("aml-sdmmc", "suspend", trace_async_id_);
  power_suspended_ = true;
  inspect_.power_suspended.Set(power_suspended_);
  FDF_LOGL(INFO, logger(), "Power suspended.");
  return ZX_OK;
}

zx_status_t AmlSdmmc::ResumePower() {
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

  TRACE_ASYNC_END("aml-sdmmc", "suspend", trace_async_id_);
  power_suspended_ = false;
  inspect_.power_suspended.Set(power_suspended_);
  FDF_LOGL(INFO, logger(), "Power resumed.");
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
  // This function is run after acquiring lock_, but the compiler doesn't recognize this.
  fit::function<zx_status_t()> task = [this]()
                                          __TA_NO_THREAD_SAFETY_ANALYSIS { return HwResetImpl(); };

  std::lock_guard<std::mutex> lock(lock_);

  if (power_suspended_) {
    zx_status_t status = ActivateWakeOnRequest();
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }

    // Delay this request until power has been resumed.
    delayed_requests_.emplace_back(
        SdmmcTaskInfo{std::move(task), std::move(arena), completer.ToAsync()});
    return;
  }

  DoTaskAndComplete(std::move(task), arena, completer);
}

zx_status_t AmlSdmmc::ActivateWakeOnRequest() {
  zx_status_t status =
      AcquireLease(wake_on_request_lessor_client_, wake_on_request_lease_control_client_end_);
  if (status != ZX_OK && status != ZX_ERR_ALREADY_BOUND) {
    FDF_LOGL(ERROR, logger(), "Failed to acquire lease during wake-on-request: %s",
             zx_status_get_string(status));
    return status;
  }

  if (status == ZX_OK) {
    // TODO(b/330223394): Update power level when ordered to do so by Power Broker via
    // RequiredLevel.
    UpdatePowerLevel(wake_on_request_current_level_client_, kPowerLevelOn);
    inspect_.wake_on_request_count.Add(1);
  }

  return ZX_OK;
}

template <typename T>
void AmlSdmmc::DoTaskAndComplete(fit::function<zx_status_t()> task, fdf::Arena& arena,
                                 T& completer) {
  zx_status_t status = task();
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::HwResetImpl() {
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
  // This function is run after acquiring lock_, but the compiler doesn't recognize this.
  fit::function<zx_status_t()> task =
      [this, timing = request->timing]()
          __TA_NO_THREAD_SAFETY_ANALYSIS { return SetTimingImpl(timing); };

  std::lock_guard<std::mutex> lock(lock_);

  if (power_suspended_) {
    zx_status_t status = ActivateWakeOnRequest();
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }

    // Delay this request until power has been resumed.
    delayed_requests_.emplace_back(
        SdmmcTaskInfo{std::move(task), std::move(arena), completer.ToAsync()});
    return;
  }

  DoTaskAndComplete(std::move(task), arena, completer);
}

zx_status_t AmlSdmmc::SetTimingImpl(fuchsia_hardware_sdmmc::wire::SdmmcTiming timing) {
  auto config = AmlSdmmcCfg::Get().ReadFrom(&*mmio_);
  if (timing == fuchsia_hardware_sdmmc::wire::SdmmcTiming::kHs400 ||
      timing == fuchsia_hardware_sdmmc::wire::SdmmcTiming::kHsddr ||
      timing == fuchsia_hardware_sdmmc::wire::SdmmcTiming::kDdr50) {
    if (timing == fuchsia_hardware_sdmmc::wire::SdmmcTiming::kHs400) {
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
  // Amlogic controller does not allow to modify voltage
  // We do not return an error here since things work fine without switching the voltage.
  completer.buffer(arena).ReplySuccess();
}

aml_sdmmc_desc_t* AmlSdmmc::SetupCmdDesc(const fuchsia_hardware_sdmmc::wire::SdmmcReq& req) {
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
    const fuchsia_hardware_sdmmc::wire::SdmmcReq& req, aml_sdmmc_desc_t* const cur_desc) {
  const uint32_t req_blk_len = log2_ceil(req.blocksize);
  if (req_blk_len > AmlSdmmcCfg::kMaxBlkLen) {
    FDF_LOGL(ERROR, logger(), "blocksize %u is greater than the max (%u)", 1 << req_blk_len,
             1 << AmlSdmmcCfg::kMaxBlkLen);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_blk_len(req_blk_len).WriteTo(&*mmio_);

  std::vector<fzl::PinnedVmo> pinned_vmos;
  pinned_vmos.reserve(req.buffers.count());

  aml_sdmmc_desc_t* desc = cur_desc;
  SdmmcVmoStore& vmos = registered_vmos_[req.client_id];
  for (const auto& buffer : req.buffers) {
    if (buffer.buffer.is_vmo()) {
      auto status = SetupUnownedVmoDescs(req, buffer, desc);
      if (!status.is_ok()) {
        return zx::error(status.error_value());
      }

      pinned_vmos.push_back(std::move(std::get<1>(status.value())));
      desc = std::get<0>(status.value());
    } else {
      vmo_store::StoredVmo<OwnedVmoInfo>* const stored_vmo = vmos.GetVmo(buffer.buffer.vmo_id());
      if (stored_vmo == nullptr) {
        FDF_LOGL(ERROR, logger(), "no VMO %u for client %u", buffer.buffer.vmo_id(), req.client_id);
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      auto status = SetupOwnedVmoDescs(req, buffer, *stored_vmo, desc);
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

zx::result<aml_sdmmc_desc_t*> AmlSdmmc::SetupOwnedVmoDescs(
    const fuchsia_hardware_sdmmc::wire::SdmmcReq& req,
    const fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion& buffer,
    vmo_store::StoredVmo<OwnedVmoInfo>& vmo, aml_sdmmc_desc_t* const cur_desc) {
  if (!(req.cmd_flags & SDMMC_CMD_READ) &&
      !(vmo.meta().rights &
        static_cast<uint32_t>(fuchsia_hardware_sdmmc::wire::SdmmcVmoRight::kRead))) {
    FDF_LOGL(ERROR, logger(), "Request would read from write-only VMO");
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }
  if ((req.cmd_flags & SDMMC_CMD_READ) &&
      !(vmo.meta().rights &
        static_cast<uint32_t>(fuchsia_hardware_sdmmc::wire::SdmmcVmoRight::kWrite))) {
    FDF_LOGL(ERROR, logger(), "Request would write to read-only VMO");
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  if (buffer.offset + buffer.size > vmo.meta().size) {
    FDF_LOGL(ERROR, logger(), "buffer reads past vmo end: offset %zu, size %zu, vmo size %zu",
             buffer.offset + vmo.meta().offset, buffer.size, vmo.meta().size);
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  fzl::PinnedVmo::Region regions[fuchsia_hardware_sdmmc::wire::kSdmmcPagesCount];
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
    const fuchsia_hardware_sdmmc::wire::SdmmcReq& req,
    const fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion& buffer,
    aml_sdmmc_desc_t* const cur_desc) {
  const bool is_read = req.cmd_flags & SDMMC_CMD_READ;
  const uint64_t pagecount =
      ((buffer.offset & PageMask()) + buffer.size + PageMask()) / zx_system_get_page_size();

  const zx::unowned_vmo vmo(buffer.buffer.vmo());
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

zx::result<aml_sdmmc_desc_t*> AmlSdmmc::PopulateDescriptors(
    const fuchsia_hardware_sdmmc::wire::SdmmcReq& req, aml_sdmmc_desc_t* const cur_desc,
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

zx_status_t AmlSdmmc::FinishReq(const fuchsia_hardware_sdmmc::wire::SdmmcReq& req) {
  if ((req.cmd_flags & SDMMC_RESP_DATA_PRESENT) && (req.cmd_flags & SDMMC_CMD_READ)) {
    for (const auto& region : req.buffers) {
      if (!region.buffer.is_vmo()) {
        continue;
      }

      // Invalidate the cache so that the next CPU read will pick up data that was written to main
      // memory by the controller.
      zx_status_t status =
          zx_vmo_op_range(region.buffer.vmo().get(), ZX_VMO_OP_CACHE_CLEAN_INVALIDATE,
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
  std::lock_guard<std::mutex> lock(lock_);

  if (power_suspended_) {
    FDF_LOGL(ERROR, logger(), "Rejecting TuningDoTransfer while power is suspended.");
    return ZX_ERR_BAD_STATE;
  }

  SetTuneSettings(context.new_settings);

  fuchsia_hardware_sdmmc::wire::SdmmcReq tuning_req;
  tuning_req.cmd_idx = context.cmd;
  tuning_req.cmd_flags = MMC_SEND_TUNING_BLOCK_FLAGS;
  tuning_req.arg = 0;
  tuning_req.blocksize = static_cast<uint32_t>(context.expected_block.size());
  tuning_req.suppress_error_messages = true;
  tuning_req.client_id = 0;

  fdf::Arena arena('AMSD');
  tuning_req.buffers.Allocate(arena, 1);
  zx::vmo dup;
  zx_status_t status = context.vmo->duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to duplicate vmo: %s", zx_status_get_string(status));
    return status;
  }
  tuning_req.buffers[0].buffer = fuchsia_hardware_sdmmc::wire::SdmmcBuffer::WithVmo(std::move(dup));
  tuning_req.buffers[0].offset = 0;
  tuning_req.buffers[0].size = context.expected_block.size();

  uint32_t unused_response[4];
  status = RequestImpl(tuning_req, unused_response);

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
  // This function is run after acquiring tuning_lock_, but the compiler doesn't recognize this.
  fit::function<zx_status_t()> task =
      [this, cmd_idx = request->cmd_idx]()
          __TA_NO_THREAD_SAFETY_ANALYSIS { return PerformTuningImpl(cmd_idx); };

  std::lock_guard<std::mutex> tuning_lock(tuning_lock_);

  {
    std::lock_guard<std::mutex> lock(lock_);
    if (power_suspended_) {
      zx_status_t status = ActivateWakeOnRequest();
      if (status != ZX_OK) {
        completer.buffer(arena).ReplyError(status);
        return;
      }

      // Delay this request until power has been resumed.
      delayed_requests_.emplace_back(
          SdmmcTaskInfo{std::move(task), std::move(arena), completer.ToAsync()});
      return;
    }
  }

  DoTaskAndComplete(std::move(task), arena, completer);
}

zx_status_t AmlSdmmc::PerformTuningImpl(uint32_t tuning_cmd_idx) {
  // Using a lambda for the constness of the resulting variables.
  const auto result = [this]() -> zx::result<std::tuple<uint32_t, uint32_t, TuneSettings>> {
    std::lock_guard<std::mutex> lock(lock_);

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
    std::lock_guard<std::mutex> lock(lock_);

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
  zx_status_t status = RegisterVmoImpl(request->vmo_id, request->client_id, std::move(request->vmo),
                                       request->offset, request->size, request->vmo_rights);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

zx_status_t AmlSdmmc::RegisterVmoImpl(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo,
                                      uint64_t offset, uint64_t size, uint32_t vmo_rights) {
  if (client_id > fuchsia_hardware_sdmmc::wire::kSdmmcMaxClientId) {
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
  const uint32_t read_perm =
      (vmo_rights & static_cast<uint32_t>(fuchsia_hardware_sdmmc::wire::SdmmcVmoRight::kRead))
          ? ZX_BTI_PERM_READ
          : 0;
  const uint32_t write_perm =
      (vmo_rights & static_cast<uint32_t>(fuchsia_hardware_sdmmc::wire::SdmmcVmoRight::kWrite))
          ? ZX_BTI_PERM_WRITE
          : 0;
  zx_status_t status = stored_vmo.Pin(bti_, read_perm | write_perm, true);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to pin VMO %u for client %u: %s", vmo_id, client_id,
             zx_status_get_string(status));
    return status;
  }

  std::lock_guard<std::mutex> lock(lock_);
  return registered_vmos_[client_id].RegisterWithKey(vmo_id, std::move(stored_vmo));
}

void AmlSdmmc::UnregisterVmo(UnregisterVmoRequestView request, fdf::Arena& arena,
                             UnregisterVmoCompleter::Sync& completer) {
  zx::vmo vmo;
  zx_status_t status = UnregisterVmoImpl(request->vmo_id, request->client_id, &vmo);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess(std::move(vmo));
}

zx_status_t AmlSdmmc::UnregisterVmoImpl(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo) {
  if (client_id > fuchsia_hardware_sdmmc::wire::kSdmmcMaxClientId) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::lock_guard<std::mutex> lock(lock_);

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
  std::lock_guard<std::mutex> lock(lock_);

  if (power_suspended_) {
    zx_status_t status = ActivateWakeOnRequest();
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }

    // Delay this request until power has been resumed.
    delayed_requests_.emplace_back(
        SdmmcRequestInfo{request, std::move(arena), completer.ToAsync()});
    return;
  }

  DoRequestAndComplete(request, arena, completer);
}

template <typename T>
void AmlSdmmc::DoRequestAndComplete(RequestRequestView request, fdf::Arena& arena, T& completer) {
  fidl::Array<uint32_t, 4> response;
  for (const auto& req : request->reqs) {
    zx_status_t status = RequestImpl(req, response.data());
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
  }
  completer.buffer(arena).ReplySuccess(response);
}

zx_status_t AmlSdmmc::RequestImpl(const fuchsia_hardware_sdmmc::wire::SdmmcReq& req,
                                  uint32_t out_response[4]) {
  if (req.client_id > fuchsia_hardware_sdmmc::wire::kSdmmcMaxClientId) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (shutdown_) {
    return ZX_ERR_CANCELED;
  }

  // Wait for the bus to become idle before issuing the next request. This could be necessary if the
  // card is driving CMD low after a voltage switch.
  WaitForBus();

  // stop executing
  AmlSdmmcStart::Get().ReadFrom(&*mmio_).set_desc_busy(0).WriteTo(&*mmio_);

  std::optional<std::vector<fzl::PinnedVmo>> pinned_vmos;

  aml_sdmmc_desc_t* desc = SetupCmdDesc(req);
  aml_sdmmc_desc_t* last_desc = desc;
  if (req.cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    auto status = SetupDataDescs(req, desc);
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
           &req, req.cmd_idx, desc->cmd_info, desc->data_addr, desc->cmd_arg);

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

  zx::result<std::array<uint32_t, AmlSdmmc::kResponseCount>> response = WaitForInterrupt(req);
  if (response.is_ok()) {
    memcpy(out_response, response.value().data(), sizeof(uint32_t) * AmlSdmmc::kResponseCount);
  }

  if (zx_status_t status = FinishReq(req); status != ZX_OK) {
    return status;
  }

  return response.status_value();
}

zx_status_t AmlSdmmc::Init(
    const fuchsia_hardware_platform_device::wire::NodeDeviceInfo& device_info) {
  std::lock_guard<std::mutex> lock(lock_);

  // The core clock must be enabled before attempting to access the start register.
  ConfigureDefaultRegs();

  // Stop processing DMA descriptors before releasing quarantine.
  AmlSdmmcStart::Get().ReadFrom(&*mmio_).set_desc_busy(0).WriteTo(&*mmio_);
  zx_status_t status = bti_.release_quarantine();
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to release quarantined pages");
    return status;
  }

  dev_info_.caps = static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostCap::kBusWidth8) |
                   static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostCap::kVoltage330) |
                   static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostCap::kSdr104) |
                   static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostCap::kSdr50) |
                   static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostCap::kDdr50) |
                   static_cast<uint64_t>(fuchsia_hardware_sdmmc::SdmmcHostCap::kDma);

  dev_info_.max_transfer_size = kMaxDmaDescriptors * zx_system_get_page_size();
  dev_info_.max_transfer_size_non_dma = AML_SDMMC_MAX_PIO_DATA_SIZE;

  inspect_.Init(device_info, inspector().root(), power_suspended_);
  inspect_.max_delay.Set(AmlSdmmcClock::kMaxDelay + 1);

  return ZX_OK;
}

void AmlSdmmc::PrepareStop(fdf::PrepareStopCompleter completer) {
  // If there's a pending request, wait for it to complete (and any pages to be unpinned).
  {
    std::lock_guard<std::mutex> lock(lock_);
    shutdown_ = true;

    descs_buffer_.reset();
  }

  completer(zx::ok());
}

}  // namespace aml_sdmmc
