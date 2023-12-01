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
#include <string.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/codec/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/ti/platform/cpp/bind.h>
#include <ddktl/metadata/audio.h>
#include <soc/aml-common/aml-audio.h>
#include <soc/aml-meson/g12b-clk.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>
#include <ti/ti-audio.h>

#include "sherlock-gpios.h"
#include "sherlock.h"
#include "src/devices/board/drivers/sherlock/sherlock-tweeter-left-bind.h"
#include "src/devices/board/drivers/sherlock/sherlock-tweeter-right-bind.h"
#include "src/devices/board/drivers/sherlock/sherlock-woofer-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

// Enables BT PCM audio.
#define ENABLE_BT

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t AddTas5720Device(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus,
                             const char* device_name, uint32_t device_instance_id,
                             cpp20::span<const device_fragment_t> device_fragments,
                             const uint32_t* instance_count) {
  fpbus::Node dev;
  dev.name() = device_name;
  dev.pid() = PDEV_PID_GENERIC;
  dev.vid() = PDEV_VID_TI;
  dev.did() = PDEV_DID_TI_TAS5720;
  dev.instance_id() = device_instance_id;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(instance_count),
              reinterpret_cast<const uint8_t*>(instance_count) + sizeof(*instance_count)),
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('5720');
  auto fragments = platform_bus_composite::MakeFidlFragment(fidl_arena, device_fragments.data(),
                                                            device_fragments.size());
  fdf::WireUnownedResult result =
      pbus.buffer(arena)->AddComposite(fidl::ToWire(fidl_arena, dev), fragments, "i2c");
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddComposite request: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add composite: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t Sherlock::AudioInit() {
  uint8_t tdm_instance_id = 1;
  static const std::vector<fpbus::Mmio> audio_mmios{
      {{
          .base = T931_EE_AUDIO_BASE,
          .length = T931_EE_AUDIO_LENGTH,
      }},
      {{
          .base = T931_GPIO_BASE,
          .length = T931_GPIO_LENGTH,
      }},
      {{
          .base = T931_GPIO_AO_BASE,
          .length = T931_GPIO_AO_LENGTH,
      }},
  };

  static const std::vector<fpbus::Bti> tdm_btis{
      {{
          .iommu_index = 0,
          .bti_id = BTI_AUDIO_OUT,
      }},
  };
  static const std::vector<fpbus::Irq> frddr_b_irqs{
      {{
          .irq = T931_AUDIO_FRDDR_B,
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
      }},
  };
  static const std::vector<fpbus::Irq> toddr_b_irqs{
      {{
          .irq = T931_AUDIO_TODDR_B,
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('AUDI');
  auto result = pbus_.buffer(arena)->GetBoardInfo();
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Audio(tdm_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Audio(tdm_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  auto& board_info = result->value()->info;

  if (board_info.board_revision < BOARD_REV_EVT1 && board_info.board_revision != BOARD_REV_B72) {
    // For audio we don't support boards revision lower than EVT with the exception of the B72
    // board.
    zxlogf(WARNING, "%s: Board revision unsupported, skipping audio initialization.", __FILE__);
    return ZX_ERR_NOT_SUPPORTED;
  }

  const char* product_name = "sherlock";
  constexpr size_t device_name_max_length = 32;

  std::vector<fdf::ParentSpec> sherlock_tdm_i2s_parents;
  sherlock_tdm_i2s_parents.reserve(5);

  const auto gpio_init_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const auto gpio_init_props = std::vector{
      fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const auto gpio_init_parent = std::vector{
      fdf::ParentSpec{{gpio_init_rules, gpio_init_props}},
  };

  sherlock_tdm_i2s_parents.push_back(gpio_init_parent[0]);

  // Add a spec for the enable audio GPIO pin.
  auto enable_audio_gpio_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_SOC_AUDIO_EN)),
  };
  auto enable_audio_gpio_props = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_SOC_AUDIO_ENABLE),
  };
  sherlock_tdm_i2s_parents.push_back(fdf::ParentSpec{{
      .bind_rules = enable_audio_gpio_rules,
      .properties = enable_audio_gpio_props,
  }});

  // Add a spec for the woofer, left tweeter, and right tweeter in that order.
  for (size_t i = 0; i < 3; i++) {
    auto codec_rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                                bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
        fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                                bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI),
        fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                                bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TAS5720),
        fdf::MakeAcceptBindRule(bind_fuchsia::CODEC_INSTANCE, static_cast<uint32_t>(i + 1)),
    };
    auto codec_props = std::vector{
        fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                          bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
        fdf::MakeProperty(bind_fuchsia::CODEC_INSTANCE, static_cast<uint32_t>(i + 1)),
    };
    sherlock_tdm_i2s_parents.push_back(fdf::ParentSpec{{
        .bind_rules = codec_rules,
        .properties = codec_props,
    }});
  }

  zx_status_t status = clk_impl_.Disable(g12b_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Disable(CLK_HIFI_PLL) failed, st = %d", __func__, status);
    return status;
  }

  status = clk_impl_.SetRate(g12b_clk::CLK_HIFI_PLL, T931_HIFI_PLL_RATE);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: SetRate(CLK_HIFI_PLL) failed, st = %d", __func__, status);
    return status;
  }

  status = clk_impl_.Enable(g12b_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Enable(CLK_HIFI_PLL) failed, st = %d", __func__, status);
    return status;
  }

  // TDM pin configuration.
  gpio_init_steps_.push_back({T931_GPIOZ(7), GpioSetAltFunction(T931_GPIOZ_7_TDMC_SCLK_FN)});
  gpio_init_steps_.push_back({T931_GPIOZ(6), GpioSetAltFunction(T931_GPIOZ_6_TDMC_FS_FN)});
  gpio_init_steps_.push_back({T931_GPIOZ(2), GpioSetAltFunction(T931_GPIOZ_2_TDMC_D0_FN)});
  constexpr uint64_t ua = 3000;
  gpio_init_steps_.push_back({T931_GPIOZ(7), GpioSetDriveStrength(ua)});
  gpio_init_steps_.push_back({T931_GPIOZ(6), GpioSetDriveStrength(ua)});
  gpio_init_steps_.push_back({T931_GPIOZ(2), GpioSetDriveStrength(ua)});
  gpio_init_steps_.push_back({T931_GPIOZ(3), GpioSetAltFunction(T931_GPIOZ_3_TDMC_D1_FN)});
  gpio_init_steps_.push_back({T931_GPIOZ(3), GpioSetDriveStrength(ua)});

  gpio_init_steps_.push_back({T931_GPIOAO(9), GpioSetAltFunction(T931_GPIOAO_9_MCLK_FN)});
  gpio_init_steps_.push_back({T931_GPIOAO(9), GpioSetDriveStrength(ua)});

