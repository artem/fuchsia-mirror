// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_AML_SDMMC_H_
#define SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_AML_SDMMC_H_

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/driver/fidl.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/ddk/metadata.h>
#include <lib/dma-buffer/buffer.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio.h>
#include <lib/stdcompat/span.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <threads.h>

#include <array>
#include <limits>
#include <vector>

#include <fbl/auto_lock.h>
#include <soc/aml-common/aml-sdmmc.h>

#include "src/lib/vmo_store/vmo_store.h"

namespace aml_sdmmc {

class AmlSdmmc : public fdf::DriverBase,
                 public ddk::SdmmcProtocol<AmlSdmmc>,
                 public fdf::WireServer<fuchsia_hardware_sdmmc::Sdmmc> {
 public:
  // Note: This name can't be changed without migrating users in other repos.
  static constexpr char kDriverName[] = "aml-sd-emmc";

  // Limit maximum number of descriptors to 512 for now
  static constexpr size_t kMaxDmaDescriptors = 512;

  AmlSdmmc(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)),
        registered_vmos_{
            // clang-format off
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            // clang-format on
        } {}

  ~AmlSdmmc() override {
    if (irq_.is_valid()) {
      irq_.destroy();
    }
  }

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) TA_EXCL(lock_) override;

  // ddk::SdmmcProtocol implementation
  zx_status_t SdmmcHostInfo(sdmmc_host_info_t* out_info);
  zx_status_t SdmmcSetSignalVoltage(sdmmc_voltage_t voltage);
  zx_status_t SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) TA_EXCL(lock_);
  zx_status_t SdmmcSetBusFreq(uint32_t bus_freq) TA_EXCL(lock_);
  zx_status_t SdmmcSetTiming(sdmmc_timing_t timing) TA_EXCL(lock_);
  zx_status_t SdmmcHwReset() TA_EXCL(lock_);
  zx_status_t SdmmcPerformTuning(uint32_t cmd_idx) TA_EXCL(tuning_lock_);
  zx_status_t SdmmcRegisterInBandInterrupt(const in_band_interrupt_protocol_t* interrupt_cb);
  void SdmmcAckInBandInterrupt() {}
  zx_status_t SdmmcRegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo, uint64_t offset,
                               uint64_t size, uint32_t vmo_rights) TA_EXCL(lock_);
  zx_status_t SdmmcUnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo)
      TA_EXCL(lock_);
  zx_status_t SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) TA_EXCL(lock_);

  // fuchsia_hardware_sdmmc::Sdmmc implementation
  void HostInfo(fdf::Arena& arena, HostInfoCompleter::Sync& completer) override;
  void SetSignalVoltage(SetSignalVoltageRequestView request, fdf::Arena& arena,
                        SetSignalVoltageCompleter::Sync& completer) override;
  void SetBusWidth(SetBusWidthRequestView request, fdf::Arena& arena,
                   SetBusWidthCompleter::Sync& completer) override;
  void SetBusFreq(SetBusFreqRequestView request, fdf::Arena& arena,
                  SetBusFreqCompleter::Sync& completer) override;
  void SetTiming(SetTimingRequestView request, fdf::Arena& arena,
                 SetTimingCompleter::Sync& completer) override;
  void HwReset(fdf::Arena& arena, HwResetCompleter::Sync& completer) override;
  void PerformTuning(PerformTuningRequestView request, fdf::Arena& arena,
                     PerformTuningCompleter::Sync& completer) override;
  void RegisterInBandInterrupt(RegisterInBandInterruptRequestView request, fdf::Arena& arena,
                               RegisterInBandInterruptCompleter::Sync& completer) override;
  void AckInBandInterrupt(fdf::Arena& arena,
                          AckInBandInterruptCompleter::Sync& completer) override {
    // Mirroring AmlSdmmc::SdmmcAckInBandInterrupt().
  }
  void RegisterVmo(RegisterVmoRequestView request, fdf::Arena& arena,
                   RegisterVmoCompleter::Sync& completer) override;
  void UnregisterVmo(UnregisterVmoRequestView request, fdf::Arena& arena,
                     UnregisterVmoCompleter::Sync& completer) override;
  void Request(RequestRequestView request, fdf::Arena& arena,
               RequestCompleter::Sync& completer) override;

  // TODO(b/309152899): Integrate with Power Framework.
  // TODO(b/309152899): Consider not reporting an error upon client calls while power_suspended_.
  zx_status_t SuspendPower() TA_EXCL(lock_);
  zx_status_t ResumePower() TA_EXCL(lock_);

  // Visible for tests
  zx_status_t Init(const pdev_device_info_t& device_info) TA_EXCL(lock_);
  void set_board_config(const aml_sdmmc_config_t& board_config) { board_config_ = board_config; }
  bool power_suspended() TA_EXCL(lock_) {
    fbl::AutoLock lock(&lock_);
    return power_suspended_;
  }

 protected:
  virtual zx_status_t WaitForInterruptImpl();
  virtual void WaitForBus() const TA_REQ(lock_);

  // Visible for tests
  const zx::bti& bti() const { return bti_; }
  const fdf::MmioBuffer& mmio() TA_EXCL(lock_) {
    fbl::AutoLock lock(&lock_);
    return *mmio_;
  }
  void* descs_buffer() TA_EXCL(lock_) {
    fbl::AutoLock lock(&lock_);
    return descs_buffer_->virt();
  }

 private:
  constexpr static size_t kResponseCount = 4;

  struct TuneResults {
    uint64_t results = 0;

    std::string ToString(const uint32_t param_max) const {
      char string[param_max + 2];
      for (uint32_t i = 0; i <= param_max; i++) {
        string[i] = (results & (1ULL << i)) ? '|' : '-';
      }
      string[param_max + 1] = '\0';
      return string;
    }
  };

  struct TuneWindow {
    uint32_t start = 0;
    uint32_t size = 0;

    uint32_t middle() const { return start + (size / 2); }
  };

  struct TuneSettings {
    uint32_t adj_delay = 0;
    uint32_t delay = 0;
  };

  // VMO metadata that needs to be stored in accordance with the SDMMC protocol.
  struct OwnedVmoInfo {
    uint64_t offset;
    uint64_t size;
    uint32_t rights;
  };

  struct Inspect {
    inspect::Node root;
    inspect::UintProperty bus_clock_frequency;
    inspect::UintProperty adj_delay;
    inspect::UintProperty delay_lines;
    std::vector<inspect::Node> tuning_results_nodes;
    std::vector<inspect::StringProperty> tuning_results;
    inspect::UintProperty max_delay;
    inspect::UintProperty longest_window_start;
    inspect::UintProperty longest_window_size;
    inspect::UintProperty longest_window_adj_delay;
    inspect::UintProperty distance_to_failing_point;

    void Init(const pdev_device_info_t& device_info, inspect::Node& parent);
  };

  struct TuneContext {
    zx::unowned_vmo vmo;
    cpp20::span<const uint8_t> expected_block;
    uint32_t cmd;
    TuneSettings new_settings;
    TuneSettings original_settings;
  };

  using SdmmcVmoStore = vmo_store::VmoStore<vmo_store::HashTableStorage<uint32_t, OwnedVmoInfo>>;

  zx::result<> InitResources(fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev_client);

  void Serve(fdf::ServerEnd<fuchsia_hardware_sdmmc::Sdmmc> request);

  compat::DeviceServer::BanjoConfig get_banjo_config() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_SDMMC};
    config.callbacks[ZX_PROTOCOL_SDMMC] = banjo_server_.callback();
    return config;
  }

  aml_sdmmc_desc_t* descs() const TA_REQ(lock_) {
    return static_cast<aml_sdmmc_desc_t*>(descs_buffer_->virt());
  }

  zx_status_t SdmmcRequestLocked(const sdmmc_req_t* req, uint32_t out_response[4]) TA_REQ(lock_);

  uint32_t DistanceToFailingPoint(TuneSettings point,
                                  cpp20::span<const TuneResults> adj_delay_results);
  zx::result<TuneSettings> PerformTuning(cpp20::span<const TuneResults> adj_delay_results);
  zx_status_t TuningDoTransfer(const TuneContext& context) TA_REQ(tuning_lock_);
  bool TuningTestSettings(const TuneContext& context) TA_REQ(tuning_lock_);
  // Sweeps from zero to the max delay and creates a TuneWindow representing the largest span of
  // delay values that failed.
  TuneWindow GetFailingWindow(TuneResults results);
  TuneResults TuneDelayLines(const TuneContext& context) TA_REQ(tuning_lock_);

  void SetTuneSettings(const TuneSettings& settings) TA_REQ(lock_);
  TuneSettings GetTuneSettings() TA_REQ(lock_);

  void ConfigureDefaultRegs() TA_REQ(lock_);
  aml_sdmmc_desc_t* SetupCmdDesc(const sdmmc_req_t& req) TA_REQ(lock_);
  // Returns a pointer to the LAST descriptor used.
  zx::result<std::pair<aml_sdmmc_desc_t*, std::vector<fzl::PinnedVmo>>> SetupDataDescs(
      const sdmmc_req_t& req, aml_sdmmc_desc_t* cur_desc) TA_REQ(lock_);
  // These return pointers to the NEXT descriptor to use.
  zx::result<aml_sdmmc_desc_t*> SetupOwnedVmoDescs(const sdmmc_req_t& req,
                                                   const sdmmc_buffer_region_t& buffer,
                                                   vmo_store::StoredVmo<OwnedVmoInfo>& vmo,
                                                   aml_sdmmc_desc_t* cur_desc) TA_REQ(lock_);
  zx::result<std::pair<aml_sdmmc_desc_t*, fzl::PinnedVmo>> SetupUnownedVmoDescs(
      const sdmmc_req_t& req, const sdmmc_buffer_region_t& buffer, aml_sdmmc_desc_t* cur_desc)
      TA_REQ(lock_);
  zx::result<aml_sdmmc_desc_t*> PopulateDescriptors(const sdmmc_req_t& req,
                                                    aml_sdmmc_desc_t* cur_desc,
                                                    fzl::PinnedVmo::Region region) TA_REQ(lock_);
  zx_status_t FinishReq(const sdmmc_req_t& req);

  void ClearStatus() TA_REQ(lock_);
  zx::result<std::array<uint32_t, kResponseCount>> WaitForInterrupt(const sdmmc_req_t& req)
      TA_REQ(lock_);

  std::optional<fdf::MmioBuffer> mmio_ TA_GUARDED(lock_);

  zx::bti bti_;

  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> reset_gpio_;
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> clock_gate_;
  zx::interrupt irq_;
  aml_sdmmc_config_t board_config_;

  sdmmc_host_info_t dev_info_;
  std::unique_ptr<dma_buffer::ContiguousBuffer> descs_buffer_ TA_GUARDED(lock_);
  uint32_t max_freq_, min_freq_;
  bool power_suspended_ TA_GUARDED(lock_) = false;
  uint32_t clk_div_saved_ = 0;

  // TODO(https://fxbug.dev/42084501): Remove redundant locking when Banjo is removed.
  fbl::Mutex lock_ TA_ACQ_AFTER(tuning_lock_);
  fbl::Mutex tuning_lock_ TA_ACQ_BEFORE(lock_);
  bool shutdown_ TA_GUARDED(lock_) = false;
  std::array<SdmmcVmoStore, SDMMC_MAX_CLIENT_ID + 1> registered_vmos_ TA_GUARDED(lock_);

  uint64_t consecutive_cmd_errors_ = 0;
  uint64_t consecutive_data_errors_ = 0;

  Inspect inspect_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  compat::BanjoServer banjo_server_{ZX_PROTOCOL_SDMMC, this, &sdmmc_protocol_ops_};
  compat::SyncInitializedDeviceServer compat_server_;

  // Dedicated dispatcher for inlining fuchsia_hardware_sdmmc::Sdmmc FIDL requests.
  fdf::Dispatcher worker_dispatcher_;
};

}  // namespace aml_sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_AML_SDMMC_H_
