// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <fuchsia/hardware/spiimpl/cpp/banjo.h>
#include <lib/async/cpp/executor.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fpromise/promise.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/mmio/mmio.h>
#include <lib/stdcompat/span.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/profile.h>
#include <lib/zx/result.h>

#include <optional>

#include <fbl/array.h>
#include <fbl/mutex.h>
#include <soc/aml-common/aml-spi.h>

#include "sdk/lib/driver/compat/cpp/device_server.h"
#include "src/lib/vmo_store/vmo_store.h"

namespace spi {

class AmlSpi : public ddk::SpiImplProtocol<AmlSpi> {
 public:
  struct OwnedVmoInfo {
    uint64_t offset;
    uint64_t size;
    uint32_t rights;
  };

  using SpiVmoStore = vmo_store::VmoStore<vmo_store::HashTableStorage<uint32_t, OwnedVmoInfo>>;

  struct ChipInfo {
    ChipInfo() : registered_vmos(vmo_store::Options{}) {}
    ~ChipInfo() = default;

    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio;
    std::optional<SpiVmoStore> registered_vmos;
  };

  // DmaBuffer holds a contiguous VMO that is both pinned and mapped.
  struct DmaBuffer {
    static zx_status_t Create(const zx::bti& bti, size_t size, DmaBuffer* out_dma_buffer);

    zx::vmo vmo;
    fzl::PinnedVmo pinned;
    fzl::VmoMapper mapped;
  };

  AmlSpi(fdf::MmioBuffer mmio, fidl::ClientEnd<fuchsia_hardware_registers::Device> reset,
         fidl::ClientEnd<fuchsia_scheduler::ProfileProvider> profile_provider, uint32_t reset_mask,
         fbl::Array<ChipInfo> chips, zx::interrupt interrupt,
         const amlogic_spi::amlspi_config_t& config, zx::bti bti, DmaBuffer tx_buffer,
         DmaBuffer rx_buffer)
      : mmio_(std::move(mmio)),
        reset_(std::move(reset)),
        dispatcher_(fdf::Dispatcher::GetCurrent()),
        profile_provider_(std::move(profile_provider), dispatcher_->async_dispatcher()),
        reset_mask_(reset_mask),
        chips_(std::move(chips)),
        interrupt_(std::move(interrupt)),
        config_(config),
        bti_(std::move(bti)),
        tx_buffer_(std::move(tx_buffer)),
        rx_buffer_(std::move(rx_buffer)) {}

  spi_impl_protocol_ops_t* ops() { return &spi_impl_protocol_ops_; }

  void InitRegisters();

  void Shutdown();

  uint32_t SpiImplGetChipSelectCount() { return static_cast<uint32_t>(chips_.size()); }
  zx_status_t SpiImplExchange(uint32_t cs, const uint8_t* txdata, size_t txdata_size,
                              uint8_t* out_rxdata, size_t rxdata_size, size_t* out_rxdata_actual);

  zx_status_t SpiImplRegisterVmo(uint32_t chip_select, uint32_t vmo_id, zx::vmo vmo,
                                 uint64_t offset, uint64_t size, uint32_t rights);
  zx_status_t SpiImplUnregisterVmo(uint32_t chip_select, uint32_t vmo_id, zx::vmo* out_vmo);
  void SpiImplReleaseRegisteredVmos(uint32_t chip_select);
  zx_status_t SpiImplTransmitVmo(uint32_t chip_select, uint32_t vmo_id, uint64_t offset,
                                 uint64_t size);
  zx_status_t SpiImplReceiveVmo(uint32_t chip_select, uint32_t vmo_id, uint64_t offset,
                                uint64_t size);
  zx_status_t SpiImplExchangeVmo(uint32_t chip_select, uint32_t tx_vmo_id, uint64_t tx_offset,
                                 uint32_t rx_vmo_id, uint64_t rx_offset, uint64_t size);

  zx_status_t SpiImplLockBus(uint32_t chip_select) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t SpiImplUnlockBus(uint32_t chip_select) { return ZX_ERR_NOT_SUPPORTED; }

 private:
  void DumpState() TA_REQ(bus_lock_);

  void Exchange8(const uint8_t* txdata, uint8_t* out_rxdata, size_t size) TA_REQ(bus_lock_);
  void Exchange64(const uint8_t* txdata, uint8_t* out_rxdata, size_t size) TA_REQ(bus_lock_);

  void SetThreadProfile();

