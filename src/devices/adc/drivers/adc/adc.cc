// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/adc/drivers/adc/adc.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/adc/cpp/bind.h>
#include <bind/fuchsia/hardware/adc/cpp/bind.h>
#include <fbl/alloc_checker.h>

namespace adc {

AdcDevice::AdcDevice(fdf::ClientEnd<fuchsia_hardware_adcimpl::Device> adc_impl, uint32_t channel,
                     std::string_view name, uint8_t resolution, Adc* adc)
    : adc_impl_(std::move(adc_impl)),
      channel_(channel),
      name_(name),
      resolution_(resolution),
      compat_server_(adc->dispatcher(), adc->incoming(), adc->outgoing(), adc->node_name(), name_,
                     std::string(Adc::kDeviceName) + "/") {}

void AdcDevice::GetResolution(GetResolutionCompleter::Sync& completer) {
  completer.Reply(fit::ok(resolution_));
}

void AdcDevice::GetSample(GetSampleCompleter::Sync& completer) {
  fdf::Arena arena('ADC_');
  const auto result = adc_impl_.buffer(arena)->GetSample(channel_);
  if (!result.ok()) {
    completer.Reply(fit::error(result.status()));
    return;
  }
  if (result->is_error()) {
    completer.Reply(fit::error(result->error_value()));
    return;
  }
  completer.Reply(fit::ok(result.value()->value));
}

void AdcDevice::GetNormalizedSample(GetNormalizedSampleCompleter::Sync& completer) {
  fdf::Arena arena('ADC_');
  float sample;
  {
    const auto result = adc_impl_.buffer(arena)->GetSample(channel_);
    if (!result.ok()) {
      completer.Reply(fit::error(result.status()));
      return;
    }
    if (result->is_error()) {
      completer.Reply(fit::error(result->error_value()));
      return;
    }
    sample = static_cast<float>(result->value()->value);
  }

  completer.Reply(fit::ok(sample / static_cast<float>(((1 << resolution_) - 1))));
}

zx::result<std::unique_ptr<AdcDevice>> AdcDevice::Create(
    fdf::ClientEnd<fuchsia_hardware_adcimpl::Device> adc_impl,
    fuchsia_hardware_adcimpl::AdcChannel channel, Adc* adc) {
  uint8_t resolution;
  {
    fdf::Arena arena('ADC_');
    const auto result = fdf::WireCall(adc_impl).buffer(arena)->GetResolution();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to GetResolution %s", result.FormatDescription().c_str());
      return zx::error(ZX_ERR_INTERNAL);
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "Failed to GetResolution %d", result->error_value());
      return zx::error(result->error_value());
    }
    resolution = result->value()->resolution;
  }

  auto dev = std::make_unique<AdcDevice>(std::move(adc_impl), *channel.idx(),
                                         std::move(*channel.name()), resolution, adc);

  // Serve fuchsia_hardware_adc.
  {
    auto result = adc->outgoing()->AddService<fuchsia_hardware_adc::Service>(
        fuchsia_hardware_adc::Service::InstanceHandler({
            .device = dev->bindings_.CreateHandler(
                dev.get(), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                fidl::kIgnoreBindingClosure),
        }),
        dev->name_);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
      return zx::error(result.status_value());
    }
  }

  // Create node.
  fidl::Arena arena;
  auto offers = dev->compat_server_.CreateOffers(arena);
  offers.push_back(fdf::MakeOffer<fuchsia_hardware_adc::Service>(arena, dev->name_));
  auto properties = std::vector{
      fdf::MakeProperty(arena, bind_fuchsia_hardware_adc::SERVICE,
                        bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(arena, bind_fuchsia_adc::CHANNEL, dev->channel_),
  };

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, dev->name_)
                  .offers(arena, std::move(offers))
                  .properties(arena, std::move(properties))
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                controller_endpoints.status_string());

  fidl::WireResult result =
      fidl::WireCall(adc->node())->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  dev->controller_.Bind(std::move(controller_endpoints->client));

  return zx::ok(std::move(dev));
}

zx::result<> Adc::Start() {
  // Get metadata.
  auto metadata =
      compat::GetMetadata<fuchsia_hardware_adcimpl::Metadata>(incoming(), DEVICE_METADATA_ADC);
  if (metadata.is_error()) {
    FDF_LOG(ERROR, "Failed to get metadata  %s", metadata.status_string());
    return metadata.take_error();
  }
  auto channels = std::move(*metadata->channels());

  // Make sure that the list of ADC channels has no duplicates.
  auto adc_cmp_lt = [](fuchsia_hardware_adcimpl::AdcChannel& lhs,
                       fuchsia_hardware_adcimpl::AdcChannel& rhs) {
    return *lhs.idx() < *rhs.idx();
  };
  auto adc_cmp_eq = [](fuchsia_hardware_adcimpl::AdcChannel& lhs,
                       fuchsia_hardware_adcimpl::AdcChannel& rhs) {
    return *lhs.idx() == *rhs.idx();
  };
  std::sort(channels.begin(), channels.end(), adc_cmp_lt);
  auto result = std::adjacent_find(channels.begin(), channels.end(), adc_cmp_eq);
  if (result != channels.end()) {
    FDF_LOG(ERROR, "adc channel '%d' was published more than once", *result->idx());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Create a device per channel.
  for (auto channel : channels) {
    auto adcimpl = incoming()->Connect<fuchsia_hardware_adcimpl::Service::Device>();
    if (adcimpl.is_error()) {
      FDF_LOG(ERROR, "Failed to open adcimpl service: %s", adcimpl.status_string());
      return adcimpl.take_error();
    }

    auto device = AdcDevice::Create(std::move(adcimpl.value()), std::move(channel), this);
    if (device.is_error()) {
      FDF_LOG(ERROR, "Failed to add create device %s", device.status_string());
      return zx::error(device.status_value());
    }
    devices_.emplace_back(std::move(*device));
  }

  return zx::ok();
}

void Adc::Stop() {
  for (auto& dev : devices_) {
    auto result = dev->controller()->Remove();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Could not remove child: %s", result.status_string());
    }
  }
}

}  // namespace adc

FUCHSIA_DRIVER_EXPORT(adc::Adc);
