// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/test/power-integration-test/test-power-parent-device.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/element-description.h>
#include <lib/driver/power/cpp/power-support.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/test/platform/cpp/bind.h>

namespace fake_parent_device {

zx::result<> FakeParent::Start() {
  node_.Bind(std::move(node()));
  fdf::Arena arena('TEST');

  auto device_service =
      incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("platform-device");
  if (!device_service.is_ok()) {
    return zx::error(ZX_ERR_INTERNAL);
  }

  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> device_client(
      std::move(device_service.value()));
  auto config_result = device_client->GetPowerConfiguration();
  auto power_config = config_result->value()->config.data();

  // connect to power broker
  auto power_broker_req = incoming()->Connect<fuchsia_power_broker::Topology>();
  {
    if (power_broker_req.is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    fit::result<fdf_power::Error, fdf_power::TokenMap> token_result =
        fdf_power::GetDependencyTokens(*incoming(), power_config[0]);
    if (token_result.is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    fdf_power::TokenMap tokens = std::move(token_result.value());

    fdf_power::ElementDesc description =
        fdf_power::ElementDescBuilder(*power_config, std::move(tokens)).Build();

    fidl::ClientEnd<fuchsia_power_broker::Topology> broker = std::move(power_broker_req.value());
    fit::result<fdf_power::Error, fuchsia_power_broker::TopologyAddElementResponse> add_result =
        fdf_power::AddElement(
            broker, power_config[0], std::move(description.tokens_),
            description.active_token_.borrow(), description.passive_token_.borrow(),
            std::move(description.level_control_servers_), std::move(description.lessor_server_));

    topology_client_ =
        fidl::WireClient<fuchsia_power_broker::Topology>(std::move(broker), dispatcher());

    if (!add_result.is_ok()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    element_ctrl_ = std::move(add_result->element_control_channel());
  }

  // Add a child
  {
    std::string power_element_name = std::string(power_config[0].element().name().data(),
                                                 power_config[0].element().name().size());

    // First we'll set up a FIDL server that provides a token which gives
    // our child access to our power element.
    server_ = std::make_unique<fake_parent_device::FakeParentServer>(power_element_name);
    {
      auto result = outgoing()->AddService<fuchsia_hardware_power::PowerTokenService>(
          fuchsia_hardware_power::PowerTokenService::InstanceHandler({
              .token_provider =
                  bindings_.CreateHandler(server_.get(), dispatcher(), fidl::kIgnoreBindingClosure),
          }),
          power_element_name);

      if (!result.is_ok()) {
        return zx::error(ZX_ERR_INTERNAL);
      }
    }

    // Next create a node for our child to bind to.
    fuchsia_driver_framework::NodeAddArgs node_args;
    node_args.name() = "fake-child";
    auto properties = std::vector{
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST),
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_CHILD),
    };
    node_args.properties() = properties;

    // Create the offer for our token provider so the child can access it.
    {
      std::vector<fuchsia_component_decl::NameMapping> mappings =
          std::vector<fuchsia_component_decl::NameMapping>{fuchsia_component_decl::NameMapping{{
              .source_name = power_element_name,
              .target_name = power_element_name,
          }}};

      fuchsia_component_decl::OfferService services = fuchsia_component_decl::OfferService{{
          .source_name = std::string(fuchsia_hardware_power::PowerTokenService::Name),
          .target_name = std::string(fuchsia_hardware_power::PowerTokenService::Name),
          .source_instance_filter = std::vector<std::string>{power_element_name},
          .renamed_instances = mappings,
      }};

      auto component_offer_decl = fuchsia_component_decl::Offer::WithService(services);
      node_args.offers2() = std::vector<fuchsia_driver_framework::Offer>{
          fuchsia_driver_framework::Offer::WithZirconTransport(component_offer_decl),
      };
    }

    // The token service is ready, the child is defined, so we're ready to
    // add the child device node
    {
      auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>().value();
      auto result = node_->AddChild(fidl::ToWire(arena, std::move(node_args)),
                                    std::move(endpoints.server), {});

      if (!result.ok()) {
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (result->is_error()) {
        return zx::error(ZX_ERR_INTERNAL);
      }

      child_controller_.Bind(std::move(endpoints.client));
    }
  }

  return zx::ok();
}

void FakeParentServer::GetToken(GetTokenCompleter::Sync& completer) {
  zx_handle_t mine;
  zx_event_create(0, &mine);
  zx_handle_t yours;
  zx_handle_duplicate(mine, ZX_RIGHT_SAME_RIGHTS, &yours);
  completer.ReplySuccess(zx::event(yours), fidl::StringView::FromExternal(element_name_));
}
void FakeParentServer::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_power::PowerTokenProvider> md,
    fidl::UnknownMethodCompleter::Sync& completer) {}

}  // namespace fake_parent_device

FUCHSIA_DRIVER_EXPORT(fake_parent_device::FakeParent);
