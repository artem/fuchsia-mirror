// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <cstdint>

namespace amlogic_display {

zx::result<BoardInfo> GetBoardInfo(
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());
  fidl::WireResult<fuchsia_hardware_platform_device::Device::GetBoardInfo> result =
      fidl::WireCall(platform_device)->GetBoardInfo();
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to get board info: FIDL call failed: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to get board info: Platform device failed: %s",
           zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  if (!result.value()->has_vid()) {
    zxlogf(ERROR, "Failed to get board info: Vendor ID is missing");
    return zx::error(ZX_ERR_BAD_STATE);
  }
  if (!result.value()->has_pid()) {
    zxlogf(ERROR, "Failed to get board info: Product ID is missing");
    return zx::error(ZX_ERR_BAD_STATE);
  }
  BoardInfo board_info = {
      .board_vendor_id = result.value()->vid(),
      .board_product_id = result.value()->pid(),
  };
  return zx::ok(board_info);
}

zx::result<fdf::MmioBuffer> MapMmio(
    MmioResourceIndex mmio_index,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());
  fidl::WireResult<fuchsia_hardware_platform_device::Device::GetMmioById> result =
      fidl::WireCall(platform_device)->GetMmioById(static_cast<uint32_t>(mmio_index));
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to map MMIO resource #%" PRIu32 ": FIDL call failed: %s",
           static_cast<uint32_t>(mmio_index), result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to map MMIO resource #%" PRIu32 ": Platform device failed %s",
           static_cast<uint32_t>(mmio_index), zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  const fuchsia_hardware_platform_device::wire::Mmio* mmio_params = result->value();
  if (!mmio_params->has_offset() || !mmio_params->has_size() || !mmio_params->has_vmo()) {
    zxlogf(ERROR, "Failed to map MMIO resource #%" PRIu32 ": Platform device provided invalid MMIO",
           static_cast<uint32_t>(mmio_index));
    return zx::error(ZX_ERR_BAD_STATE);
  };

  zx::result<fdf::MmioBuffer> create_mmio_result = fdf::MmioBuffer::Create(
      mmio_params->offset(), mmio_params->size(), std::move(mmio_params->vmo()),
      /*cache_policy=*/ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (create_mmio_result.is_error()) {
    zxlogf(ERROR, "Failed to create fdf::MmioBuffer: %s", create_mmio_result.status_string());
    return create_mmio_result.take_error();
  }
  return zx::ok(std::move(create_mmio_result).value());
}

zx::result<zx::interrupt> GetInterrupt(
    InterruptResourceIndex interrupt_index,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());
  fidl::WireResult<fuchsia_hardware_platform_device::Device::GetInterruptById> result =
      fidl::WireCall(platform_device)
          ->GetInterruptById(static_cast<uint32_t>(interrupt_index), /*flags=*/0);
  if (result.status() != ZX_OK) {
    zxlogf(ERROR, "Failed to get interrupt resource #%" PRIu32 ": FIDL failed %s",
           static_cast<uint32_t>(interrupt_index), result.status_string());
    return zx::error(result.status());
  }
  if (!result->is_ok()) {
    zxlogf(ERROR, "Failed to get interrupt resource #%" PRIu32 ": Platform device failed %s",
           static_cast<uint32_t>(interrupt_index), zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  zx::interrupt interrupt = std::move(result->value()->irq);
  ZX_DEBUG_ASSERT_MSG(interrupt.is_valid(),
                      "GetInterruptById() succeeded but didn't populate the out-param");
  return zx::ok(std::move(interrupt));
}

zx::result<zx::bti> GetBti(
    BtiResourceIndex bti_index,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());
  fidl::WireResult<fuchsia_hardware_platform_device::Device::GetBtiById> result =
      fidl::WireCall(platform_device)->GetBtiById(static_cast<uint32_t>(bti_index));
  if (result.status() != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI resource #%" PRIu32 ": FIDL failed %s",
           static_cast<uint32_t>(bti_index), result.status_string());
    return zx::error(result.status());
  }
  if (!result->is_ok()) {
    zxlogf(ERROR, "Failed to get BTI resource #%" PRIu32 ": Platform device failed %s",
           static_cast<uint32_t>(bti_index), zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  zx::bti bti = std::move(result->value()->bti);
  ZX_DEBUG_ASSERT_MSG(bti.is_valid(), "GetBtiById() succeeded but didn't populate the out-param");
  return zx::ok(std::move(bti));
}

zx::result<zx::resource> GetSecureMonitorCall(
    SecureMonitorCallResourceIndex secure_monitor_call_index,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());
  fidl::WireResult<fuchsia_hardware_platform_device::Device::GetSmcById> result =
      fidl::WireCall(platform_device)->GetSmcById(static_cast<uint32_t>(secure_monitor_call_index));
  if (result.status() != ZX_OK) {
    zxlogf(ERROR, "Failed to get SMC resource #%" PRIu32 ": FIDL failed %s",
           static_cast<uint32_t>(secure_monitor_call_index), result.status_string());
    return zx::error(result.status());
  }
  if (!result->is_ok()) {
    zxlogf(ERROR, "Failed to get SMC resource #%" PRIu32 ": Platform device failed %s",
           static_cast<uint32_t>(secure_monitor_call_index),
           zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  zx::resource secure_monitor_call = std::move(result->value()->smc);
  ZX_DEBUG_ASSERT_MSG(secure_monitor_call.is_valid(),
                      "GetSmcById() succeeded but didn't populate the out-param");
  return zx::ok(std::move(secure_monitor_call));
}

}  // namespace amlogic_display
