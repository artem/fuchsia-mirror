// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/display-engine.h"

#include <fidl/fuchsia.hardware.amlogiccanvas/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/component/incoming/cpp/constants.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/image-format/image_format.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/bti.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/limits.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <cinttypes>
#include <cstddef>
#include <memory>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"
#include "src/graphics/display/drivers/amlogic-display/capture.h"
#include "src/graphics/display/drivers/amlogic-display/hot-plug-detection.h"
#include "src/graphics/display/drivers/amlogic-display/pixel-grid-size2d.h"
#include "src/graphics/display/drivers/amlogic-display/vout.h"
#include "src/graphics/display/drivers/amlogic-display/vsync-receiver.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/metadata/metadata-getter.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/namespace/namespace.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace amlogic_display {

// Currently amlogic-display implementation uses pointers to ImageInfo as
// handles to images, while handles to images are defined as a fixed-size
// uint64_t in the banjo protocol. This works on platforms where uint64_t and
// uintptr_t are equivalent but this may cause portability issues in the future.
// TODO(https://fxbug.dev/42079128): Do not use pointers as handles.
static_assert(std::is_same_v<uint64_t, uintptr_t>);

namespace {

// List of supported pixel formats.
// TODO(https://fxbug.dev/42148348): Add more supported formats.
constexpr std::array<fuchsia_images2::wire::PixelFormat, 2> kSupportedPixelFormats = {
    fuchsia_images2::wire::PixelFormat::kB8G8R8A8,
    fuchsia_images2::wire::PixelFormat::kR8G8B8A8,
};

constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 2> kSupportedBanjoPixelFormats = {
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
};

constexpr uint32_t kBufferAlignment = 64;

bool IsFormatSupported(fuchsia_images2::wire::PixelFormat format) {
  return std::find(kSupportedPixelFormats.begin(), kSupportedPixelFormats.end(), format) !=
         kSupportedPixelFormats.end();
}

void SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType format,
                                      uint64_t modifier,
                                      fuchsia_sysmem::wire::ImageFormatConstraints& constraints) {
  constraints.color_spaces_count = 1;
  constraints.color_space[0].type = fuchsia_sysmem::wire::ColorSpaceType::kSrgb;
  constraints.pixel_format = {
      .type = format,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = modifier,
          },
  };
  constraints.bytes_per_row_divisor = kBufferAlignment;
  constraints.start_offset_divisor = kBufferAlignment;
}

ColorSpaceConversionMode GetColorSpaceConversionMode(VoutType vout_type) {
  switch (vout_type) {
    case VoutType::kDsi:
      return ColorSpaceConversionMode::kRgbInternalRgbOut;
    case VoutType::kHdmi:
      return ColorSpaceConversionMode::kRgbInternalYuvOut;
  }
  ZX_ASSERT_MSG(false, "Invalid VoutType: %u", static_cast<uint8_t>(vout_type));
}

// TODO(https://fxbug.dev/329375540): Use driver metadata instead of hardcoded
// per-board rules to determine the display engine reset policy.
bool IsFullHardwareResetRequired(
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device) {
  constexpr bool kDefaultValue = true;

  zx::result<BoardInfo> board_info_result = GetBoardInfo(platform_device);
  if (board_info_result.is_error()) {
    zxlogf(ERROR,
           "Failed to get board information: %s. Falling back to "
           "default option (%d)",
           board_info_result.status_string(), kDefaultValue);
    return kDefaultValue;
  }

  const uint32_t vendor_id = board_info_result->board_vendor_id;
  const uint32_t product_id = board_info_result->board_product_id;

  // On Khadas VIM3, the bootloader display driver may set the display engine
  // and the display device in an invalid state. We completely clear the stale
  // hardware configuration by performing a reset on the display engine.
  if (vendor_id == PDEV_VID_KHADAS && product_id == PDEV_PID_VIM3) {
    return true;
  }

  // On Astro, Sherlock and Nelson, the bootloader display driver sets up the
  // display engine and the panel. The Fuchsia driver doesn't initialize the
  // hardware registers and only loads the current hardware state.
  if (vendor_id == PDEV_VID_GOOGLE && product_id == PDEV_PID_ASTRO) {
    return false;
  }
  if (vendor_id == PDEV_VID_GOOGLE && product_id == PDEV_PID_SHERLOCK) {
    return false;
  }
  if (vendor_id == PDEV_VID_GOOGLE && product_id == PDEV_PID_NELSON) {
    return false;
  }

  zxlogf(INFO,
         "Unknown board type (vid=%" PRIx32 ", pid=%" PRIx32
         "). Falling back to default option (%d).",
         vendor_id, product_id, kDefaultValue);
  return kDefaultValue;
}

}  // namespace

bool DisplayEngine::IgnoreDisplayMode() const {
  // The DSI specification doesn't support switching display modes. The display
  // mode is provided by the peripheral supplier through side channels and
  // should be fixed while the display device is available.
  ZX_DEBUG_ASSERT(vout_ != nullptr);
  return vout_->type() == VoutType::kDsi;
}

bool DisplayEngine::IsNewDisplayTiming(const display::DisplayTiming& timing) {
  return current_display_timing_ != timing;
}

zx_status_t DisplayEngine::DisplayControllerImplSetMinimumRgb(uint8_t minimum_rgb) {
  if (fully_initialized()) {
    video_input_unit_->SetMinimumRgb(minimum_rgb);
    return ZX_OK;
  }
  return ZX_ERR_INTERNAL;
}

zx::result<> DisplayEngine::ResetDisplayEngine() {
  ZX_DEBUG_ASSERT(!fully_initialized());
  zxlogf(TRACE, "Display engine reset started.");

  zx::result<> result = vout_->PowerOff();
  if (!result.is_ok()) {
    zxlogf(ERROR, "Failed to power off Vout before VPU reset: %s", result.status_string());
  }

  vpu_->PowerOff();
  vpu_->PowerOn();

  // TODO(https://fxbug.dev/42082920): Instead of enabling it ad-hoc here, make
  // `Vpu::PowerOn()` idempotent and always enable the AFBC in `Vpu::PowerOn()`.
  vpu_->AfbcPower(/*power_on=*/true);

  const ColorSpaceConversionMode color_conversion_mode = GetColorSpaceConversionMode(vout_->type());
  vpu_->SetupPostProcessorColorConversion(color_conversion_mode);
  // All the VPU registers are now reset. We need to claim the hardware
  // ownership again.
  vpu_->CheckAndClaimHardwareOwnership();

  result = vout_->PowerOn();
  if (!result.is_ok()) {
    zxlogf(ERROR, "Failed to power on Vout after VPU reset: %s", result.status_string());
    return result.take_error();
  }

  zxlogf(TRACE, "Display engine reset finished successfully.");
  return zx::ok();
}