#ifdef ENABLE_BT
  // PCM pin assignments.
  gpio_init_steps_.push_back({T931_GPIOX(8), GpioSetAltFunction(T931_GPIOX_8_TDMA_DIN1_FN)});
  gpio_init_steps_.push_back({T931_GPIOX(9), GpioSetAltFunction(T931_GPIOX_9_TDMA_D0_FN)});
  gpio_init_steps_.push_back({T931_GPIOX(10), GpioSetAltFunction(T931_GPIOX_10_TDMA_FS_FN)});
  gpio_init_steps_.push_back({T931_GPIOX(11), GpioSetAltFunction(T931_GPIOX_11_TDMA_SCLK_FN)});
  gpio_init_steps_.push_back({T931_GPIOX(9), GpioSetDriveStrength(ua)});
  gpio_init_steps_.push_back({T931_GPIOX(10), GpioSetDriveStrength(ua)});
  gpio_init_steps_.push_back({T931_GPIOX(11), GpioSetDriveStrength(ua)});
#endif

  // PDM pin assignments.
  gpio_init_steps_.push_back({T931_GPIOA(7), GpioSetAltFunction(T931_GPIOA_7_PDM_DCLK_FN)});
  gpio_init_steps_.push_back({T931_GPIOA(8), GpioSetAltFunction(T931_GPIOA_8_PDM_DIN0_FN)});

  // Add TDM OUT to the codecs.
  {
    gpio_init_steps_.push_back({T931_GPIOH(7), GpioConfigOut(1)});  // SOC_AUDIO_EN.

    constexpr uint32_t woofer_instance_count = 1;
    status = AddTas5720Device(pbus_, "audio-tas5720-woofer", woofer_instance_count,
                              audio_tas5720_woofer_fragments, &woofer_instance_count);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to add woofer composite device: %s", zx_status_get_string(status));
      return status;
    }

    constexpr uint32_t left_tweeter_instance_count = 2;
    status = AddTas5720Device(pbus_, "audio-tas5720-left-tweeter", left_tweeter_instance_count,
                              audio_tas5720_tweeter_left_fragments, &left_tweeter_instance_count);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to add left tweeter composite device: %s",
             zx_status_get_string(status));
      return status;
    }

    constexpr uint32_t right_tweeter_instance_count = 3;
    status = AddTas5720Device(pbus_, "audio-tas5720-right-tweeter", right_tweeter_instance_count,
                              audio_tas5720_tweeter_right_fragments, &right_tweeter_instance_count);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to add right tweeter composite device: %s",
             zx_status_get_string(status));
      return status;
    }
  }
  metadata::AmlConfig metadata = {};
  snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
  strncpy(metadata.product_name, product_name, sizeof(metadata.product_name));

  metadata.is_input = false;
  // Compatible clocks with other TDM drivers.
  metadata.mClockDivFactor = 10;
  metadata.sClockDivFactor = 25;
  metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_SPEAKERS;
  metadata.bus = metadata::AmlBus::TDM_C;
  metadata.version = metadata::AmlVersion::kS905D2G;  // Also works with T931G.
  metadata.dai.type = metadata::DaiType::I2s;
  metadata.dai.bits_per_sample = 16;
  metadata.dai.bits_per_slot = 32;
  // Ranges could be wider, but only using them crossed-over at 1'200 Hz in this product.
  metadata.ring_buffer.frequency_ranges[0].min_frequency = 20;
  metadata.ring_buffer.frequency_ranges[0].max_frequency = 1'600;
  metadata.ring_buffer.frequency_ranges[1].min_frequency = 20;
  metadata.ring_buffer.frequency_ranges[1].max_frequency = 1'600;
  metadata.ring_buffer.frequency_ranges[2].min_frequency = 1'000;
  metadata.ring_buffer.frequency_ranges[2].max_frequency = 40'000;
  metadata.ring_buffer.frequency_ranges[3].min_frequency = 1'000;
  metadata.ring_buffer.frequency_ranges[3].max_frequency = 40'000;
  metadata.codecs.number_of_codecs = 3;
  metadata.codecs.types[0] = metadata::CodecType::Tas5720;
  metadata.codecs.types[1] = metadata::CodecType::Tas5720;
  metadata.codecs.types[2] = metadata::CodecType::Tas5720;
  // This driver advertises 4 channels.
  // The samples in the first channel are unused (can be zero).
  // The samples in the second channel are used for the woofer and are expected to have a mix of
  // both left and right channel from stereo audio.
  // The samples in the third channel are expected to come from the left channel of stereo audio
  // and are used for the left tweeter.
  // The samples in the fourth channel are expected to come from the right channel of stereo audio
  // and are used for the right tweeter.
  metadata.ring_buffer.number_of_channels = 4;
  metadata.swaps = 0x0123;
  metadata.lanes_enable_mask[0] = 3;
  metadata.lanes_enable_mask[1] = 3;
