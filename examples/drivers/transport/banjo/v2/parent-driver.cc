// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fuchsia/examples/gizmo/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace banjo_transport {

class ParentBanjoTransportDriver : public fdf::DriverBase,
                                   public ddk::MiscProtocol<ParentBanjoTransportDriver> {
 public:
  ParentBanjoTransportDriver(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("transport-parent", std::move(start_args), std::move(driver_dispatcher)) {}

  void Start(fdf::StartCompleter completer) override {
    node_.Bind(std::move(node()));
    start_completer_.emplace(std::move(completer));
    child_.OnInitialized(
        fit::bind_member<&ParentBanjoTransportDriver::CompatServerInitialized>(this));
  }

  // Add a child device node and offer the service capabilities.
  void CompatServerInitialized(zx::result<> compat_result) {
    if (compat_result.is_error()) {
      return CompleteStart(compat_result.take_error());
    }

    // Offer `fuchsia.examples.gizmo.Service` to the driver that binds to the node.
    auto args = fuchsia_driver_framework::NodeAddArgs({
        .name = std::string(name()),
        .offers = child_.CreateOffers(),
        .properties = {{banjo_server_.property()}},
    });

    // Create endpoints of the `NodeController` for the node.
    auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    if (endpoints.is_error()) {
      FDF_SLOG(ERROR, "Failed to create endpoint", KV("status", endpoints.status_string()));
      return CompleteStart(zx::error(endpoints.status_value()));
    }

    auto result = node_->AddChild({std::move(args), std::move(endpoints->server), {}});
    if (result.is_error()) {
      const auto& error = result.error_value();
      FDF_SLOG(ERROR, "Failed to add child", KV("status", error.FormatDescription()));
      return CompleteStart(zx::error(error.is_domain_error()
                                         ? static_cast<uint32_t>(error.domain_error())
                                         : error.framework_error().status()));
    }
    controller_.Bind(std::move(endpoints->client), dispatcher());

    CompleteStart(zx::ok());
  }

  zx_status_t MiscGetHardwareId(uint32_t* out_response) {
    *out_response = 0x1234ABCD;

    return ZX_OK;
  }

  zx_status_t MiscGetFirmwareVersion(uint32_t* out_major, uint32_t* out_minor) {
    *out_major = 0x0;
    *out_minor = 0x1;

    return ZX_OK;
  }

 private:
  compat::DeviceServer::BanjoConfig get_banjo_config() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_MISC};
    config.callbacks[ZX_PROTOCOL_MISC] = banjo_server_.callback();
    return config;
  }

  void CompleteStart(zx::result<> result) {
    ZX_ASSERT(start_completer_.has_value());
    start_completer_.value()(result);
    start_completer_.reset();
  }

  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
  fidl::Client<fuchsia_driver_framework::NodeController> controller_;

  std::optional<fdf::StartCompleter> start_completer_;

  compat::BanjoServer banjo_server_{ZX_PROTOCOL_MISC, this, &misc_protocol_ops_};
  compat::DeviceServer child_{dispatcher(),
                              incoming(),
                              outgoing(),
                              node_name(),
                              name(),
                              std::nullopt,
                              compat::ForwardMetadata::None(),
                              get_banjo_config()};
};

}  // namespace banjo_transport

FUCHSIA_DRIVER_EXPORT(banjo_transport::ParentBanjoTransportDriver);