void DisplayEngine::DisplayControllerImplSetDisplayControllerInterface(
    const display_controller_interface_protocol_t* intf) {
  fbl::AutoLock display_lock(&display_mutex_);
  dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient(intf);

  if (display_attached_) {
    added_display_args_t args;
    vout_->PopulateAddedDisplayArgs(&args, display_id_, kSupportedBanjoPixelFormats);
    dc_intf_.OnDisplaysChanged(&args, 1, nullptr, 0);
  }
}

void DisplayEngine::DisplayControllerImplResetDisplayControllerInterface() {
  fbl::AutoLock lock(&display_mutex_);
  dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient();
}

zx_status_t DisplayEngine::DisplayControllerImplImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    zxlogf(ERROR, "Buffer Collection (id=%lu) already exists", driver_buffer_collection_id.value());
    return ZX_ERR_ALREADY_EXISTS;
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_.is_valid(), "sysmem allocator is not initialized");

  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollection>();
  if (!endpoints.is_ok()) {
    zxlogf(ERROR, "Failed to create sysmem BufferCollection endpoints: %s",
           endpoints.status_string());
    return ZX_ERR_INTERNAL;
  }
  auto& [collection_client_endpoint, collection_server_endpoint] = endpoints.value();

  auto bind_result = sysmem_->BindSharedCollection(
      fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>(std::move(collection_token)),
      std::move(collection_server_endpoint));
  if (!bind_result.ok()) {
    zxlogf(ERROR, "Failed to complete FIDL call BindSharedCollection: %s",
           bind_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Failed to release buffer collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  buffer_collections_.erase(driver_buffer_collection_id);
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplImportImage(
    const image_metadata_t* image_metadata, uint64_t banjo_driver_buffer_collection_id,
    uint32_t index, uint64_t* out_image_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Failed to import Image on collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  zx_status_t status = ZX_OK;
  auto import_info = std::make_unique<ImageInfo>();
  if (import_info == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }

  if (image_metadata->tiling_type != IMAGE_TILING_TYPE_LINEAR) {
    status = ZX_ERR_INVALID_ARGS;
    return status;
  }

  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& collection =
      buffer_collections_.at(driver_buffer_collection_id);
  fidl::WireResult check_result = collection->CheckBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    return check_result.status();
  }
  const auto& check_response = check_result.value();
  if (check_response.status == ZX_ERR_UNAVAILABLE) {
    return ZX_ERR_SHOULD_WAIT;
  }
  if (check_response.status != ZX_OK) {
    return check_response.status;
  }

  fidl::WireResult wait_result = collection->WaitForBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    return wait_result.status();
  }
  auto& wait_response = wait_result.value();
  if (wait_response.status != ZX_OK) {
    return wait_response.status;
  }
  fuchsia_sysmem::wire::BufferCollectionInfo2& collection_info =
      wait_response.buffer_collection_info;

  if (!collection_info.settings.has_image_format_constraints ||
      index >= collection_info.buffer_count) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (const auto v2_pixel_format = sysmem::V2CopyFromV1PixelFormatType(
          collection_info.settings.image_format_constraints.pixel_format.type);
      !format_support_check_(v2_pixel_format)) {
    zxlogf(ERROR, "Failed to import image: pixel format %u not supported",
           static_cast<uint32_t>(v2_pixel_format));
    return ZX_ERR_NOT_SUPPORTED;
  }

  import_info->pixel_format = sysmem::V2CopyFromV1PixelFormat(
      collection_info.settings.image_format_constraints.pixel_format);

  ZX_DEBUG_ASSERT(
      collection_info.settings.image_format_constraints.pixel_format.has_format_modifier);

  const auto format_modifier =
      collection_info.settings.image_format_constraints.pixel_format.format_modifier.value;

  switch (format_modifier) {
    case fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuv:
    case fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTe: {
      // AFBC does not use canvas.
      uint64_t offset = collection_info.buffers[index].vmo_usable_start;
      size_t size =
          fbl::round_up(ImageFormatImageSize(ImageConstraintsToFormat(
                                                 collection_info.settings.image_format_constraints,
                                                 image_metadata->width, image_metadata->height)
                                                 .value()),
                        ZX_PAGE_SIZE);
      zx_paddr_t paddr;
      zx_status_t status =
          bti_.pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, collection_info.buffers[index].vmo,
                   offset & ~(ZX_PAGE_SIZE - 1), size, &paddr, 1, &import_info->pmt);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to pin BTI: %s", zx_status_get_string(status));
        return status;
      }
      import_info->paddr = paddr;
      import_info->image_height = image_metadata->height;
      import_info->image_width = image_metadata->width;
      import_info->is_afbc = true;
    } break;
    case fuchsia_sysmem::wire::kFormatModifierLinear:
    case fuchsia_sysmem::wire::kFormatModifierArmLinearTe: {
      uint32_t minimum_row_bytes;
      if (!ImageFormatMinimumRowBytes(collection_info.settings.image_format_constraints,
                                      image_metadata->width, &minimum_row_bytes)) {
        zxlogf(ERROR, "Invalid image width %d for collection", image_metadata->width);
        return ZX_ERR_INVALID_ARGS;
      }

      fuchsia_hardware_amlogiccanvas::wire::CanvasInfo canvas_info;
      canvas_info.height = image_metadata->height;
      canvas_info.stride_bytes = minimum_row_bytes;
      canvas_info.blkmode = fuchsia_hardware_amlogiccanvas::CanvasBlockMode::kLinear;
      canvas_info.endianness = fuchsia_hardware_amlogiccanvas::CanvasEndianness();
      canvas_info.flags = fuchsia_hardware_amlogiccanvas::CanvasFlags::kRead;

      fidl::WireResult result =
          canvas_->Config(std::move(collection_info.buffers[index].vmo),
                          collection_info.buffers[index].vmo_usable_start, canvas_info);
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to configure canvas: %s", result.error().FormatDescription().c_str());
        return ZX_ERR_NO_RESOURCES;
      }
      fidl::WireResultUnwrapType<fuchsia_hardware_amlogiccanvas::Device::Config>& response =
          result.value();
      if (response.is_error()) {
        zxlogf(ERROR, "Failed to configure canvas: %s",
               zx_status_get_string(response.error_value()));
        return ZX_ERR_NO_RESOURCES;
      }

      import_info->canvas = canvas_.client_end();
      import_info->canvas_idx = result->value()->canvas_idx;
      import_info->image_height = image_metadata->height;
      import_info->image_width = image_metadata->width;
      import_info->is_afbc = false;
    } break;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid pixel format modifier: %lu", format_modifier);
      return ZX_ERR_INVALID_ARGS;
  }
  // TODO(https://fxbug.dev/42079128): Using pointers as handles impedes portability of
  // the driver. Do not use pointers as handles.
  *out_image_handle = reinterpret_cast<uint64_t>(import_info.get());
  fbl::AutoLock lock(&image_mutex_);
  imported_images_.push_back(std::move(import_info));
  return status;
}

