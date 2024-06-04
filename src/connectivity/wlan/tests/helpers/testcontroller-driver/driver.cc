// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/fidl.h>
#include <fidl/test.wlan.testcontroller/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/stdcompat/source_location.h>

#include <sstream>
#include <vector>

#include <wlan/drivers/fidl_bridge.h>

namespace wlan_testcontroller {

using ::wlan::drivers::fidl_bridge::FidlErrorToStatus;
using ::wlan::drivers::fidl_bridge::ForwardResult;

// Server that forwards calls from WlanFullmacImplIfcChannel to WlanFullmacImplIfc.
class WlanFullmacImplIfcToDriverBridge
    : public fidl::Server<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge> {
  using WlanFullmacImplIfcBridge = fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge;
  using WlanFullmacImplIfc = fuchsia_wlan_fullmac::WlanFullmacImplIfc;

 public:
  explicit WlanFullmacImplIfcToDriverBridge(
      fdf_dispatcher_t* driver_dispatcher,
      fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge> server_end,
      fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc> bridge_client_end)
      : binding_(fdf_dispatcher_get_async_dispatcher(driver_dispatcher), std::move(server_end),
                 this, fidl::kIgnoreBindingClosure),
        bridge_client_(std::move(bridge_client_end), driver_dispatcher) {
    WLAN_TRACE_DURATION();
  }

  void OnScanResult(OnScanResultRequest& request, OnScanResultCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->OnScanResult(request).Then(
        ForwardResult<WlanFullmacImplIfc::OnScanResult>(completer.ToAsync()));
  }
  void OnScanEnd(OnScanEndRequest& request, OnScanEndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->OnScanEnd(request).Then(
        ForwardResult<WlanFullmacImplIfc::OnScanEnd>(completer.ToAsync()));
  }
  void ConnectConf(ConnectConfRequest& request, ConnectConfCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->ConnectConf(request).Then(
        ForwardResult<WlanFullmacImplIfc::ConnectConf>(completer.ToAsync()));
  }
  void RoamConf(RoamConfRequest& request, RoamConfCompleter::Sync& completer) override {}
  void AuthInd(AuthIndRequest& request, AuthIndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->AuthInd(request).Then(
        ForwardResult<WlanFullmacImplIfc::AuthInd>(completer.ToAsync()));
  }
  void DeauthConf(DeauthConfRequest& request, DeauthConfCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->DeauthConf(request).Then(
        ForwardResult<WlanFullmacImplIfc::DeauthConf>(completer.ToAsync()));
  }
  void DeauthInd(DeauthIndRequest& request, DeauthIndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->DeauthInd(request).Then(
        ForwardResult<WlanFullmacImplIfc::DeauthInd>(completer.ToAsync()));
  }
  void AssocInd(AssocIndRequest& request, AssocIndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->AssocInd(request).Then(
        ForwardResult<WlanFullmacImplIfc::AssocInd>(completer.ToAsync()));
  }
  void DisassocConf(DisassocConfRequest& request, DisassocConfCompleter::Sync& completer) override {
  }
  void DisassocInd(DisassocIndRequest& request, DisassocIndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->DisassocInd(request).Then(
        ForwardResult<WlanFullmacImplIfc::DisassocInd>(completer.ToAsync()));
  }
  void StartConf(StartConfRequest& request, StartConfCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->StartConf(request).Then(
        ForwardResult<WlanFullmacImplIfc::StartConf>(completer.ToAsync()));
  }
  void StopConf(StopConfRequest& request, StopConfCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->StopConf(request).Then(
        ForwardResult<WlanFullmacImplIfc::StopConf>(completer.ToAsync()));
  }
  void EapolConf(EapolConfRequest& request, EapolConfCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->EapolConf(request).Then(
        ForwardResult<WlanFullmacImplIfc::EapolConf>(completer.ToAsync()));
  }
  void OnChannelSwitch(OnChannelSwitchRequest& request,
                       OnChannelSwitchCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->OnChannelSwitch(request).Then(
        ForwardResult<WlanFullmacImplIfc::OnChannelSwitch>(completer.ToAsync()));
  }
  void SignalReport(SignalReportRequest& request, SignalReportCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->SignalReport(request).Then(
        ForwardResult<WlanFullmacImplIfc::SignalReport>(completer.ToAsync()));
  }
  void EapolInd(EapolIndRequest& request, EapolIndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->EapolInd(request).Then(
        ForwardResult<WlanFullmacImplIfc::EapolInd>(completer.ToAsync()));
  }
  void OnPmkAvailable(OnPmkAvailableRequest& request,
                      OnPmkAvailableCompleter::Sync& completer) override {}
  void SaeHandshakeInd(SaeHandshakeIndRequest& request,
                       SaeHandshakeIndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->SaeHandshakeInd(request).Then(
        ForwardResult<WlanFullmacImplIfc::SaeHandshakeInd>(completer.ToAsync()));
  }
  void SaeFrameRx(SaeFrameRxRequest& request, SaeFrameRxCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->SaeFrameRx(request).Then(
        ForwardResult<WlanFullmacImplIfc::SaeFrameRx>(completer.ToAsync()));
  }
  void OnWmmStatusResp(OnWmmStatusRespRequest& request,
                       OnWmmStatusRespCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->OnWmmStatusResp(request).Then(
        ForwardResult<WlanFullmacImplIfc::OnWmmStatusResp>(completer.ToAsync()));
  }

 private:
  fidl::ServerBinding<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge> binding_;
  fdf::Client<fuchsia_wlan_fullmac::WlanFullmacImplIfc> bridge_client_;
};

// Server that forwards calls from WlanFullmacImpl to WlanFullmacImplBridge.
class WlanFullmacImplToChannelBridge : public fdf::Server<fuchsia_wlan_fullmac::WlanFullmacImpl> {
 private:
  using WlanFullmacImplBridge = fuchsia_wlan_fullmac::WlanFullmacImplBridge;

