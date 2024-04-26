// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <lib/operation/ethernet.h>
#include <lib/zx/result.h>

#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

class SoftmacIfcBridge : public fdf::Server<fuchsia_wlan_softmac::WlanSoftmacIfc> {
 public:
  static zx::result<std::unique_ptr<SoftmacIfcBridge>> New(
      const fdf::Dispatcher& dispatcher, const frame_processor_t* frame_processor,
      fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmacIfc>&& server_endpoint,
      fidl::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>&& bridge_client_endpoint);

  ~SoftmacIfcBridge() override = default;

  void Recv(RecvRequest& fdf_request, RecvCompleter::Sync& completer) override;
  zx::result<> EthernetTx(eth::BorrowedOperation<>* op, trace_async_id_t async_id) const;
  void ReportTxResult(ReportTxResultRequest& request,
                      ReportTxResultCompleter::Sync& completer) override;
  void NotifyScanComplete(NotifyScanCompleteRequest& request,
                          NotifyScanCompleteCompleter::Sync& completer) override;

 private:
  explicit SoftmacIfcBridge(const frame_processor_t* frame_processor)
      : frame_processor_(*frame_processor) {}
  const frame_processor_t frame_processor_;

  std::unique_ptr<fdf::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacIfc>>
      softmac_ifc_server_binding_;

  std::unique_ptr<fidl::Client<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>>
      softmac_ifc_bridge_client_;
};

}  // namespace wlan::drivers::wlansoftmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_