void DisplayEngine::DisplayControllerImplReleaseImage(uint64_t image_handle) {
  fbl::AutoLock lock(&image_mutex_);
  auto info = reinterpret_cast<ImageInfo*>(image_handle);
  imported_images_.erase(*info);
}

config_check_result_t DisplayEngine::DisplayControllerImplCheckConfiguration(
    const display_config_t** display_configs, size_t display_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual) {
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = 0;
  }
  if (display_count != 1) {
    ZX_DEBUG_ASSERT(display_count == 0);
    return CONFIG_CHECK_RESULT_OK;
  }

  fbl::AutoLock lock(&display_mutex_);

  // no-op, just wait for the client to try a new config
  if (!display_attached_ || display::ToDisplayId(display_configs[0]->display_id) != display_id_) {
    return CONFIG_CHECK_RESULT_OK;
  }

  display::DisplayTiming display_timing = display::ToDisplayTiming(display_configs[0]->mode);
  if (!IgnoreDisplayMode()) {
    // `current_display_timing_` is already applied to the display so it's
    // guaranteed to be supported. We only perform the timing check if there
    // is a new `display_timing`.
    if (IsNewDisplayTiming(display_timing) && !vout_->IsDisplayTimingSupported(display_timing)) {
      return CONFIG_CHECK_RESULT_UNSUPPORTED_MODES;
    }
  }

  bool success = true;

  ZX_DEBUG_ASSERT(client_composition_opcodes_count >= display_configs[0]->layer_count);
  cpp20::span<client_composition_opcode_t> client_composition_opcodes(
      out_client_composition_opcodes_list, display_configs[0]->layer_count);
  std::fill(client_composition_opcodes.begin(), client_composition_opcodes.end(), 0);
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = client_composition_opcodes.size();
  }

  if (display_configs[0]->layer_count > 1) {
    // We only support 1 layer
    success = false;
  }

  // TODO(https://fxbug.dev/42080882): Move color conversion validation code to a common
  // library.
  if (success && display_configs[0]->cc_flags) {
    // Make sure cc values are correct
    if (display_configs[0]->cc_flags & COLOR_CONVERSION_PREOFFSET) {
      for (float cc_preoffset : display_configs[0]->cc_preoffsets) {
        success = success && cc_preoffset > -1;
        success = success && cc_preoffset < 1;
      }
    }
    if (success && display_configs[0]->cc_flags & COLOR_CONVERSION_POSTOFFSET) {
      for (float cc_postoffset : display_configs[0]->cc_postoffsets) {
        success = success && cc_postoffset > -1;
        success = success && cc_postoffset < 1;
      }
    }
  }

  if (success) {
    const uint32_t width = display_timing.horizontal_active_px;
    const uint32_t height = display_timing.vertical_active_lines;
    // Make sure ther layer configuration is supported
    const primary_layer_t& layer = display_configs[0]->layer_list[0]->cfg.primary;
    // TODO(https://fxbug.dev/42080883) Instead of using memcmp() to compare the frame
    // with expected frames, we should use the common type in "api-types-cpp"
    // which supports comparison opeartors.
    frame_t frame = {
        .x_pos = 0,
        .y_pos = 0,
        .width = width,
        .height = height,
    };

    if (layer.alpha_mode == ALPHA_PREMULTIPLIED) {
      // we don't support pre-multiplied alpha mode
      client_composition_opcodes[0] |= CLIENT_COMPOSITION_OPCODE_ALPHA;
    }
    success = display_configs[0]->layer_list[0]->type == LAYER_TYPE_PRIMARY &&
              layer.transform_mode == FRAME_TRANSFORM_IDENTITY && layer.image.width == width &&
              layer.image.height == height &&
              memcmp(&layer.dest_frame, &frame, sizeof(frame_t)) == 0 &&
              memcmp(&layer.src_frame, &frame, sizeof(frame_t)) == 0;
  }
  if (!success) {
    client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
    for (unsigned i = 1; i < display_configs[0]->layer_count; i++) {
      client_composition_opcodes[i] = CLIENT_COMPOSITION_OPCODE_MERGE_SRC;
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

void DisplayEngine::DisplayControllerImplApplyConfiguration(
    const display_config_t** display_configs, size_t display_count,
    const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(display_configs);
  ZX_DEBUG_ASSERT(banjo_config_stamp);
  const display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);

  fbl::AutoLock lock(&display_mutex_);

  if (display_count == 1 && display_configs[0]->layer_count) {
    if (!IgnoreDisplayMode()) {
      // Perform Vout modeset iff there's a new display mode.
      //
      // Setting up OSD may require Vout framebuffer information, which may be
      // changed on each ApplyConfiguration(), so we need to apply the
      // configuration to Vout first before initializing the display and OSD.
      display::DisplayTiming display_timing = display::ToDisplayTiming(display_configs[0]->mode);
      if (IsNewDisplayTiming(display_timing)) {
        zx::result<> apply_config_result = vout_->ApplyConfiguration(display_timing);
        if (!apply_config_result.is_ok()) {
          zxlogf(ERROR, "Failed to apply config to Vout: %s", apply_config_result.status_string());
          return;
        }
        current_display_timing_ = display_timing;
      }
    }

    // The only way a checked configuration could now be invalid is if display was
    // unplugged. If that's the case, then the upper layers will give a new configuration
    // once they finish handling the unplug event. So just return.
    if (!display_attached_ || display::ToDisplayId(display_configs[0]->display_id) != display_id_) {
      return;
    }

    // Since Amlogic does not support plug'n play (fixed display), there is no way
    // a checked configuration could be invalid at this point.
    video_input_unit_->FlipOnVsync(display_configs[0], config_stamp);
  } else {
    if (fully_initialized()) {
      {
        fbl::AutoLock lock2(&capture_mutex_);
        if (current_capture_target_image_ != nullptr) {
          // there's an active capture. stop it before disabling osd
          vpu_->CaptureDone();
          current_capture_target_image_ = nullptr;
        }
      }
      video_input_unit_->DisableLayer(config_stamp);
    }
  }

  // If bootloader does not enable any of the display hardware, no vsync will be generated.
  // This fakes a vsync to let clients know we are ready until we actually initialize hardware
  if (!fully_initialized()) {
    if (dc_intf_.is_valid()) {
      if (display_count == 0 || display_configs[0]->layer_count == 0) {
        const config_stamp_t banjo_config_stamp_out = display::ToBanjoConfigStamp(config_stamp);
        dc_intf_.OnDisplayVsync(display::ToBanjoDisplayId(display_id_), zx_clock_get_monotonic(),
                                &banjo_config_stamp_out);
      }
    }
  }
}

void DisplayEngine::Deinitialize() {
  vsync_receiver_.reset();

  // TODO(https://fxbug.dev/42082206): Power off should occur after all threads are
  // destroyed. Otherwise other threads may still write to the VPU MMIO which
  // can cause the system to hang.
  if (fully_initialized()) {
    video_input_unit_->Release();
    vpu_->PowerOff();
  }

  capture_.reset();
  hot_plug_detection_.reset();
}

zx_status_t DisplayEngine::DisplayControllerImplGetSysmemConnection(zx::channel connection) {
  fidl::ServerEnd<fuchsia_sysmem::Allocator> allocator_server(std::move(connection));
  zx::result<> connect_result = incoming_.Connect<fuchsia_hardware_sysmem::Service::AllocatorV1>(
      std::move(allocator_server), "sysmem");
  if (connect_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to sysmem Allocator service: %s",
           connect_result.status_string());
    return connect_result.status_value();
  }
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplSetBufferCollectionConstraints(
    const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR,
           "Failed to set buffer collection constraints for %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& collection =
      buffer_collections_.at(driver_buffer_collection_id);
  fuchsia_sysmem::wire::BufferCollectionConstraints constraints = {};
  const char* buffer_name;
  if (usage->tiling_type == IMAGE_TILING_TYPE_CAPTURE) {
    constraints.usage.cpu =
        fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  } else {
    constraints.usage.display = fuchsia_sysmem::wire::kDisplayUsageLayer;
  }
  constraints.has_buffer_memory_constraints = true;
  fuchsia_sysmem::wire::BufferMemoryConstraints& buffer_constraints =
      constraints.buffer_memory_constraints;
  buffer_constraints.physically_contiguous_required = true;
  buffer_constraints.secure_required = false;
  buffer_constraints.ram_domain_supported = true;
  buffer_constraints.cpu_domain_supported = false;
  buffer_constraints.inaccessible_domain_supported = true;
  buffer_constraints.heap_permitted_count = 2;
  buffer_constraints.heap_permitted[0] = fuchsia_sysmem::wire::HeapType::kSystemRam;
  buffer_constraints.heap_permitted[1] = fuchsia_sysmem::wire::HeapType::kAmlogicSecure;

  if (usage->tiling_type == IMAGE_TILING_TYPE_CAPTURE) {
    constraints.image_format_constraints_count = 1;
    fuchsia_sysmem::wire::ImageFormatConstraints& image_constraints =
        constraints.image_format_constraints[0];

    SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType::kBgr24,
                                     fuchsia_sysmem::wire::kFormatModifierLinear,
                                     image_constraints);

    const PixelGridSize2D display_contents_size = video_input_unit_->display_contents_size();
    image_constraints.min_coded_width = display_contents_size.width;
    image_constraints.max_coded_width = display_contents_size.width;
    image_constraints.min_coded_height = display_contents_size.height;
    image_constraints.max_coded_height = display_contents_size.height;
    // Amlogic display capture engine (VDIN) outputs in formats with 3 bytes per
    // pixel.
    constexpr uint32_t kCaptureImageBytesPerPixel = 3;
    image_constraints.min_bytes_per_row =
        fbl::round_up(display_contents_size.width * kCaptureImageBytesPerPixel, kBufferAlignment);
    image_constraints.max_coded_width_times_coded_height =
        display_contents_size.width * display_contents_size.height;
    buffer_name = "Display capture";
  } else {
    // TODO(https://fxbug.dev/42176441): Currently the buffer collection constraints are
    // applied to all displays. If the |vout_| device type changes, then the
    // existing image formats might not work for the new device type. To resolve
    // this, the driver should set per-display buffer collection constraints
    // instead.
    constraints.image_format_constraints_count = 0;
    ZX_DEBUG_ASSERT(format_support_check_ != nullptr);
    if (format_support_check_(fuchsia_images2::wire::PixelFormat::kB8G8R8A8)) {
      for (const auto format_modifier : {fuchsia_sysmem::wire::kFormatModifierLinear,
                                         fuchsia_sysmem::wire::kFormatModifierArmLinearTe}) {
        const size_t index = constraints.image_format_constraints_count++;
        auto& image_constraints = constraints.image_format_constraints[index];
        SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType::kBgra32,
                                         format_modifier, image_constraints);
      }
    }
    if (format_support_check_(fuchsia_images2::wire::PixelFormat::kR8G8B8A8)) {
      for (const auto format_modifier :
           {fuchsia_sysmem::wire::kFormatModifierLinear,
            fuchsia_sysmem::wire::kFormatModifierArmLinearTe,
            fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuv,
            fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTe}) {
        const size_t index = constraints.image_format_constraints_count++;
        auto& image_constraints = constraints.image_format_constraints[index];
        SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType::kR8G8B8A8,
                                         format_modifier, image_constraints);
      }
    }
    buffer_name = "Display";
  }

  // Set priority to 10 to override the Vulkan driver name priority of 5, but be less than most
  // application priorities.
  constexpr uint32_t kNamePriority = 10;
  fidl::OneWayStatus set_name_result =
      collection->SetName(kNamePriority, fidl::StringView::FromExternal(buffer_name));
  if (!set_name_result.ok()) {
    zxlogf(ERROR, "Failed to set name: %d", set_name_result.status());
    return set_name_result.status();
  }
  fidl::OneWayStatus set_constraints_result = collection->SetConstraints(true, constraints);
  if (!set_constraints_result.ok()) {
    zxlogf(ERROR, "Failed to set constraints: %d", set_constraints_result.status());
    return set_constraints_result.status();
  }

  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplSetDisplayPower(uint64_t display_id,
                                                                bool power_on) {
  fbl::AutoLock lock(&display_mutex_);
  if (display::ToDisplayId(display_id) != display_id_ || !display_attached_) {
    return ZX_ERR_NOT_FOUND;
  }
  if (power_on) {
    zx::result<> power_on_result = vout_->PowerOn();
    if (power_on_result.is_ok()) {
      // Powering on the display panel also resets the display mode set on the
      // display. This clears the display mode set previously to force a Vout
      // modeset to be performed on the next ApplyConfiguration().
      current_display_timing_ = {};
    }
    return power_on_result.status_value();
  }
  return vout_->PowerOff().status_value();
}