 public:
  explicit WlanFullmacImplToChannelBridge(
      fdf_dispatcher_t* driver_dispatcher,
      fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end,
      fidl::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImplBridge> bridge_client_end)
      : binding_(driver_dispatcher, std::move(server_end), this, fidl::kIgnoreBindingClosure),
        bridge_client_(std::move(bridge_client_end),
                       fdf_dispatcher_get_async_dispatcher(driver_dispatcher)),
        driver_dispatcher_(driver_dispatcher) {
    WLAN_TRACE_DURATION();
  }

  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    // Start has to swap out the fdf::ClientEnd and instead pass along a fidl::ClientEnd to the
    // bridge server. This is necessary because the test case runs in a non-driver component, which
    // cannot use the fdf::ClientEnd.
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge>();
    if (endpoints.is_error()) {
      FDF_LOG(ERROR, "Could not create WlanFullmacImplIfcBridge endpoints: %s",
              endpoints.status_string());
      completer.Reply(endpoints.take_error());
      return;
    }

    // Create and bind fic bridge server.
    ifc_bridge_server_ = std::make_unique<WlanFullmacImplIfcToDriverBridge>(
        driver_dispatcher_, std::move(endpoints->server), std::move(request.ifc()));

    bridge_client_->Start(std::move(endpoints->client))
        .Then([completer = completer.ToAsync()](
                  fidl::Result<WlanFullmacImplBridge::Start>& result) mutable {
          WLAN_LAMBDA_TRACE_DURATION("WlanFullmacImplBridge::Start callback");
          // Unlike all the other methods in WlanFullmacImpl,
          // WlanFullmacImpl::Start and WlanFullmacImplBridge::Start results are
          // considered different types by the compiler. So we need to convert between the two
          // types manually here.
          if (result.is_error()) {
            auto& error = result.error_value();
            FDF_SLOG(ERROR, "Start failed", KV("status", error.FormatDescription()));
            completer.Reply(zx::error(FidlErrorToStatus(error)));
          } else {
            // Forward the SME channel provided by test suite
            completer.Reply(zx::ok(std::move(result->sme_channel())));
          }
        });
  }

