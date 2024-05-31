// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "low_energy_connection_server.h"

#include "src/connectivity/bluetooth/core/bt-host/fidl/helpers.h"

namespace bthost {

namespace fbg = fuchsia::bluetooth::gatt2;

LowEnergyConnectionServer::LowEnergyConnectionServer(
    bt::gap::Adapter::WeakPtr adapter, bt::gatt::GATT::WeakPtr gatt,
    std::unique_ptr<bt::gap::LowEnergyConnectionHandle> connection, zx::channel handle,
    fit::callback<void()> closed_cb)
    : ServerBase(this, std::move(handle)),
      conn_(std::move(connection)),
      closed_handler_(std::move(closed_cb)),
      peer_id_(conn_->peer_identifier()),
      adapter_(std::move(adapter)),
      gatt_(std::move(gatt)) {
  BT_DEBUG_ASSERT(conn_);

  set_error_handler([this](zx_status_t) { OnClosed(); });
  conn_->set_closed_callback(fit::bind_member<&LowEnergyConnectionServer::OnClosed>(this));
}

void LowEnergyConnectionServer::OnClosed() {
  if (closed_handler_) {
    binding()->Close(ZX_ERR_CONNECTION_RESET);
    closed_handler_();
  }
}

void LowEnergyConnectionServer::RequestGattClient(fidl::InterfaceRequest<fbg::Client> client) {
  if (gatt_client_server_.has_value()) {
    bt_log(INFO, "fidl", "%s: gatt client server already bound (peer: %s)", __FUNCTION__,
           bt_str(peer_id_));
    client.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }

  fit::callback<void()> server_error_cb = [this] {
    bt_log(TRACE, "fidl", "gatt client server error (peer: %s)", bt_str(peer_id_));
    gatt_client_server_.reset();
  };
  gatt_client_server_.emplace(peer_id_, gatt_, std::move(client), std::move(server_error_cb));
}

void LowEnergyConnectionServer::AcceptCis(
    fuchsia::bluetooth::le::ConnectionAcceptCisRequest parameters) {}

void LowEnergyConnectionServer::GetCodecLocalDelayRange(
    ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest parameters,
    GetCodecLocalDelayRangeCallback callback) {
  bt_log(INFO, "fidl", "request received to read controller supported delay");

  if (!parameters.has_logical_transport_type()) {
    bt_log(WARN, "fidl", "request to read controller delay missing logical_transport_type");
    callback(fpromise::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  if (!parameters.has_data_direction()) {
    bt_log(WARN, "fidl", "request to read controller delay missing data_direction");
    callback(fpromise::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  if (!parameters.has_codec_attributes()) {
    bt_log(WARN, "fidl", "request to read controller delay missing codec_attributes");
    callback(fpromise::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  if (!parameters.codec_attributes().has_codec_id()) {
    bt_log(WARN, "fidl", "request to read controller delay missing codec_id");
    callback(fpromise::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  // Process required parameters
  pw::bluetooth::emboss::LogicalTransportType transport_type =
      fidl_helpers::LogicalTransportTypeFromFidl(parameters.logical_transport_type());
  pw::bluetooth::emboss::DataPathDirection direction =
      fidl_helpers::DataPathDirectionFromFidl(parameters.data_direction());
  bt::StaticPacket<pw::bluetooth::emboss::CodecIdWriter> codec_id =
      fidl_helpers::CodecIdFromFidl(parameters.codec_attributes().codec_id());

  // Codec configuration is optional
  std::optional<std::vector<uint8_t>> codec_configuration;
  if (parameters.codec_attributes().has_codec_configuration()) {
    codec_configuration = parameters.codec_attributes().codec_configuration();
  } else {
    codec_configuration = std::nullopt;
  }

  adapter_->GetSupportedDelayRange(
      codec_id, transport_type, direction, codec_configuration,
      [callback = std::move(callback)](zx_status_t status, uint32_t min_delay_us,
                                       uint32_t max_delay_us) {
        if (status != ZX_OK) {
          bt_log(WARN, "fidl", "failed to get controller supported delay");
          callback(fpromise::error(ZX_ERR_INTERNAL));
          return;
        }
        bt_log(INFO, "fidl", "controller supported delay [%d, %d] microseconds", min_delay_us,
               max_delay_us);
        fuchsia::bluetooth::le::CodecDelay_GetCodecLocalDelayRange_Response response;
        zx::duration min_delay = zx::usec(min_delay_us);
        zx::duration max_delay = zx::usec(max_delay_us);
        response.set_min_controller_delay(min_delay.get());
        response.set_max_controller_delay(max_delay.get());
        callback(fuchsia::bluetooth::le::CodecDelay_GetCodecLocalDelayRange_Result::WithResponse(
            std::move(response)));
      });
}

}  // namespace bthost