bool DisplayEngine::DisplayControllerImplIsCaptureSupported() { return true; }

zx_status_t DisplayEngine::DisplayControllerImplImportImageForCapture(
    uint64_t banjo_driver_buffer_collection_id, uint32_t index, uint64_t* out_capture_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR,
           "Failed to import capture image on collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& collection =
      buffer_collections_.at(driver_buffer_collection_id);
  auto import_capture = std::make_unique<ImageInfo>();
  if (import_capture == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }
  fbl::AutoLock lock(&capture_mutex_);
  fidl::WireResult check_result = collection->CheckBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    return check_result.status();
  }
  const auto& check_response = check_result.value();
  if (check_response.status == ZX_ERR_UNAVAILABLE) {
    return ZX_ERR_SHOULD_WAIT;
  }
  if (check_response.status != ZX_OK) {
    return check_response.status;
  }

  fidl::WireResult wait_result = collection->WaitForBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    return wait_result.status();
  }
  auto& wait_response = wait_result.value();
  if (wait_response.status != ZX_OK) {
    return wait_response.status;
  }
  fuchsia_sysmem::wire::BufferCollectionInfo2& collection_info =
      wait_response.buffer_collection_info;

  if (!collection_info.settings.has_image_format_constraints ||
      index >= collection_info.buffer_count) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // Ensure the proper format
  ZX_DEBUG_ASSERT(collection_info.settings.image_format_constraints.pixel_format.type ==
                  fuchsia_sysmem::wire::PixelFormatType::kBgr24);

  // Allocate a canvas for the capture image
  fuchsia_hardware_amlogiccanvas::wire::CanvasInfo canvas_info;
  canvas_info.height = collection_info.settings.image_format_constraints.min_coded_height;
  canvas_info.stride_bytes = collection_info.settings.image_format_constraints.min_bytes_per_row;
  canvas_info.blkmode = fuchsia_hardware_amlogiccanvas::CanvasBlockMode::kLinear;

  // Canvas images are by default little-endian for each 128-bit (16-byte)
  // chunk. By default, for 8-bit YUV444 images, the pixels are interpreted as
  //   Y0  U0  V0  Y1  U1  V1  Y2  U2    V2  Y3  U3  V3  Y4  U4  V4  Y5...
  //
  // However, capture memory interface uses big-endian for each 128-bit
  // (16-byte) chunk (defined in Vpu::CaptureInit), and the high- and low-64
  // bits (8 bytes) are already swapped. This is effectively big-endian for
  // each 64-bit chunk. So, the 8-bit YUV444 pixels are stored by the capture
  // memory interface as
  //   U2  Y2  V1  U1  Y1  V0  U0  Y0    Y5  V4  U4  Y4  V3  U3  Y3  V2...
  //
  // In order to read / write the captured canvas image correctly, the canvas
  // endianness must match that of capture memory interface.
  //
  // To convert 128-bit little-endian to 64-bit big-endian, we need to swap
  // every 8-bit pairs, 16-bit pairs and 32-bit pairs within every 64-bit chunk:
  //
  //   The original bytes written by the capture memory interface:
  //     U2  Y2  V1  U1  Y1  V0  U0  Y0    Y5  V4  U4  Y4  V3  U3  Y3  V2...
  //   Swapping every 8-bit pairs we get:
  //     Y2  U2  U1  V1  V0  Y1  Y0  U0    V4  Y5  Y4  U4  U3  V3  V2  Y3...
  //   Then we swap every 16-bit pairs:
  //     U1  V1  Y2  U2  Y0  U0  V0  Y1    Y4  U4  V4  Y5  V2  Y3  U3  V3...
  //   Then we swap every 32-bit pairs:
  //     Y0  U0  V0  Y1  U1  V1  Y2  U2    V2  Y3  U3  V3  Y4  U4  V4  Y5...
  //   Then we got the correct pixel interpretation.
  constexpr fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness kCanvasBigEndian64Bit =
      fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness::kSwap8BitPairs |
      fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness::kSwap16BitPairs |
      fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness::kSwap32BitPairs;

  canvas_info.endianness = fuchsia_hardware_amlogiccanvas::CanvasEndianness(kCanvasBigEndian64Bit);
  canvas_info.flags = fuchsia_hardware_amlogiccanvas::CanvasFlags::kRead |
                      fuchsia_hardware_amlogiccanvas::CanvasFlags::kWrite;

  fidl::WireResult result =
      canvas_->Config(std::move(collection_info.buffers[index].vmo),
                      collection_info.buffers[index].vmo_usable_start, canvas_info);
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to configure canvas: %s", result.error().FormatDescription().c_str());
    return ZX_ERR_NO_RESOURCES;
  }
  fidl::WireResultUnwrapType<fuchsia_hardware_amlogiccanvas::Device::Config>& response =
      result.value();
  if (response.is_error()) {
    zxlogf(ERROR, "Failed to configure canvas: %s", zx_status_get_string(response.error_value()));
    return ZX_ERR_NO_RESOURCES;
  }

  // At this point, we have setup a canvas with the BufferCollection-based VMO. Store the
  // capture information
  //
  // TODO(https://fxbug.dev/42082204): Currently there's no guarantee in the canvas API
  // for the uniqueness of `canvas_idx`, and this driver doesn't check if there
  // is any image with the same canvas index either. We should either make this
  // a formal guarantee in Canvas.Config() API, or perform a check against all
  // imported images to make sure the canvas is unique so that the driver won't
  // overwrite other images.
  import_capture->canvas_idx = result->value()->canvas_idx;
  import_capture->canvas = canvas_.client_end();
  import_capture->image_height = collection_info.settings.image_format_constraints.min_coded_height;
  import_capture->image_width = collection_info.settings.image_format_constraints.min_coded_width;
  // TODO(https://fxbug.dev/42079128): Using pointers as handles impedes portability of
  // the driver. Do not use pointers as handles.
  *out_capture_handle = reinterpret_cast<uint64_t>(import_capture.get());
  imported_captures_.push_back(std::move(import_capture));

  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplStartCapture(uint64_t capture_handle) {
  if (!fully_initialized()) {
    zxlogf(ERROR, "Failed to start capture before initializing the display");
    return ZX_ERR_SHOULD_WAIT;
  }

  fbl::AutoLock lock(&capture_mutex_);
  if (current_capture_target_image_ != nullptr) {
    zxlogf(ERROR, "Failed to start capture while another capture is in progress");
    return ZX_ERR_SHOULD_WAIT;
  }

  // Confirm that the handle was previously imported (hence valid)
  // TODO(https://fxbug.dev/42079128): This requires an enumeration over all the imported
  // capture images for each StartCapture(). We should use hash maps to map
  // handles (which shouldn't be pointers) to ImageInfo instead.
  ImageInfo* info = reinterpret_cast<ImageInfo*>(capture_handle);
  uint8_t canvas_index = info->canvas_idx;
  if (imported_captures_.find_if([canvas_index](const ImageInfo& info) {
        return info.canvas_idx == canvas_index;
      }) == imported_captures_.end()) {
    // invalid handle
    zxlogf(ERROR, "Invalid capture_handle: %" PRIu64, capture_handle);
    return ZX_ERR_NOT_FOUND;
  }

  // TODO(https://fxbug.dev/42082204): A valid canvas index can be zero.
  ZX_DEBUG_ASSERT(info->canvas_idx > 0);
  ZX_DEBUG_ASSERT(info->image_height > 0);
  ZX_DEBUG_ASSERT(info->image_width > 0);

  auto status = vpu_->CaptureInit(info->canvas_idx, info->image_height, info->image_width);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to init capture: %s", zx_status_get_string(status));
    return status;
  }

  status = vpu_->CaptureStart();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start capture: %s", zx_status_get_string(status));
    return status;
  }
  current_capture_target_image_ = info;
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
  fbl::AutoLock lock(&capture_mutex_);
  if (capture_handle == reinterpret_cast<uint64_t>(current_capture_target_image_)) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Find and erase previously imported capture
  // TODO(https://fxbug.dev/42079128): This requires an enumeration over all the imported
  // capture images for each StartCapture(). We should use hash maps to map
  // handles (which shouldn't be pointers) to ImageInfo instead.
  uint8_t canvas_index = reinterpret_cast<ImageInfo*>(capture_handle)->canvas_idx;
  if (imported_captures_.erase_if(
          [canvas_index](const ImageInfo& i) { return i.canvas_idx == canvas_index; }) == nullptr) {
    zxlogf(ERROR, "Tried to release non-existent capture image %d", canvas_index);
    return ZX_ERR_NOT_FOUND;
  }

  return ZX_OK;
}

