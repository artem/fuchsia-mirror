// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/softmac/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/operation/ethernet.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>

#include <memory>
#include <mutex>

#include <ddktl/device.h>
#include <fbl/ref_ptr.h>
#include <wlan/common/macaddr.h>

#include "buffer_allocator.h"
#include "device_interface.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

#define PRE_ALLOC_RECV_BUFFER_SIZE 2000

class WlanSoftmacHandle {
 public:
  explicit WlanSoftmacHandle(DeviceInterface* device);
  ~WlanSoftmacHandle();

  zx_status_t Init(fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac> client);
  zx_status_t StopMainLoop();
  void QueueEthFrameTx(eth::BorrowedOperation<> op);

 private:
  DeviceInterface* device_;
  wlansoftmac_handle_t* inner_handle_;

  static DeviceInterface* AsDeviceInterface(void* device) {
    return static_cast<DeviceInterface*>(device);
  }

  async::Loop wlan_softmac_bridge_server_loop_;
};

class SoftmacBinding : public DeviceInterface,
                       public fdf::WireServer<fuchsia_wlan_softmac::WlanSoftmacIfc> {
 public:
  static zx::result<std::unique_ptr<SoftmacBinding>> New(zx_device_t* device);
  ~SoftmacBinding() override = default;

  static constexpr inline SoftmacBinding* AsSoftmacBinding(void* ctx) {
    return static_cast<SoftmacBinding*>(ctx);
  }

  // DeviceInterface methods
  zx_status_t Start(const rust_wlan_softmac_ifc_protocol_copy_t* ifc,
                    zx::channel* out_sme_channel) final;
  zx_status_t DeliverEthernet(cpp20::span<const uint8_t> eth_frame) final
      __TA_EXCLUDES(ethernet_proxy_lock_);
  zx_status_t QueueTx(UsedBuffer used_buffer, wlan_tx_info_t tx_info) final;
  zx_status_t SetEthernetStatus(uint32_t status) final __TA_EXCLUDES(ethernet_proxy_lock_);
  zx_status_t JoinBss(join_bss_request_t* cfg) final;
  zx_status_t InstallKey(wlan_key_configuration_t* key_config) final;
  fbl::RefPtr<DeviceState> GetState() final;
  const discovery_support_t& GetDiscoverySupport() const final;
  const mac_sublayer_support_t& GetMacSublayerSupport() const final;
  const security_support_t& GetSecuritySupport() const final;
  const spectrum_management_support_t& GetSpectrumManagementSupport() const final;

  void Recv(RecvRequestView request, fdf::Arena& arena, RecvCompleter::Sync& completer) override;
  void ReportTxResult(ReportTxResultRequestView request, fdf::Arena& arena,
                      ReportTxResultCompleter::Sync& completer) override;
  void NotifyScanComplete(NotifyScanCompleteRequestView request, fdf::Arena& arena,
                          NotifyScanCompleteCompleter::Sync& completer) override;

 private:
  // Private constructor to require use of New().
  explicit SoftmacBinding(zx_device_t* device);
  zx_device_t* device_ = nullptr;

  /////////////////////////////////////
  // Member variables and methods to implement a child device
  // supporting the ZX_PROTOCOL_ETHERNET_IMPL custom protocol.
  zx_device_t* child_device_ = nullptr;
  void Unbind();
  void Release();

  zx_status_t EthernetImplQuery(uint32_t options, ethernet_info_t* info);
  zx_status_t EthernetImplStart(const ethernet_ifc_protocol_t* ifc)
      __TA_EXCLUDES(ethernet_proxy_lock_);
  void EthernetImplStop() __TA_EXCLUDES(ethernet_proxy_lock_);
  void EthernetImplQueueTx(uint32_t options, ethernet_netbuf_t* netbuf,
                           ethernet_impl_queue_tx_callback callback, void* cookie);
  static zx_status_t EthernetImplSetParam(uint32_t param, int32_t value, const uint8_t* data_buffer,
                                          size_t data_size);
  static void EthernetImplGetBti(zx_handle_t* out_bti);

  const zx_protocol_device_t eth_device_ops_ = {
      .version = DEVICE_OPS_VERSION,
      .init =
          [](void* ctx) {
            device_init_reply_args_t args = {};
            device_init_reply(AsSoftmacBinding(ctx)->child_device_, ZX_OK, &args);
          },
      .unbind = [](void* ctx) { AsSoftmacBinding(ctx)->Unbind(); },
      .release = [](void* ctx) { AsSoftmacBinding(ctx)->Release(); },
  };

  const ethernet_impl_protocol_ops_t ethernet_impl_ops_ = {
      .query = [](void* ctx, uint32_t options, ethernet_info_t* info) -> zx_status_t {
        return AsSoftmacBinding(ctx)->EthernetImplQuery(options, info);
      },
      .stop = [](void* ctx) { AsSoftmacBinding(ctx)->EthernetImplStop(); },
      .start = [](void* ctx, const ethernet_ifc_protocol_t* ifc) -> zx_status_t {
        return AsSoftmacBinding(ctx)->EthernetImplStart(ifc);
      },
      .queue_tx =
          [](void* ctx, uint32_t options, ethernet_netbuf_t* netbuf,
             ethernet_impl_queue_tx_callback callback, void* cookie) {
            AsSoftmacBinding(ctx)->EthernetImplQueueTx(options, netbuf, callback, cookie);
          },
      .set_param = [](void* ctx, uint32_t param, int32_t value, const uint8_t* data_buffer,
                      size_t data_size) -> zx_status_t {
        return SoftmacBinding::EthernetImplSetParam(param, value, data_buffer, data_size);
      },
      .get_bti = [](void* ctx,
                    zx_handle_t* out_bti) { SoftmacBinding::EthernetImplGetBti(out_bti); },
  };

  std::mutex ethernet_proxy_lock_;
  ddk::EthernetIfcProtocolClient ethernet_proxy_ __TA_GUARDED(ethernet_proxy_lock_);

  /////////////////////////////////////
  // Member variables and methods to support communication via SME,
  // MLME, and WlanSoftmac protocols.

  enum class DevicePacket : uint64_t {
    kShutdown,
    kPacketQueued,
    kIndication,
    kHwScanComplete,
  };

  // Informs the message loop to shut down. Calling this function more than once
  // has no effect.
  void ShutdownMainLoop();

  bool main_loop_dead_ = false;

  // Manages the lifetime of the protocol struct we pass down to the vendor driver. Actual
  // calls to this protocol should only be performed by the vendor driver.
  std::unique_ptr<wlan_softmac_ifc_protocol_ops_t> wlan_softmac_ifc_protocol_ops_;
  std::unique_ptr<wlan_softmac_ifc_protocol_t> wlan_softmac_ifc_protocol_;

  // TODO(fxbug.dev/135358): Do not cache feature support in the generic driver.
  //                         This will cause more queries into the vendor
  //                         driver, but these calls seem inexpensive, these
  //                         calls have not been profiled, and this behavior is
  //                         not documented in the SDK. Moreover, caching
  //                         introduces opportunities for subtle bugs.
  discovery_support_t discovery_support_ = {};
  mac_sublayer_support_t mac_sublayer_support_ = {};
  security_support_t security_support_ = {};
  spectrum_management_support_t spectrum_management_support_ = {};
  fbl::RefPtr<DeviceState> state_;

  std::unique_ptr<WlanSoftmacHandle> softmac_handle_;

  // The FIDL client to communicate with iwlwifi
  fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac> client_;

  // Dispatcher for being a FIDL client firing requests on WlanSoftmac protocol.
  fdf::Dispatcher client_dispatcher_;

  // Dispatcher for being a FIDL server dispatching requests from WlanSoftmacIfc protocol.
  fdf::Dispatcher server_dispatcher_;

  // Preallocated buffer for small frames
  uint8_t pre_alloc_recv_buffer_[PRE_ALLOC_RECV_BUFFER_SIZE];

  // Lock for Rec() function to make it thread safe.
  std::mutex rx_lock_;
};

}  // namespace wlan::drivers::wlansoftmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_H_