  void Stop(StopCompleter::Sync& completer) override {}
  void Query(QueryCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->Query().Then(ForwardResult<WlanFullmacImplBridge::Query>(completer.ToAsync()));
  }
  void QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->QueryMacSublayerSupport().Then(
        ForwardResult<WlanFullmacImplBridge::QueryMacSublayerSupport>(completer.ToAsync()));
  }
  void QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->QuerySecuritySupport().Then(
        ForwardResult<WlanFullmacImplBridge::QuerySecuritySupport>(completer.ToAsync()));
  }
  void QuerySpectrumManagementSupport(
      QuerySpectrumManagementSupportCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->QuerySpectrumManagementSupport().Then(
        ForwardResult<WlanFullmacImplBridge::QuerySpectrumManagementSupport>(completer.ToAsync()));
  }
  void StartScan(StartScanRequest& request, StartScanCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->StartScan(request).Then(
        ForwardResult<WlanFullmacImplBridge::StartScan>(completer.ToAsync()));
  }
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->Connect(request).Then(
        ForwardResult<WlanFullmacImplBridge::Connect>(completer.ToAsync()));
  }
  void Reconnect(ReconnectRequest& request, ReconnectCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->Reconnect(request).Then(
        ForwardResult<WlanFullmacImplBridge::Reconnect>(completer.ToAsync()));
  }
  void AuthResp(AuthRespRequest& request, AuthRespCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->AuthResp(request).Then(
        ForwardResult<WlanFullmacImplBridge::AuthResp>(completer.ToAsync()));
  }
  void Deauth(DeauthRequest& request, DeauthCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->Deauth(request).Then(
        ForwardResult<WlanFullmacImplBridge::Deauth>(completer.ToAsync()));
  }
  void AssocResp(AssocRespRequest& request, AssocRespCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->AssocResp(request).Then(
        ForwardResult<WlanFullmacImplBridge::AssocResp>(completer.ToAsync()));
  }
  void Disassoc(DisassocRequest& request, DisassocCompleter::Sync& completer) override {}
  void Reset(ResetRequest& request, ResetCompleter::Sync& completer) override {}
  void StartBss(StartBssRequest& request, StartBssCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->StartBss(request).Then(
        ForwardResult<WlanFullmacImplBridge::StartBss>(completer.ToAsync()));
  }
  void StopBss(StopBssRequest& request, StopBssCompleter ::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->StopBss(request).Then(
        ForwardResult<WlanFullmacImplBridge::StopBss>(completer.ToAsync()));
  }
  void SetKeysReq(SetKeysReqRequest& request, SetKeysReqCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->SetKeysReq(request).Then(
        ForwardResult<WlanFullmacImplBridge::SetKeysReq>(completer.ToAsync()));
  }
  void DelKeysReq(DelKeysReqRequest& request, DelKeysReqCompleter::Sync& completer) override {}
  void EapolTx(EapolTxRequest& request, EapolTxCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->EapolTx(request).Then(
        ForwardResult<WlanFullmacImplBridge::EapolTx>(completer.ToAsync()));
  }
  void GetIfaceCounterStats(GetIfaceCounterStatsCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->GetIfaceCounterStats().Then(
        ForwardResult<WlanFullmacImplBridge::GetIfaceCounterStats>(completer.ToAsync()));
  }
  void GetIfaceHistogramStats(GetIfaceHistogramStatsCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->GetIfaceHistogramStats().Then(
        ForwardResult<WlanFullmacImplBridge::GetIfaceHistogramStats>(completer.ToAsync()));
  }
  void SetMulticastPromisc(SetMulticastPromiscRequest& request,
                           SetMulticastPromiscCompleter::Sync& completer) override {}
  void SaeHandshakeResp(SaeHandshakeRespRequest& request,
                        SaeHandshakeRespCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->SaeHandshakeResp(request).Then(
        ForwardResult<WlanFullmacImplBridge::SaeHandshakeResp>(completer.ToAsync()));
  }
  void SaeFrameTx(SaeFrameTxRequest& request, SaeFrameTxCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->SaeFrameTx(request).Then(
        ForwardResult<WlanFullmacImplBridge::SaeFrameTx>(completer.ToAsync()));
  }
  void WmmStatusReq(WmmStatusReqCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->WmmStatusReq().Then(
        ForwardResult<WlanFullmacImplBridge::WmmStatusReq>(completer.ToAsync()));
  }
  void OnLinkStateChanged(OnLinkStateChangedRequest& request,
                          OnLinkStateChangedCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->OnLinkStateChanged(request).Then(
        ForwardResult<WlanFullmacImplBridge::OnLinkStateChanged>(completer.ToAsync()));
  }

 private:
  fdf::ServerBinding<fuchsia_wlan_fullmac::WlanFullmacImpl> binding_;
  fidl::Client<fuchsia_wlan_fullmac::WlanFullmacImplBridge> bridge_client_;
  fdf_dispatcher_t* driver_dispatcher_{};
  std::unique_ptr<WlanFullmacImplIfcToDriverBridge> ifc_bridge_server_;
};