bool DisplayEngine::DisplayControllerImplIsCaptureCompleted() {
  fbl::AutoLock lock(&capture_mutex_);
  return (current_capture_target_image_ == nullptr);
}

void DisplayEngine::OnVsync(zx::time timestamp) {
  display::ConfigStamp current_config_stamp = display::kInvalidConfigStamp;
  if (fully_initialized()) {
    current_config_stamp = video_input_unit_->GetLastConfigStampApplied();
  }
  fbl::AutoLock lock(&display_mutex_);
  if (dc_intf_.is_valid() && display_attached_) {
    const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(current_config_stamp);
    dc_intf_.OnDisplayVsync(display::ToBanjoDisplayId(display_id_), timestamp.get(),
                            &banjo_config_stamp);
  }
}

void DisplayEngine::OnCaptureComplete() {
  if (!fully_initialized()) {
    zxlogf(WARNING, "Capture interrupt fired before the display was initialized");
    return;
  }

  vpu_->CaptureDone();
  {
    fbl::AutoLock display_lock(&display_mutex_);
    if (dc_intf_.is_valid()) {
      dc_intf_.OnCaptureComplete();
    }
  }
  {
    fbl::AutoLock capture_lock(&capture_mutex_);
    current_capture_target_image_ = nullptr;
  }
}