  void WaitForTransferComplete() TA_REQ(bus_lock_);
  void WaitForDmaTransferComplete() TA_REQ(bus_lock_);

  void InitRegistersLocked() TA_REQ(bus_lock_);

  // Checks size against the registered VMO size and returns a Span with offset applied. Returns a
  // Span with data set to nullptr if vmo_id wasn't found. Returns a Span with size set to zero if
  // offset and/or size are invalid.
  zx::result<cpp20::span<uint8_t>> GetVmoSpan(uint32_t chip_select, uint32_t vmo_id,
                                              uint64_t offset, uint64_t size, uint32_t right)
      TA_REQ(vmo_lock_);

  zx_status_t ExchangeDma(const uint8_t* txdata, uint8_t* out_rxdata, uint64_t size)
      TA_REQ(bus_lock_);

  size_t DoDmaTransfer(size_t words_remaining) TA_REQ(bus_lock_);

  bool UseDma(size_t size) const TA_REQ(bus_lock_);

  // Shims to support thread annotations on ChipInfo members.
  const fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>& gpio(uint32_t chip_select)
      TA_REQ(bus_lock_) {
    return chips_[chip_select].gpio;
  }

  std::optional<SpiVmoStore>& registered_vmos(uint32_t chip_select) TA_REQ(vmo_lock_) {
    return chips_[chip_select].registered_vmos;
  }

  fdf::MmioBuffer mmio_ TA_GUARDED(bus_lock_);
  fidl::WireSyncClient<fuchsia_hardware_registers::Device> reset_;
  fdf::UnownedDispatcher dispatcher_;
  fidl::WireClient<fuchsia_scheduler::ProfileProvider> profile_provider_;
  const uint32_t reset_mask_;
  const fbl::Array<ChipInfo> chips_;
  bool need_reset_ TA_GUARDED(bus_lock_) = false;
  zx::interrupt interrupt_;
  const amlogic_spi::amlspi_config_t config_;
  bool apply_scheduler_role_ = true;
  // Protects mmio_, need_reset_, and the DMA buffers.
  fbl::Mutex bus_lock_;
  // Protects registered_vmos members of chips_.
  fbl::Mutex vmo_lock_;
  zx::bti bti_;
  DmaBuffer tx_buffer_ TA_GUARDED(bus_lock_);
  DmaBuffer rx_buffer_ TA_GUARDED(bus_lock_);
  bool shutdown_ TA_GUARDED(bus_lock_) = false;
};

// AmlSpiDriver is a helper class that is responsible for acquiring resources on behalf of AmlSpi so
// that it can support RAII in DFv2. It implements the driver Start hook, and forwards Stop to the
// AmlSpi instance.
class AmlSpiDriver : public fdf::DriverBase {
 public:
  AmlSpiDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("aml-spi", std::move(start_args), std::move(dispatcher)),
        executor_(fdf::DriverBase::dispatcher()) {}

  void Start(fdf::StartCompleter completer) override;
  void Stop() override {
    if (device_) {
      device_->Shutdown();
    }
  }

 protected:
  // MapMmio can be overridden by a test in order to provide an fdf::MmioBuffer backed by a fake.
  virtual fpromise::promise<fdf::MmioBuffer, zx_status_t> MapMmio(
      fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id);

 private:
  void OnCompatServerInitialized(fdf::StartCompleter completer);
  void AddNode(fdf::MmioBuffer mmio, const amlogic_spi::amlspi_config_t& config,
               zx::interrupt interrupt, zx::bti bti, fdf::StartCompleter completer);

  fpromise::promise<amlogic_spi::amlspi_config_t, zx_status_t> GetConfig();
  fpromise::promise<zx::interrupt, zx_status_t> GetInterrupt();
  fpromise::promise<zx::bti, zx_status_t> GetBti();

  fbl::Array<AmlSpi::ChipInfo> InitChips(const amlogic_spi::amlspi_config_t& config);
  zx::result<compat::DeviceServer::GenericProtocol> GetBanjoProto(compat::BanjoProtoId id);

  fidl::WireClient<fuchsia_driver_framework::Node> parent_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> controller_;
  fidl::WireClient<fuchsia_hardware_platform_device::Device> pdev_;
  fidl::WireClient<fuchsia_driver_compat::Device> compat_;
  compat::AsyncInitializedDeviceServer compat_server_;
  std::unique_ptr<AmlSpi> device_;
  async::Executor executor_;
};

}  // namespace spi
