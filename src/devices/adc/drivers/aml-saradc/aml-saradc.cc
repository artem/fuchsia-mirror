// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/adc/drivers/aml-saradc/aml-saradc.h"

#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <fbl/auto_lock.h>

#include "src/devices/adc/drivers/aml-saradc/registers.h"

namespace aml_saradc {

void AmlSaradcDevice::SetClock(uint32_t src, uint32_t div) {
  ao_mmio_.ModifyBits32(src << AO_SAR_CLK_SRC_POS, AO_SAR_CLK_SRC_MASK, AO_SAR_CLK_OFFS);
  ao_mmio_.ModifyBits32(div << AO_SAR_CLK_DIV_POS, AO_SAR_CLK_DIV_MASK, AO_SAR_CLK_OFFS);
}

void AmlSaradcDevice::Shutdown() {
  Stop();
  Enable(false);
}

void AmlSaradcDevice::Stop() {
  // Stop Conversion
  adc_mmio_.SetBits32(REG0_SAMPLING_STOP_MASK, AO_SAR_ADC_REG0_OFFS);
  // Disable Sampling
  adc_mmio_.ClearBits32(REG0_SAMPLING_ENABLE_MASK, AO_SAR_ADC_REG0_OFFS);
}

void AmlSaradcDevice::ClkEna(bool ena) {
  if (ena) {
    ao_mmio_.SetBits32(AO_SAR_CLK_ENA_MASK, AO_SAR_CLK_OFFS);
  } else {
    ao_mmio_.ClearBits32(AO_SAR_CLK_ENA_MASK, AO_SAR_CLK_OFFS);
  }
}

void AmlSaradcDevice::Enable(bool ena) {
  if (ena) {
    // Enable bandgap reference
    adc_mmio_.SetBits32(REG11_TS_VBG_EN_MASK, AO_SAR_ADC_REG11_OFFS);
    // Set common mode vref
    adc_mmio_.ClearBits32(REG11_RSV6_MASK, AO_SAR_ADC_REG11_OFFS);
    // Select bandgap as reference
    adc_mmio_.ClearBits32(REG11_RSV5_MASK, AO_SAR_ADC_REG11_OFFS);
    // Enable IRQ
    adc_mmio_.SetBits32(REG0_FIFO_IRQ_EN_MASK, AO_SAR_ADC_REG0_OFFS);
    // Enable the ADC
    adc_mmio_.SetBits32(REG3_ADC_EN_MASK, AO_SAR_ADC_REG3_OFFS);
    zx_nanosleep(zx_deadline_after(ZX_USEC(5)));
    // Enable clock source
    ClkEna(true);
  } else {
    // Disable IRQ
    adc_mmio_.ClearBits32(REG0_FIFO_IRQ_EN_MASK, AO_SAR_ADC_REG0_OFFS);
    // Disable clock source
    ClkEna(false);
    // Disable the ADC
    adc_mmio_.ClearBits32(REG3_ADC_EN_MASK, AO_SAR_ADC_REG3_OFFS);
  }
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
}

void AmlSaradcDevice::GetSample(GetSampleRequest& request, GetSampleCompleter::Sync& completer) {
  auto channel = request.channel_id();
  if (channel >= kMaxChannels) {
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  // Slow clock for conversion
  ClkEna(false);
  SetClock(CLK_SRC_OSCIN, 160);
  ClkEna(true);

  // Select channel
  adc_mmio_.Write32(channel, AO_SAR_ADC_CHAN_LIST_OFFS);

  // Set analog mux (active and idle) to requested channel
  adc_mmio_.Write32(0x000c000c | (channel << 23) | (channel << 7), AO_SAR_ADC_DETECT_IDLE_SW_OFFS);

  // Enable sampling
  adc_mmio_.SetBits32(REG0_SAMPLING_ENABLE_MASK, AO_SAR_ADC_REG0_OFFS);

  // Start sampling
  adc_mmio_.SetBits32(REG0_SAMPLING_START_MASK, AO_SAR_ADC_REG0_OFFS);

  fit::result<uint32_t, zx_status_t> result = fit::error(ZX_ERR_UNAVAILABLE);
  auto status = irq_.wait(nullptr);
  if (status == ZX_OK) {
    uint32_t value = adc_mmio_.Read32(AO_SAR_ADC_FIFO_RD_OFFS);
    result = fit::ok((value >> 2) & 0x3ff);
  } else {
    result = fit::error(status);
  }

  Stop();
  ClkEna(false);
  SetClock(CLK_SRC_OSCIN, 20);
  ClkEna(true);

  completer.Reply(result);
}

void AmlSaradcDevice::HwInit() {
  adc_mmio_.Write32(0x84004040, AO_SAR_ADC_REG0_OFFS);
  // Set IRQ trigger to one sample.
  adc_mmio_.ModifyBits32(1 << REG0_FIFO_CNT_IRQ_POS, REG0_FIFO_CNT_IRQ_MASK, AO_SAR_ADC_REG0_OFFS);

  // Set channel list to only channel zero
  adc_mmio_.Write32(0x00000000, AO_SAR_ADC_CHAN_LIST_OFFS);

  // Disable averaging modes
  adc_mmio_.Write32(0x00000000, AO_SAR_ADC_AVG_CNTL_OFFS);

  adc_mmio_.Write32(0x9388000a, AO_SAR_ADC_REG3_OFFS);

  adc_mmio_.Write32(0x010a000a, AO_SAR_ADC_DELAY_OFFS);

  adc_mmio_.Write32(0x03eb1a0c, AO_SAR_ADC_AUX_SW_OFFS);

  adc_mmio_.Write32(0x008c000c, AO_SAR_ADC_CHAN_10_SW_OFFS);

  adc_mmio_.Write32(0x000c000c, AO_SAR_ADC_DETECT_IDLE_SW_OFFS);
  // Disable ring counter (not used on g12)
  adc_mmio_.SetBits32((1 << 27), AO_SAR_ADC_REG3_OFFS);

  adc_mmio_.SetBits32(REG11_RSV1_MASK, AO_SAR_ADC_REG11_OFFS);

  adc_mmio_.Write32(0x00002000, AO_SAR_ADC_REG13_OFFS);

  // Select 24MHz oscillator / 20 = 1.2MHz
  SetClock(CLK_SRC_OSCIN, 20);
  Enable(true);
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
}

zx::result<> AmlSaradc::CreateNode() {
  fidl::Arena arena;
  auto offers = compat_server_.CreateOffers2(arena);
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_adcimpl::Service>(arena, component::kDefaultInstance));

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .offers2(arena, std::move(offers))
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                controller_endpoints.status_string());