void DisplayEngine::OnHotPlugStateChange(HotPlugDetectionState current_state) {
  fbl::AutoLock lock(&display_mutex_);

  bool display_added = false;
  added_display_args_t added_display_args;

  bool display_removed = false;
  display::DisplayId removed_display_id;

  if (current_state == HotPlugDetectionState::kDetected && !display_attached_) {
    zxlogf(INFO, "Display is connected");

    display_attached_ = true;

    // When the new display is attached to the display engine, it's not set
    // up with any DisplayMode. This clears the display mode set previously
    // to force a Vout modeset to be performed on the next
    // ApplyConfiguration().
    current_display_timing_ = {};

    vout_->DisplayConnected();
    vout_->PopulateAddedDisplayArgs(&added_display_args, display_id_, kSupportedBanjoPixelFormats);
    display_added = true;
  } else if (current_state == HotPlugDetectionState::kNotDetected && display_attached_) {
    zxlogf(INFO, "Display Disconnected!");
    vout_->DisplayDisconnected();

    display_removed = true;
    removed_display_id = display_id_;
    display_id_++;
    display_attached_ = false;
  }

  if (dc_intf_.is_valid() && (display_removed || display_added)) {
    const size_t added_display_count = display_added ? 1 : 0;
    const uint64_t banjo_removed_display_id = display::ToBanjoDisplayId(removed_display_id);
    const size_t removed_display_count = display_removed ? 1 : 0;

    dc_intf_.OnDisplaysChanged(
        /*added_display_list=*/&added_display_args, added_display_count,
        /*removed_display_list=*/&banjo_removed_display_id, removed_display_count);
  }
}

