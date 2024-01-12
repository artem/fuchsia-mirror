// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.softmac/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fidl_driver/cpp/wire_client.h>
#include <lib/operation/ethernet.h>
#include <lib/zx/result.h>

#include "device_interface.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

using StartStaCompleter = fit::callback<void(zx_status_t status)>;
using StopStaCompleter = fit::callback<void()>;

class SoftmacBridge : public fidl::WireServer<fuchsia_wlan_softmac::WlanSoftmacBridge> {
 public:
  static zx::result<std::unique_ptr<SoftmacBridge>> New(
      fdf::Dispatcher& softmac_bridge_server_dispatcher,
      std::unique_ptr<fit::callback<void(zx_status_t status)>> completer, DeviceInterface* device,
      fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client);
  zx_status_t StopSta(std::unique_ptr<StopStaCompleter> completer);
  ~SoftmacBridge() override;

  void Query(QueryCompleter::Sync& completer) final;
  void QueryDiscoverySupport(QueryDiscoverySupportCompleter::Sync& completer) final;
  void QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) final;
  void QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) final;
  void QuerySpectrumManagementSupport(
      QuerySpectrumManagementSupportCompleter::Sync& completer) final;
  void SetChannel(SetChannelRequestView request, SetChannelCompleter::Sync& completer) final;
  void JoinBss(JoinBssRequestView request, JoinBssCompleter::Sync& completer) final;
  void EnableBeaconing(EnableBeaconingRequestView request,
                       EnableBeaconingCompleter::Sync& completer) final;
  void DisableBeaconing(DisableBeaconingCompleter::Sync& completer) final;
  void InstallKey(InstallKeyRequestView request, InstallKeyCompleter::Sync& completer) final;
  void NotifyAssociationComplete(NotifyAssociationCompleteRequestView request,
                                 NotifyAssociationCompleteCompleter::Sync& completer) final;
  void ClearAssociation(ClearAssociationRequestView request,
                        ClearAssociationCompleter::Sync& completer) final;
  void StartPassiveScan(StartPassiveScanRequestView request,
                        StartPassiveScanCompleter::Sync& completer) final;
  void StartActiveScan(StartActiveScanRequestView request,
                       StartActiveScanCompleter::Sync& completer) final;
  void CancelScan(CancelScanRequestView request, CancelScanCompleter::Sync& completer) final;
  void UpdateWmmParameters(UpdateWmmParametersRequestView request,
                           UpdateWmmParametersCompleter::Sync& completer) final;

  void QueueEthFrameTx(eth::BorrowedOperation<> op);

 private:
  explicit SoftmacBridge(DeviceInterface* device_interface,
                         fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client)
      : softmac_client_(
            std::forward<fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>>(softmac_client)),
        device_interface_(device_interface) {}

  template <typename, typename = void>
  static constexpr bool has_value_type = false;
  template <typename T>
  static constexpr bool has_value_type<T, std::void_t<typename T::value_type>> = true;

  template <typename FidlMethod>
  static fidl::WireResultUnwrapType<FidlMethod> FlattenAndLogError(
      const std::string& method_name, fdf::WireUnownedResult<FidlMethod> result);

  template <typename FidlMethod>
  using Dispatcher = std::function<fdf::WireUnownedResult<FidlMethod>(
      const fdf::Arena&, const fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&)>;

  template <typename Completer, typename FidlMethod>
  void DispatchAndComplete(const std::string& method_name, Dispatcher<FidlMethod> dispatcher,
                           Completer& completer);

  std::unique_ptr<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>
      softmac_bridge_server_;
  fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac> softmac_client_;

  DeviceInterface* device_interface_;
  wlansoftmac_handle_t* rust_handle_;

  static wlansoftmac_in_buf_t IntoRustInBuf(std::unique_ptr<Buffer> owned_buffer);
  wlansoftmac_buffer_provider_ops_t rust_buffer_provider{
      .get_buffer = [](size_t min_len) -> wlansoftmac_in_buf_t {
        // Note: Once Rust MLME supports more than sending WLAN frames this needs
        // to change.
        auto buffer = GetBuffer(min_len);
        ZX_DEBUG_ASSERT(buffer != nullptr);
        return IntoRustInBuf(std::move(buffer));
      },
  };
};

}  // namespace wlan::drivers::wlansoftmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_
