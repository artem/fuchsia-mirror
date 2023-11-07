// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <zircon/syscalls/smc.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/rpmb/cpp/bind.h>

#include "nelson.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "src/devices/lib/fidl-metadata/tee.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

// The Nelson Secure OS memory region is defined within the bootloader image.
// The ZBI provided to the kernel must mark this memory space as reserved.
// The OP-TEE driver will query OP-TEE for the exact sub-range of this memory
// space to be used by the driver.
#define NELSON_SECURE_OS_BASE 0x05300000
#define NELSON_SECURE_OS_LENGTH 0x02000000

#define NELSON_OPTEE_DEFAULT_THREAD_COUNT 2

using tee_thread_config_t = fidl_metadata::tee::CustomThreadConfig;

static const std::vector<fpbus::Mmio> nelson_tee_mmios{
    {{
        .base = NELSON_SECURE_OS_BASE,
        .length = NELSON_SECURE_OS_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> nelson_tee_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_TEE,
    }},
};

static const std::vector<fpbus::Smc> nelson_tee_smcs{
    {{
        .service_call_num_base = ARM_SMC_SERVICE_CALL_NUM_TRUSTED_OS_BASE,
        .count = ARM_SMC_SERVICE_CALL_NUM_TRUSTED_OS_LENGTH,
        .exclusive = false,
    }},
};

static tee_thread_config_t tee_thread_cfg[] = {
    {.role = "fuchsia.tee.media",
     .count = 1,
     .trusted_apps = {
         {0x9a04f079,
          0x9840,
          0x4286,
          {0xab, 0x92, 0xe6, 0x5b, 0xe0, 0x88, 0x5f, 0x95}},  // playready
         {0xe043cde0, 0x61d0, 0x11e5, {0x9c, 0x26, 0x00, 0x02, 0xa5, 0xd5, 0xc5, 0x1b}}  // widevine
     }}};

const std::vector<fdf::BindRule> kRpmbRules = std::vector{fdf::MakeAcceptBindRule(
    bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_rpmb::BIND_FIDL_PROTOCOL_DEVICE)};

const std::vector<fdf::NodeProperty> kRpmbProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_rpmb::BIND_FIDL_PROTOCOL_DEVICE)};

const std::vector<fdf::ParentSpec> kTeeCompositeParents = {{kRpmbRules, kRpmbProperties}};

zx_status_t Nelson::TeeInit() {
  std::vector<fpbus::Metadata> metadata;

  fpbus::Node dev;
  dev.name() = "tee";
  dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_OPTEE;
  dev.mmio() = nelson_tee_mmios;
  dev.bti() = nelson_tee_btis;
  dev.smc() = nelson_tee_smcs;

  auto optee_status = fidl_metadata::tee::TeeMetadataToFidl(
      NELSON_OPTEE_DEFAULT_THREAD_COUNT,
      cpp20::span<const tee_thread_config_t>(tee_thread_cfg, std::size(tee_thread_cfg)));
  if (optee_status.is_error()) {
    zxlogf(ERROR, "%s: failed to fidl encode optee thread config: %d", __func__,
           optee_status.error_value());
    return optee_status.error_value();
  }

  auto& data = optee_status.value();

  metadata.emplace_back([&]() {
    fpbus::Metadata ret;
    ret.type() = DEVICE_METADATA_TEE_THREAD_CONFIG;
    ret.data() = std::move(data);
    return ret;
  }());

  dev.metadata() = std::move(metadata);

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('TEE_');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "tee", .parents = kTeeCompositeParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Tee(dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Tee(dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace nelson
