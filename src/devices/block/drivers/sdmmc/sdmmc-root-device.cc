// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-root-device.h"

#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>

#include <memory>

#include <fbl/alloc_checker.h>

#include "sdio-controller-device.h"
#include "sdmmc-block-device.h"

namespace sdmmc {

zx_status_t SdmmcRootDevice::Bind(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  std::unique_ptr<SdmmcRootDevice> dev(new (&ac) SdmmcRootDevice(parent));

  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate device");
    return ZX_ERR_NO_MEMORY;
  }
  zx_status_t st = dev->DdkAdd(ddk::DeviceAddArgs("sdmmc")
                                   .set_flags(DEVICE_ADD_NON_BINDABLE)
                                   .forward_metadata(parent, DEVICE_METADATA_GPT_INFO));
  if (st != ZX_OK) {
    return st;
  }

  [[maybe_unused]] auto* placeholder = dev.release();
  return st;
}

// TODO(hanbinyoon): Simplify further using templated lambda come C++20.
// Returns nullptr if device was successfully added. Returns the SdmmcDevice if the probe failed
// (i.e., no eligible device present).
template <class DeviceType>
static zx::result<std::unique_ptr<SdmmcDevice>> MaybeAddDevice(
    const std::string& name, zx_device_t* zxdev, std::unique_ptr<SdmmcDevice> sdmmc,
    const fuchsia_hardware_sdmmc::wire::SdmmcMetadata& metadata) {
  std::unique_ptr<DeviceType> device;
  if (zx_status_t st = DeviceType::Create(zxdev, std::move(sdmmc), &device) != ZX_OK) {
    zxlogf(ERROR, "Failed to create %s device, retcode = %d", name.c_str(), st);
    return zx::error(st);
  }

  if (zx_status_t st = device->Probe(metadata); st != ZX_OK) {
    return zx::ok(std::move(device->TakeSdmmcDevice()));
  }

  if (zx_status_t st = device->AddDevice(); st != ZX_OK) {
    return zx::error(st);
  }

  [[maybe_unused]] auto* placeholder = device.release();
  return zx::ok(nullptr);
}

zx::result<fidl::ObjectView<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>>
SdmmcRootDevice::GetMetadata(fidl::AnyArena& allocator) {
  constexpr uint32_t kMaxCommandPacking = 16;

  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>(
      parent(), DEVICE_METADATA_SDMMC);
  if (decoded.is_error()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No metadata provided");
      return zx::ok(fidl::ObjectView(allocator,
                                     fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(allocator)
                                         .enable_trim(true)
                                         .enable_cache(true)
                                         .removable(false)
                                         .max_command_packing(kMaxCommandPacking)
                                         .Build()));
    } else {
      zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
      return decoded.take_error();
    }
  }

  // Default to trim and cache enabled, non-removable.
  return zx::ok(fidl::ObjectView(
      allocator,
      fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(allocator)
          .enable_trim(!decoded->has_enable_trim() || decoded->enable_trim())
          .enable_cache(!decoded->has_enable_cache() || decoded->enable_cache())
          .removable(decoded->has_removable() && decoded->removable())
          .max_command_packing(decoded->has_max_command_packing() ? decoded->max_command_packing()
                                                                  : kMaxCommandPacking)
          .Build()));
}

void SdmmcRootDevice::DdkInit(ddk::InitTxn txn) {
  auto sdmmc = std::make_unique<SdmmcDevice>(parent());
  zx_status_t st = sdmmc->Init(this);
  if (st != ZX_OK) {
    zxlogf(ERROR, "failed to get host info");
    return txn.Reply(st);
  }

  zxlogf(DEBUG, "host caps dma %d 8-bit bus %d max_transfer_size %" PRIu64 "",
         sdmmc->UseDma() ? 1 : 0, (sdmmc->host_info().caps & SDMMC_HOST_CAP_BUS_WIDTH_8) ? 1 : 0,
         sdmmc->host_info().max_transfer_size);

  fidl::Arena arena;
  const zx::result metadata = GetMetadata(arena);
  if (metadata.is_error()) {
    return txn.Reply(metadata.status_value());
  }

  // Reset the card.
  sdmmc->HwReset();

  // No matter what state the card is in, issuing the GO_IDLE_STATE command will
  // put the card into the idle state.
  if ((st = sdmmc->SdmmcGoIdle()) != ZX_OK) {
    zxlogf(ERROR, "SDMMC_GO_IDLE_STATE failed, retcode = %d", st);
    return txn.Reply(st);
  }

  // Probe for SDIO first, then SD/MMC.
  zx::result<std::unique_ptr<SdmmcDevice>> result =
      MaybeAddDevice<SdioControllerDevice>("sdio", zxdev(), std::move(sdmmc), **metadata);
  if (result.is_error()) {
    return txn.Reply(result.status_value());
  } else if (*result == nullptr) {
    return txn.Reply(ZX_OK);
  }
  result = MaybeAddDevice<SdmmcBlockDevice>("block", zxdev(), std::move(*result), **metadata);
  if (result.is_error()) {
    return txn.Reply(result.status_value());
  } else if (*result == nullptr) {
    return txn.Reply(ZX_OK);
  }

  if (metadata->removable()) {
    // This controller is connected to a removable card slot, and no card was inserted. Indicate
    // success so that our device remains available.
    // TODO(fxbug.dev/130283): Enable detection of card insert/removal after initialization.
    zxlogf(INFO, "failed to probe removable device");
    return txn.Reply(ZX_OK);
  }

  // Failure to probe a hardwired device is unexpected. Reply with an error code so that our device
  // gets removed.
  zxlogf(ERROR, "failed to probe irremovable device");
  return txn.Reply(ZX_ERR_NOT_FOUND);
}

void SdmmcRootDevice::DdkRelease() { delete this; }

}  // namespace sdmmc

static constexpr zx_driver_ops_t sdmmc_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = sdmmc::SdmmcRootDevice::Bind;
  return ops;
}();

ZIRCON_DRIVER(sdmmc, sdmmc_driver_ops, "zircon", "0.1");
