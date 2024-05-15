// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDIO_FUNCTION_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDIO_FUNCTION_DEVICE_H_

#include <fidl/fuchsia.hardware.sdio/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.sdio/cpp/wire.h>
#include <fuchsia/hardware/sdio/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>

#include <atomic>
#include <memory>

namespace sdmmc {

using fuchsia_hardware_sdio::wire::SdioRwTxn;

class SdioControllerDevice;

class SdioFunctionDevice : public ddk::SdioProtocol<SdioFunctionDevice> {
 public:
  static zx_status_t Create(SdioControllerDevice* sdio_parent, uint32_t func,
                            std::unique_ptr<SdioFunctionDevice>* out_dev);

  zx_status_t AddDevice(const sdio_func_hw_info_t& hw_info);

  // Both Banjo and FIDL are supported. Changes in one implementation should be reflected in the
  // other.
  // TODO(b/333726427): Delete the Banjo implementation.

  zx_status_t SdioGetDevHwInfo(sdio_hw_info_t* out_hw_info);
  zx_status_t SdioEnableFn();
  zx_status_t SdioDisableFn();
  zx_status_t SdioEnableFnIntr();
  zx_status_t SdioDisableFnIntr();
  zx_status_t SdioUpdateBlockSize(uint16_t blk_sz, bool deflt);
  zx_status_t SdioGetBlockSize(uint16_t* out_cur_blk_size);
  zx_status_t SdioDoRwByte(bool write, uint32_t addr, uint8_t write_byte, uint8_t* out_read_byte);
  zx_status_t SdioGetInBandIntr(zx::interrupt* out_irq);
  void SdioAckInBandIntr();
  zx_status_t SdioIoAbort();
  zx_status_t SdioIntrPending(bool* out_pending);
  zx_status_t SdioDoVendorControlRwByte(bool write, uint8_t addr, uint8_t write_byte,
                                        uint8_t* out_read_byte);
  zx_status_t SdioRegisterVmo(uint32_t vmo_id, zx::vmo vmo, uint64_t offset, uint64_t size,
                              uint32_t vmo_rights);
  zx_status_t SdioUnregisterVmo(uint32_t vmo_id, zx::vmo* out_vmo);
  zx_status_t SdioDoRwTxn(const sdio_rw_txn_t* txn);
  zx_status_t SdioRequestCardReset();
  zx_status_t SdioPerformTuning();

 private:
  class DriverTransportImpl : public fdf::WireServer<fuchsia_hardware_sdio::DriverDevice> {
   public:
    explicit DriverTransportImpl(SdioFunctionDevice* parent) : parent_(parent) {}

   private:
    void GetDevHwInfo(fdf::Arena& arena, GetDevHwInfoCompleter::Sync& completer) override;
    void EnableFn(fdf::Arena& arena, EnableFnCompleter::Sync& completer) override;
    void DisableFn(fdf::Arena& arena, DisableFnCompleter::Sync& completer) override;
    void EnableFnIntr(fdf::Arena& arena, EnableFnIntrCompleter::Sync& completer) override;
    void DisableFnIntr(fdf::Arena& arena, DisableFnIntrCompleter::Sync& completer) override;
    void UpdateBlockSize(fuchsia_hardware_sdio::wire::DeviceUpdateBlockSizeRequest* request,
                         fdf::Arena& arena, UpdateBlockSizeCompleter::Sync& completer) override;
    void GetBlockSize(fdf::Arena& arena, GetBlockSizeCompleter::Sync& completer) override;
    void DoRwByte(fuchsia_hardware_sdio::wire::DeviceDoRwByteRequest* request, fdf::Arena& arena,
                  DoRwByteCompleter::Sync& completer) override;
    void GetInBandIntr(fdf::Arena& arena, GetInBandIntrCompleter::Sync& completer) override;
    void AckInBandIntr(fdf::Arena& arena, AckInBandIntrCompleter::Sync& completer) override;
    void IoAbort(fdf::Arena& arena, IoAbortCompleter::Sync& completer) override;
    void IntrPending(fdf::Arena& arena, IntrPendingCompleter::Sync& completer) override;
    void DoVendorControlRwByte(
        fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteRequest* request, fdf::Arena& arena,
        DoVendorControlRwByteCompleter::Sync& completer) override;
    void RegisterVmo(fuchsia_hardware_sdio::wire::DeviceRegisterVmoRequest* request,
                     fdf::Arena& arena, RegisterVmoCompleter::Sync& completer) override;
    void UnregisterVmo(fuchsia_hardware_sdio::wire::DeviceUnregisterVmoRequest* request,
                       fdf::Arena& arena, UnregisterVmoCompleter::Sync& completer) override;
    void DoRwTxn(fuchsia_hardware_sdio::wire::DeviceDoRwTxnRequest* request, fdf::Arena& arena,
                 DoRwTxnCompleter::Sync& completer) override;
    void RequestCardReset(fdf::Arena& arena, RequestCardResetCompleter::Sync& completer) override;
    void PerformTuning(fdf::Arena& arena, PerformTuningCompleter::Sync& completer) override;

    SdioFunctionDevice* const parent_;
  };