zx_status_t DisplayEngine::SetupHotplugDisplayDetection() {
  ZX_DEBUG_ASSERT_MSG(!hot_plug_detection_, "HPD already set up");

  zx::result<std::unique_ptr<HotPlugDetection>> hot_plug_detection_result =
      HotPlugDetection::Create(incoming_,
                               fit::bind_member<&DisplayEngine::OnHotPlugStateChange>(this));

  if (hot_plug_detection_result.is_error()) {
    // HotPlugDetection::Create() logged the error.
    return hot_plug_detection_result.status_value();
  }
  hot_plug_detection_ = std::move(hot_plug_detection_result).value();
  return ZX_OK;
}

zx_status_t DisplayEngine::InitializeHdmiVout() {
  ZX_DEBUG_ASSERT(vout_ == nullptr);

  zx::result<std::unique_ptr<Vout>> create_hdmi_vout_result =
      Vout::CreateHdmiVout(incoming_, root_node_.CreateChild("vout"));
  if (!create_hdmi_vout_result.is_ok()) {
    zxlogf(ERROR, "Failed to initialize HDMI Vout device: %s",
           create_hdmi_vout_result.status_string());
    return create_hdmi_vout_result.status_value();
  }
  vout_ = std::move(create_hdmi_vout_result).value();

  return ZX_OK;
}

zx_status_t DisplayEngine::InitializeMipiDsiVout(display_panel_t panel_info) {
  ZX_DEBUG_ASSERT(vout_ == nullptr);

  zxlogf(INFO, "Provided Display Info: %" PRIu32 " x %" PRIu32 " with panel type %" PRIu32,
         panel_info.width, panel_info.height, panel_info.panel_type);
  {
    fbl::AutoLock lock(&display_mutex_);
    zx::result<std::unique_ptr<Vout>> create_dsi_vout_result =
        Vout::CreateDsiVout(incoming_, panel_info.panel_type, panel_info.width, panel_info.height,
                            root_node_.CreateChild("vout"));
    if (!create_dsi_vout_result.is_ok()) {
      zxlogf(ERROR, "Failed to initialize DSI Vout device: %s",
             create_dsi_vout_result.status_string());
      return create_dsi_vout_result.status_value();
    }
    vout_ = std::move(create_dsi_vout_result).value();

    display_attached_ = true;
  }

  return ZX_OK;
}

zx_status_t DisplayEngine::InitializeVout() {
  ZX_ASSERT(vout_ == nullptr);

  zx::result<std::unique_ptr<display_panel_t>> metadata_result =
      metadata_getter_.Get<display_panel_t>(DEVICE_METADATA_DISPLAY_PANEL_CONFIG,
                                            component::kDefaultInstance);
  if (metadata_result.is_ok()) {
    display_panel_t panel_info = *std::move(metadata_result).value();
    return InitializeMipiDsiVout(panel_info);
  }

  if (metadata_result.status_value() == ZX_ERR_NOT_FOUND) {
    return InitializeHdmiVout();
  }

  zxlogf(ERROR, "Failed to get display panel metadata: %s", metadata_result.status_string());
  return metadata_result.status_value();
}

zx_status_t DisplayEngine::GetCommonProtocolsAndResources() {
  ZX_ASSERT(!pdev_.is_valid());
  ZX_ASSERT(!sysmem_.is_valid());
  ZX_ASSERT(!canvas_.is_valid());
  ZX_ASSERT(!bti_.is_valid());

  static constexpr char kPdevFragmentName[] = "pdev";
  zx::result<fidl::ClientEnd<fuchsia_hardware_platform_device::Device>> pdev_result =
      incoming_.Connect<fuchsia_hardware_platform_device::Service::Device>(kPdevFragmentName);
  if (pdev_result.is_error()) {
    zxlogf(ERROR, "Failed to get the pdev client: %s", pdev_result.status_string());
    return pdev_result.status_value();
  }

  pdev_ = fidl::WireSyncClient(std::move(pdev_result).value());
  if (!pdev_.is_valid()) {
    zxlogf(ERROR, "Failed to get a valid platform device client");
    return ZX_ERR_INTERNAL;
  }

  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> sysmem_client_result =
      incoming_.Connect<fuchsia_hardware_sysmem::Service::AllocatorV1>("sysmem");
  if (sysmem_client_result.is_error()) {
    zxlogf(ERROR, "Failed to get sysmem protocol: %s", sysmem_client_result.status_string());
    return sysmem_client_result.status_value();
  }
  sysmem_.Bind(std::move(sysmem_client_result.value()));

  zx::result<fidl::ClientEnd<fuchsia_hardware_amlogiccanvas::Device>> canvas_client_result =
      incoming_.Connect<fuchsia_hardware_amlogiccanvas::Service::Device>("canvas");
  if (canvas_client_result.is_error()) {
    zxlogf(ERROR, "Failed to get Amlogic canvas protocol: %s",
           canvas_client_result.status_string());
    return canvas_client_result.status_value();
  }
  canvas_.Bind(std::move(canvas_client_result.value()));

  zx::result<zx::bti> bti_result = GetBti(BtiResourceIndex::kDma, pdev_.client_end());
  if (bti_result.is_error()) {
    return bti_result.error_value();
  }
  bti_ = std::move(bti_result).value();

  return ZX_OK;
}

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