#ifndef FACTORY_BUILD
  // Delta between woofers and tweeters of 6.4dB.
  metadata.codecs.delta_gains[0] = 0.f;
  metadata.codecs.delta_gains[1] = -6.4f;
  metadata.codecs.delta_gains[2] = -6.4f;
#endif                                               // FACTORY_BUILD
  metadata.codecs.channels_to_use_bitmask[0] = 0x2;  // Woofer uses DAI right I2S channel.
  metadata.codecs.channels_to_use_bitmask[1] = 0x1;  // L tweeter uses DAI left I2S channel.
  metadata.codecs.channels_to_use_bitmask[2] = 0x2;  // R tweeter uses DAI right I2S channel.
  // The woofer samples are expected in the second position out of four channels.
  // In a 4-bit bitmask, counting from least-significant bit, this is index 1: value 2^1 = 2.
  metadata.codecs.ring_buffer_channels_to_use_bitmask[0] = 0x2;  // Woofer uses index 1.
  metadata.codecs.ring_buffer_channels_to_use_bitmask[1] = 0x4;  // L tweeter uses index 2.
  metadata.codecs.ring_buffer_channels_to_use_bitmask[2] = 0x8;  // R tweeter uses index 3.
  std::vector<fpbus::Metadata> tdm_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data =
              std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&metadata),
                                   reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
      }},
  };

  fpbus::Node tdm_dev;
  char name[device_name_max_length];
  snprintf(name, sizeof(name), "%s-i2s-audio-out", product_name);
  tdm_dev.name() = name;
  tdm_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  tdm_dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_T931;
  tdm_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_TDM;
  tdm_dev.instance_id() = tdm_instance_id++;
  tdm_dev.mmio() = audio_mmios;
  tdm_dev.bti() = tdm_btis;
  tdm_dev.irq() = frddr_b_irqs;
  tdm_dev.metadata() = tdm_metadata;

  {
    fidl::Arena<> fidl_arena;
    fdf::Arena arena('AUDI');
    auto sherlock_tdm_i2s_spec = fdf::CompositeNodeSpec{{
        .name = "aml_tdm",
        .parents = sherlock_tdm_i2s_parents,
    }};
    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, tdm_dev), fidl::ToWire(fidl_arena, sherlock_tdm_i2s_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Audio(tdm_dev) request failed: %s",
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Audio(tdm_dev) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

#ifdef ENABLE_BT
  // Add TDM OUT for BT.
  {
    static const std::vector<fpbus::Bti> pcm_out_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_OUT,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    strncpy(metadata.product_name, product_name, sizeof(metadata.product_name));

    metadata.is_input = false;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kS905D2G;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 1;
    metadata.dai.number_of_channels = 1;
    metadata.lanes_enable_mask[0] = 1;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    fpbus::Node tdm_dev;
    char tdm_name[device_name_max_length];
    snprintf(tdm_name, sizeof(tdm_name), "%s-pcm-dai-out", product_name);
    tdm_dev.name() = tdm_name;
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_T931;
    tdm_dev.did() = PDEV_DID_AMLOGIC_DAI_OUT;
    tdm_dev.instance_id() = tdm_instance_id++;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_out_btis;
    tdm_dev.metadata() = tdm_metadata;

    {
      auto tdm_spec = fdf::CompositeNodeSpec{{
          "aml_tdm_dai_out",
          gpio_init_parent,
      }};
      auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tdm_dev),
                                                              fidl::ToWire(fidl_arena, tdm_spec));
      if (!result.ok()) {
        zxlogf(ERROR, "AddCompositeNodeSpec(tdm_dev) request failed: %s",
               result.FormatDescription().data());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "AddCompositeNodeSpec(tdm_dev) failed: %s",
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
  }
#endif

  // Input device.
  {
    metadata::AmlPdmConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "sherlock");
    metadata.number_of_channels = 2;
    metadata.version = metadata::AmlVersion::kS905D2G;
    metadata.sysClockDivFactor = 4;
    metadata.dClockDivFactor = 250;
    std::vector<fpbus::Metadata> pdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    static const std::vector<fpbus::Mmio> pdm_mmios{
        {{
            .base = T931_EE_PDM_BASE,
            .length = T931_EE_PDM_LENGTH,
        }},
        {{
            .base = T931_EE_AUDIO_BASE,
            .length = T931_EE_AUDIO_LENGTH,
        }},
    };

    static const std::vector<fpbus::Bti> pdm_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_IN,
        }},
    };

    fpbus::Node dev_in;
    char pdm_name[device_name_max_length];
    snprintf(pdm_name, sizeof(pdm_name), "%s-pdm-audio-in", product_name);
    dev_in.name() = pdm_name;
    dev_in.vid() = PDEV_VID_AMLOGIC;
    dev_in.pid() = PDEV_PID_AMLOGIC_T931;
    dev_in.did() = PDEV_DID_AMLOGIC_PDM;
    dev_in.mmio() = pdm_mmios;
    dev_in.bti() = pdm_btis;
    dev_in.irq() = toddr_b_irqs;
    dev_in.metadata() = pdm_metadata;

    {
      auto pdm_spec = fdf::CompositeNodeSpec{{
          "aml_pdm",
          gpio_init_parent,
      }};
      auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, dev_in),
                                                              fidl::ToWire(fidl_arena, pdm_spec));
      if (!result.ok()) {
        zxlogf(ERROR, "AddCompositeNodeSpec Audio(dev_in) request failed: %s",
               result.FormatDescription().data());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "AddCompositeNodeSpec Audio(dev_in) failed: %s",
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
  }