  class ZirconTransportImpl : public fidl::WireServer<fuchsia_hardware_sdio::Device> {
   public:
    explicit ZirconTransportImpl(SdioFunctionDevice* parent) : parent_(parent) {}

   private:
    void GetDevHwInfo(GetDevHwInfoCompleter::Sync& completer) override;
    void EnableFn(EnableFnCompleter::Sync& completer) override;
    void DisableFn(DisableFnCompleter::Sync& completer) override;
    void EnableFnIntr(EnableFnIntrCompleter::Sync& completer) override;
    void DisableFnIntr(DisableFnIntrCompleter::Sync& completer) override;
    void UpdateBlockSize(UpdateBlockSizeRequestView request,
                         UpdateBlockSizeCompleter::Sync& completer) override;
    void GetBlockSize(GetBlockSizeCompleter::Sync& completer) override;
    void DoRwByte(DoRwByteRequestView request, DoRwByteCompleter::Sync& completer) override;
    void GetInBandIntr(GetInBandIntrCompleter::Sync& completer) override;
    void AckInBandIntr(AckInBandIntrCompleter::Sync& completer) override;
    void IoAbort(IoAbortCompleter::Sync& completer) override;
    void IntrPending(IntrPendingCompleter::Sync& completer) override;
    void DoVendorControlRwByte(DoVendorControlRwByteRequestView request,
                               DoVendorControlRwByteCompleter::Sync& completer) override;
    void RegisterVmo(RegisterVmoRequestView request,
                     RegisterVmoCompleter::Sync& completer) override;
    void UnregisterVmo(UnregisterVmoRequestView request,
                       UnregisterVmoCompleter::Sync& completer) override;
    void DoRwTxn(DoRwTxnRequestView request, DoRwTxnCompleter::Sync& completer) override;
    void RequestCardReset(RequestCardResetCompleter::Sync& completer) override;
    void PerformTuning(PerformTuningCompleter::Sync& completer) override;

    SdioFunctionDevice* const parent_;
  };

  SdioFunctionDevice(SdioControllerDevice* sdio_parent, uint32_t func)
      : function_(static_cast<uint8_t>(func)),
        sdio_parent_(sdio_parent),
        driver_transport_impl_(this),
        zircon_transport_impl_(this) {
    sdio_function_name_ = "sdmmc-sdio-" + std::to_string(func);
  }

  // FIDL-type implementations. Serves both Zircon and Driver transport types.

  zx::result<fuchsia_hardware_sdio::wire::DeviceGetDevHwInfoResponse*> GetDevHwInfo(
      fuchsia_hardware_sdio::wire::DeviceGetDevHwInfoResponse* response);

  zx::result<> EnableFn();

  zx::result<> DisableFn();

  zx::result<> EnableFnIntr();

  zx::result<> DisableFnIntr();

  zx::result<> UpdateBlockSize(fuchsia_hardware_sdio::wire::DeviceUpdateBlockSizeRequest* request);

  zx::result<fuchsia_hardware_sdio::wire::DeviceGetBlockSizeResponse*> GetBlockSize(
      fuchsia_hardware_sdio::wire::DeviceGetBlockSizeResponse* response);

  zx::result<fuchsia_hardware_sdio::wire::DeviceDoRwByteResponse*> DoRwByte(
      fuchsia_hardware_sdio::wire::DeviceDoRwByteRequest* request,
      fuchsia_hardware_sdio::wire::DeviceDoRwByteResponse* response);

  zx::result<fuchsia_hardware_sdio::wire::DeviceGetInBandIntrResponse*> GetInBandIntr(
      fuchsia_hardware_sdio::wire::DeviceGetInBandIntrResponse* response);

  void AckInBandIntr();

  zx::result<> IoAbort();

  zx::result<fuchsia_hardware_sdio::wire::DeviceIntrPendingResponse*> IntrPending(
      fuchsia_hardware_sdio::wire::DeviceIntrPendingResponse* response);

  zx::result<fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteResponse*>
  DoVendorControlRwByte(fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteRequest* request,
                        fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteResponse* response);

  zx::result<> RegisterVmo(fuchsia_hardware_sdio::wire::DeviceRegisterVmoRequest* request);

  zx::result<fuchsia_hardware_sdio::wire::DeviceUnregisterVmoResponse*> UnregisterVmo(
      fuchsia_hardware_sdio::wire::DeviceUnregisterVmoRequest* request,
      fuchsia_hardware_sdio::wire::DeviceUnregisterVmoResponse* response);

  zx::result<> DoRwTxn(fuchsia_hardware_sdio::wire::DeviceDoRwTxnRequest* request);

  zx::result<> RequestCardReset();

  zx::result<> PerformTuning();

  fdf::Logger& logger();

  uint8_t function_ = SDIO_MAX_FUNCS;
  SdioControllerDevice* const sdio_parent_;

  std::string sdio_function_name_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  compat::BanjoServer sdio_server_{ZX_PROTOCOL_SDIO, this, &sdio_protocol_ops_};
  compat::SyncInitializedDeviceServer compat_server_;
  DriverTransportImpl driver_transport_impl_;
  ZirconTransportImpl zircon_transport_impl_;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDIO_FUNCTION_DEVICE_H_