zx_status_t DisplayEngine::InitializeSysmemAllocator() {
  ZX_ASSERT(sysmem_.is_valid());
  const zx_koid_t current_process_koid = GetKoid(zx_process_self());
  const std::string debug_name = fxl::StringPrintf("amlogic-display[%lu]", current_process_koid);
  fidl::OneWayStatus set_debug_status =
      sysmem_->SetDebugClientInfo(fidl::StringView::FromExternal(debug_name), current_process_koid);
  if (!set_debug_status.ok()) {
    zxlogf(ERROR, "Failed to set sysmem allocator debug info: %s",
           set_debug_status.status_string());
    return set_debug_status.status();
  }
  return ZX_OK;
}

zx_status_t DisplayEngine::Initialize() {
  SetFormatSupportCheck(
      [](fuchsia_images2::wire::PixelFormat format) { return IsFormatSupported(format); });

  // Set up inspect first, since other components may add inspect children
  // during initialization.
  root_node_ = inspector_.GetRoot().CreateChild("amlogic-display");

  zx_status_t status = GetCommonProtocolsAndResources();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get common protocols resources from parent devices: %s",
           zx_status_get_string(status));
    return status;
  }

  status = InitializeVout();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initalize Vout: %s", zx_status_get_string(status));
    return status;
  }

  video_input_unit_node_ = root_node_.CreateChild("video_input_unit");
  zx::result<std::unique_ptr<VideoInputUnit>> video_input_unit_create_result =
      VideoInputUnit::Create(pdev_.client_end(), &video_input_unit_node_);
  if (video_input_unit_create_result.is_error()) {
    zxlogf(ERROR, "Failed to create VideoInputUnit instance: %s",
           video_input_unit_create_result.status_string());
    return video_input_unit_create_result.status_value();
  }
  video_input_unit_ = std::move(video_input_unit_create_result).value();

  zx::result<std::unique_ptr<Vpu>> vpu_result = Vpu::Create(pdev_.client_end());
  if (vpu_result.is_error()) {
    zxlogf(ERROR, "Failed to initialize VPU object: %s", vpu_result.status_string());
    return vpu_result.status_value();
  }
  vpu_ = std::move(vpu_result).value();

  // If the display engine was previously owned by a different driver, we
  // attempt to complete a seamless takeover. If we previously owned the
  // hardware, our driver must have been unloaded and reloaded.
  // We currently do a full hardware reset in that case.
  const bool performs_full_hardware_reset =
      IsFullHardwareResetRequired(pdev_.client_end()) || !vpu_->CheckAndClaimHardwareOwnership();
  if (performs_full_hardware_reset) {
    fbl::AutoLock lock(&display_mutex_);
    zx::result<> reset_result = ResetDisplayEngine();
    if (!reset_result.is_ok()) {
      zxlogf(ERROR, "Failed to reset the display engine: %s", reset_result.status_string());
      return reset_result.status_value();
    }
  } else {
    // It's possible that the AFBC engine is not yet turned on by the
    // previous driver when the driver takes it over so we should ensure it's
    // enabled.
    //
    // TODO(https://fxbug.dev/42082920): Instead of enabling it ad-hoc here, make
    // `Vpu::PowerOn()` idempotent and always call it when initializing the
    // driver.
    vpu_->AfbcPower(true);
  }

  status = InitializeSysmemAllocator();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize sysmem allocator: %s", zx_status_get_string(status));
    return status;
  }

  {
    zx::result<std::unique_ptr<VsyncReceiver>> vsync_receiver_result = VsyncReceiver::Create(
        bus_device_, pdev_.client_end(), fit::bind_member<&DisplayEngine::OnVsync>(this));
    if (vsync_receiver_result.is_error()) {
      // Create() already logged the error.
      return vsync_receiver_result.error_value();
    }
    vsync_receiver_ = std::move(vsync_receiver_result).value();
  }

  {
    zx::result<std::unique_ptr<Capture>> capture_result = Capture::Create(
        pdev_.client_end(), fit::bind_member<&DisplayEngine::OnCaptureComplete>(this));
    if (capture_result.is_error()) {
      // Create() already logged the error.
      return capture_result.error_value();
    }
    capture_ = std::move(capture_result).value();
  }

  if (vout_->supports_hpd()) {
    if (zx_status_t status = SetupHotplugDisplayDetection(); status != ZX_OK) {
      zxlogf(ERROR, "Failed to set up hotplug display: %s", zx_status_get_string(status));
      return status;
    }
  }

  set_fully_initialized();
  return ZX_OK;
}

DisplayEngine::DisplayEngine(zx_device_t* bus_device, display::Namespace* incoming,
                             display::MetadataGetter* metadata_getter)
    : bus_device_(bus_device), incoming_(*incoming), metadata_getter_(*metadata_getter) {
  ZX_DEBUG_ASSERT(incoming != nullptr);
  ZX_DEBUG_ASSERT(metadata_getter != nullptr);
}
DisplayEngine::~DisplayEngine() {}

// static
zx::result<std::unique_ptr<DisplayEngine>> DisplayEngine::Create(
    zx_device_t* parent, display::Namespace* incoming, display::MetadataGetter* metadata_getter) {
  fbl::AllocChecker alloc_checker;
  auto display_engine =
      fbl::make_unique_checked<DisplayEngine>(&alloc_checker, parent, incoming, metadata_getter);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayEngine");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  const zx_status_t status = display_engine->Initialize();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize DisplayEngine instance: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(display_engine));
}

}  // namespace amlogic_display