  fidl::WireResult result =
      fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints->client));

  return zx::ok();
}

zx::result<> AmlSaradc::Start() {
  // Initialize our compat server.
  {
    zx::result result =
        compat_server_.Initialize(incoming(), outgoing(), node_name(), kDeviceName,
                                  compat::ForwardMetadata::Some({DEVICE_METADATA_ADC}));
    if (result.is_error()) {
      return result.take_error();
    }
  }

  // Map hardware resources from pdev.
  std::optional<fdf::MmioBuffer> adc_mmio, ao_mmio;
  zx::interrupt irq;
  {
    zx::result result = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open pdev service: %s", result.status_string());
      return result.take_error();
    }
    auto pdev = ddk::PDevFidl(std::move(result.value()));
    if (!pdev.is_valid()) {
      FDF_LOG(ERROR, "Failed to get pdev");
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto status = pdev.MapMmio(0, &adc_mmio);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to map mmio 0");
      return zx::error(status);
    }
    status = pdev.MapMmio(1, &ao_mmio);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to map mmio 1");
      return zx::error(status);
    }

    status = pdev.GetInterrupt(0, &irq);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to get interrupt");
      return zx::error(status);
    }
  }

  device_ =
      std::make_unique<AmlSaradcDevice>(std::move(*adc_mmio), std::move(*ao_mmio), std::move(irq));
  device_->HwInit();
  auto result = outgoing()->AddService<fuchsia_hardware_adcimpl::Service>(
      fuchsia_hardware_adcimpl::Service::InstanceHandler({
          .device = bindings_.CreateHandler(device_.get(), fdf::Dispatcher::GetCurrent()->get(),
                                            fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
    return result.take_error();
  }

  if (zx::result result = CreateNode(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to create node %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

void AmlSaradc::PrepareStop(fdf::PrepareStopCompleter completer) {
  device_->Shutdown();
  completer(zx::ok());
}

}  // namespace aml_saradc

FUCHSIA_DRIVER_EXPORT(aml_saradc::AmlSaradc);
