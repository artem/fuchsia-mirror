// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDMMC_DFV1_SDMMC_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDMMC_DFV1_SDMMC_DEVICE_H_

#include <fidl/fuchsia.hardware.sdmmc/cpp/driver/fidl.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/sdmmc/hw.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>

#include <array>

#include "sdmmc-root-device.h"

namespace sdmmc {

// SdmmcDevice wraps a ddk::SdmmcProtocolClient to provide helper methods to the SD/MMC and SDIO
// core drivers. It is assumed that the underlying SDMMC protocol driver can handle calls from
// different threads, although care should be taken when calling methods that update the RCA
// (SdSendRelativeAddr and MmcSetRelativeAddr) or change the signal voltage (SdSwitchUhsVoltage).
// These are typically not used outside the probe thread however, so generally no synchronization is
// required.
class SdmmcDevice {
 public:
  explicit SdmmcDevice(SdmmcRootDevice* root_device, zx_device_t* parent)
      : root_device_(root_device), host_(parent) {}

  // For testing using Banjo.
  explicit SdmmcDevice(const ddk::SdmmcProtocolClient& host) : host_(host) {}
  // For testing using FIDL.
  explicit SdmmcDevice(fdf::ClientEnd<fuchsia_hardware_sdmmc::Sdmmc> client_end) {
    client_.Bind(std::move(client_end), fdf::Dispatcher::GetCurrent()->get());
    using_fidl_ = true;
  }

  zx_status_t Init(bool use_fidl);

  bool using_fidl() const { return using_fidl_; }
  const sdmmc_host_info_t& host_info() const { return host_info_; }

  bool UseDma() const { return host_info_.caps & SDMMC_HOST_CAP_DMA; }

  // Update the current voltage field, e.g. after reading the card status registers.
  void SetCurrentVoltage(sdmmc_voltage_t new_voltage) { signal_voltage_ = new_voltage; }

  void SetRequestRetries(uint32_t retries) { retries_ = retries; }

  // SD/MMC shared ops
  zx_status_t SdmmcGoIdle();
  zx_status_t SdmmcSendStatus(uint32_t* status);
  zx_status_t SdmmcStopTransmission(uint32_t* status = nullptr);
  zx_status_t SdmmcWaitForState(uint32_t desired_state);
  // Retries a collection of IO requests by recursively calling itself (|retries| is used to track
  // the retry count). STOP_TRANSMISSION is issued after every attempt that results in an error, but
  // not after the request succeeds. Invokes |callback| with the final status and retry count.
  void SdmmcIoRequestWithRetries(std::vector<sdmmc_req_t> reqs,
                                 fit::function<void(zx_status_t, uint32_t)> callback,
                                 uint32_t retries = 0);

  // SD ops
  zx_status_t SdSendOpCond(uint32_t flags, uint32_t* ocr);
  zx_status_t SdSendIfCond();
  zx_status_t SdSelectCard();
  zx_status_t SdSendScr(std::array<uint8_t, 8>& scr);
  zx_status_t SdSetBusWidth(sdmmc_bus_width_t width);

  // SD/SDIO shared ops
  zx_status_t SdSwitchUhsVoltage(uint32_t ocr);
  zx_status_t SdSendRelativeAddr(uint16_t* card_status);

  // SDIO ops
  zx_status_t SdioSendOpCond(uint32_t ocr, uint32_t* rocr);
  zx_status_t SdioIoRwDirect(bool write, uint32_t fn_idx, uint32_t reg_addr, uint8_t write_byte,
                             uint8_t* read_byte);
  zx_status_t SdioIoRwExtended(uint32_t caps, bool write, uint8_t fn_idx, uint32_t reg_addr,
                               bool incr, uint32_t blk_count, uint32_t blk_size,
                               cpp20::span<const sdmmc_buffer_region_t> buffers);

  // MMC ops
  zx::result<uint32_t> MmcSendOpCond(bool suppress_error_messages);
  zx_status_t MmcWaitForReadyState(uint32_t ocr);
  zx_status_t MmcAllSendCid(std::array<uint8_t, SDMMC_CID_SIZE>& cid);
  zx_status_t MmcSetRelativeAddr(uint16_t rca);
  zx_status_t MmcSendCsd(std::array<uint8_t, SDMMC_CSD_SIZE>& csd);
  zx_status_t MmcSendExtCsd(std::array<uint8_t, MMC_EXT_CSD_SIZE>& ext_csd);
  zx_status_t MmcSelectCard();
  zx_status_t MmcSwitch(uint8_t index, uint8_t value);

  // TODO(b/299501583): Migrate these to use FIDL calls.
  // Wraps ddk::SdmmcProtocolClient methods.
  zx_status_t HostInfo(sdmmc_host_info_t* info);
  zx_status_t SetSignalVoltage(sdmmc_voltage_t voltage);
  zx_status_t SetBusWidth(sdmmc_bus_width_t bus_width);
  zx_status_t SetBusFreq(uint32_t bus_freq);
  zx_status_t SetTiming(sdmmc_timing_t timing);
  zx_status_t HwReset();
  zx_status_t PerformTuning(uint32_t cmd_idx);
  zx_status_t RegisterInBandInterrupt(void* interrupt_cb_ctx,
                                      const in_band_interrupt_protocol_ops_t* interrupt_cb_ops);
  void AckInBandInterrupt();
  zx_status_t RegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo, uint64_t offset,
                          uint64_t size, uint32_t vmo_rights);
  zx_status_t UnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo);
  zx_status_t Request(const sdmmc_req_t* req, uint32_t out_response[4]) const;

 private:
  static constexpr uint32_t kTryAttempts = 10;  // 1 initial + 9 retries.

  // Retry each request retries_ times (with wait_time delay in between) by default. Requests are
  // always tried at least once.
  zx_status_t Request(const sdmmc_req_t& req, uint32_t response[4], uint32_t retries = 0,
                      zx::duration wait_time = {}) const;
  zx_status_t RequestWithBlockRead(const sdmmc_req_t& req, uint32_t response[4],
                                   cpp20::span<uint8_t> read_data) const;
  zx_status_t SdSendAppCmd();
  // In case of IO failure, stop the transmission and wait for the card to go idle before retrying.
  void SdmmcStopForRetry();

  inline uint32_t RcaArg() const { return rca_ << 16; }

  bool using_fidl_ = false;
  const SdmmcRootDevice* const root_device_ = nullptr;
  const ddk::SdmmcProtocolClient host_;
  // The FIDL client to communicate with Sdmmc device.
  fdf::WireSharedClient<fuchsia_hardware_sdmmc::Sdmmc> client_;

  sdmmc_host_info_t host_info_ = {};
  sdmmc_voltage_t signal_voltage_ = SDMMC_VOLTAGE_V330;
  uint16_t rca_ = 0;  // APP_CMD requires the initial RCA to be zero.
  uint32_t retries_ = 0;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDMMC_DFV1_SDMMC_DEVICE_H_
