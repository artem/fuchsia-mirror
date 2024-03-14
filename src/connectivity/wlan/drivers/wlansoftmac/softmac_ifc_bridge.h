// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.softmac/cpp/wire.h>
#include <fuchsia/wlan/softmac/c/banjo.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <lib/zx/result.h>

#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

class SoftmacIfcBridge : public fdf::WireServer<fuchsia_wlan_softmac::WlanSoftmacIfc> {
 public:
  static zx::result<std::unique_ptr<SoftmacIfcBridge>> New(
      const fdf::Dispatcher& softmac_ifc_server_dispatcher,
      const frame_processor_t* frame_processor,
      fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmacIfc>&& server_endpoint,
      fidl::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>&& bridge_client_endpoint);

  ~SoftmacIfcBridge() override = default;

  void Recv(RecvRequestView fdf_request, fdf::Arena& arena,
            RecvCompleter::Sync& completer) override;
  void ReportTxResult(ReportTxResultRequestView request, fdf::Arena& arena,
                      ReportTxResultCompleter::Sync& completer) override;
  void NotifyScanComplete(NotifyScanCompleteRequestView request, fdf::Arena& arena,
                          NotifyScanCompleteCompleter::Sync& completer) override;

 private:
  explicit SoftmacIfcBridge(const frame_processor_t* frame_processor)
      : frame_processor_(*frame_processor) {}
  const frame_processor_t frame_processor_;

  std::unique_ptr<fdf::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacIfc>>
      softmac_ifc_server_binding_;

  std::unique_ptr<fidl::WireClient<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>>
      softmac_ifc_bridge_client_;
};

}  // namespace wlan::drivers::wlansoftmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_