#ifdef ENABLE_BT
  // Add TDM IN for BT.
  {
    static const std::vector<fpbus::Bti> pcm_in_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_IN,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    strncpy(metadata.product_name, product_name, sizeof(metadata.product_name));

    metadata.is_input = true;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kS905D2G;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 1;
    metadata.dai.number_of_channels = 1;
    metadata.swaps = 0x0200;
    metadata.lanes_enable_mask[1] = 1;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };
    fpbus::Node tdm_dev;
    char name[device_name_max_length];
    snprintf(name, sizeof(name), "%s-pcm-dai-in", product_name);
    tdm_dev.name() = name;
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_T931;
    tdm_dev.did() = PDEV_DID_AMLOGIC_DAI_IN;
    tdm_dev.instance_id() = tdm_instance_id++;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_in_btis;
    tdm_dev.metadata() = tdm_metadata;

    {
      auto tdm_spec = fdf::CompositeNodeSpec{{
          "aml_tdm_dai_in",
          gpio_init_parent,
      }};
      auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tdm_dev),
                                                              fidl::ToWire(fidl_arena, tdm_spec));
      if (!result.ok()) {
        zxlogf(ERROR, "AddCompositeNodeSpec(tdm_dev) request failed: %s",
               result.FormatDescription().data());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "AddCompositeNodeSpec(tdm_dev) failed: %s",
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
  }
#endif
  return ZX_OK;
}

}  // namespace sherlock