class TestController : public fdf::DriverBase,
                       public fidl::Server<test_wlan_testcontroller::TestController> {
  static constexpr std::string_view kDriverName = "wlan_testcontroller";

 public:
  TestController(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure)) {
  }

  zx::result<> Start() override {
    WLAN_TRACE_DURATION();
    node_.Bind(std::move(node()));
    fidl::Arena arena;

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    // By calling AddChild with devfs_args, the child driver will be discoverable through devfs.
    auto args =
        fuchsia_driver_framework::NodeAddArgs({.name = std::string(kDriverName),
                                               .devfs_args = fuchsia_driver_framework::DevfsAddArgs(
                                                   {.connector = std::move(connector.value())})});

    auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

    auto result = node_->AddChild({std::move(args), std::move(controller_endpoints.server), {}});
    if (result.is_error()) {
      auto& error = result.error_value();
      FDF_SLOG(ERROR, "Failed to add child", KV("status", error.FormatDescription()));

      // AddChild's domain error is a NodeError, not a zx_status_t
      return zx::error(error.is_domain_error() ? ZX_ERR_INTERNAL
                                               : error.framework_error().status());
    }

    return zx::ok();
  }

  void CreateFullmac(CreateFullmacRequest& request,
                     CreateFullmacCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    auto protocol_handler =
        [this, bridge_client = std::move(request.bridge_client())](
            fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) mutable {
          WLAN_LAMBDA_TRACE_DURATION("WlanFullmacImpl::Service protocol handler");
          auto impl = std::make_unique<WlanFullmacImplToChannelBridge>(
              driver_dispatcher()->get(), std::move(server_end), std::move(bridge_client));
          fullmac_bridges_.push_back(std::move(impl));
        };

    fuchsia_wlan_fullmac::Service::InstanceHandler handler(
        {.wlan_fullmac_impl = std::move(protocol_handler)});

    // Generate a unique child name for the new fullmac child.
    std::stringstream ss;
    ss << "fullmac-child-" << num_fullmac_children_++;
    std::string child_name = ss.str();

    zx::result result =
        outgoing()->AddService<fuchsia_wlan_fullmac::Service>(std::move(handler), child_name);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add fullmac service: %s", result.status_string());
      completer.Reply(result.take_error());
      return;
    }

    auto offers = std::vector{fdf::MakeOffer2<fuchsia_wlan_fullmac::Service>(child_name)};

    auto args = fuchsia_driver_framework::NodeAddArgs({
        .name = child_name,
        .offers2 = std::move(offers),
    });

    zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    ZX_ASSERT(controller_endpoints.is_ok());

    auto add_child_result =
        node_->AddChild({std::move(args), std::move(controller_endpoints->server), {}});
    if (add_child_result.is_error()) {
      auto& error = add_child_result.error_value();
      FDF_SLOG(ERROR, "Failed to add child", KV("status", error.FormatDescription()));

      // AddChild's domain error is a NodeError, not a zx_status_t
      completer.Reply(zx::error(
          (error.is_domain_error() ? ZX_ERR_INTERNAL : error.framework_error().status())));
      return;
    }
    completer.Reply(zx::ok());
  }

 private:
  fidl::SyncClient<fuchsia_driver_framework::Node> node_;

  // Holds bindings to TestController, which all bind to this class
  fidl::ServerBindingGroup<test_wlan_testcontroller::TestController> bindings_;

  // devfs_connector_ lets the class serve the TestController protocol over devfs.
  driver_devfs::Connector<test_wlan_testcontroller::TestController> devfs_connector_;

  // Tracks the number of fullmac client and aps created. Used to generate names.
  int num_fullmac_children_ = 0;

  std::vector<std::unique_ptr<WlanFullmacImplToChannelBridge>> fullmac_bridges_;
};

}  // namespace wlan_testcontroller

FUCHSIA_DRIVER_EXPORT(wlan_testcontroller::TestController);
