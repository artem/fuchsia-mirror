// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-hdmi/aml-hdmi.h"

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fidl/epitaph.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/resource.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/syscalls/smc.h>

#include <cinttypes>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/aml-hdmi/top-regs.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/designware/color-param.h"
#include "src/graphics/display/lib/designware/hdmi-transmitter-controller-impl.h"
#include "src/graphics/display/lib/designware/hdmi-transmitter-controller.h"

#define HDMI_ASPECT_RATIO_NONE 0
#define HDMI_ASPECT_RATIO_4x3 1
#define HDMI_ASPECT_RATIO_16x9 2

#define HDMI_COLORIMETRY_ITU601 1
#define HDMI_COLORIMETRY_ITU709 2

#define DWC_OFFSET_MASK (0x10UL << 24)

namespace aml_hdmi {

void AmlHdmiDevice::WriteTopLevelReg(uint32_t addr, uint32_t val) {
  hdmitx_top_level_mmio_->Write32(val, addr);
}

uint32_t AmlHdmiDevice::ReadTopLevelReg(uint32_t addr) {
  return hdmitx_top_level_mmio_->Read32(addr);
}

zx_status_t AmlHdmiDevice::Bind() {
  // TODO(fxbug.dev/132267): Use the builder / factory pattern instead
  // of multiple stateful steps (such as Create() and Bind()) when
  // initializing the device.

  if (!pdev_.is_valid()) {
    zxlogf(ERROR, "Could not get ZX_PROTOCOL_PDEV protocol");
    return ZX_ERR_NO_RESOURCES;
  }

  // Map registers
  static constexpr uint32_t kHdmitxControllerIpIndex = 0;
  std::optional<fdf::MmioBuffer> hdmitx_controller_ip_mmio;
  zx_status_t status = pdev_.MapMmio(kHdmitxControllerIpIndex, &hdmitx_controller_ip_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map MMIO HDMITX Controller IP registers: %s",
           zx_status_get_string(status));
    return status;
  }
  {
    fbl::AutoLock dw_lock(&dw_lock_);
    hdmi_dw_ = std::make_unique<designware_hdmi::HdmiTransmitterControllerImpl>(
        std::move(*hdmitx_controller_ip_mmio));
  }

  static constexpr uint32_t kHdmitxTopLevelIndex = 1;
  status = pdev_.MapMmio(kHdmitxTopLevelIndex, &hdmitx_top_level_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map MMIO HDMITX top-level registers: %s",
           zx_status_get_string(status));
    return status;
  }

  static constexpr uint32_t kSiliconProviderSmcIndex = 0;
  status = pdev_.GetSmc(kSiliconProviderSmcIndex, &smc_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not get secure monitor call (SMC) resource: %s",
           zx_status_get_string(status));
    return status;
  }

  async_dispatcher_t* dispatcher =
      fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher());
  outgoing_ = component::OutgoingDirectory(dispatcher);

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  fuchsia_hardware_hdmi::Service::InstanceHandler handler({
      .device = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
  });
  zx::result<> result = outgoing_->AddService<fuchsia_hardware_hdmi::Service>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  result = outgoing_->Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  std::array offers = {
      fuchsia_hardware_hdmi::Service::Name,
  };

  status = DdkAdd(ddk::DeviceAddArgs("aml-hdmi")
                      .set_fidl_service_offers(offers)
                      .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

void AmlHdmiDevice::Reset(ResetRequestView request, ResetCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
  // TODO(fxb/69679): Add in Resets
  // reset hdmi related blocks (HIU, HDMI SYS, HDMI_TX)
  // auto reset0_result = display->reset_register_.WriteRegister32(PRESET0_REGISTER, 1 << 19, 1 <<
  // 19); if ((reset0_result.status() != ZX_OK) || reset0_result->is_error()) {
  //   zxlogf(ERROR, "Reset0 Write failed\n");
  // }

  /* FIXME: This will reset the entire HDMI subsystem including the HDCP engine.
   * At this point, we have no way of initializing HDCP block, so we need to
   * skip this for now.
   */
  // auto reset2_result = display->reset_register_.WriteRegister32(PRESET2_REGISTER, 1 << 15, 1 <<
  // 15); // Will mess up hdcp stuff if ((reset2_result.status() != ZX_OK) ||
  // reset2_result->is_error()) {
  //   zxlogf(ERROR, "Reset2 Write failed\n");
  // }

  // auto reset2_result = display->reset_register_.WriteRegister32(PRESET2_REGISTER, 1 << 2, 1 <<
  // 2); if ((reset2_result.status() != ZX_OK) || reset2_result->is_error()) {
  //   zxlogf(ERROR, "Reset2 Write failed\n");
  // }

  // Bring HDMI out of reset
  WriteTopLevelReg(HDMITX_TOP_SW_RESET, 0);
  usleep(200);
  WriteTopLevelReg(HDMITX_TOP_CLK_CNTL, 0x000000ff);

  fbl::AutoLock lock(&dw_lock_);
  auto status = hdmi_dw_->InitHw();
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void CalculateTxParam(const display::DisplayTiming& display_timing,
                      designware_hdmi::hdmi_param_tx* p) {
  p->is4K = display_timing.pixel_clock_frequency_khz > 500'000;

  if (display_timing.horizontal_active_px * 3 == display_timing.vertical_active_lines * 4) {
    p->aspect_ratio = HDMI_ASPECT_RATIO_4x3;
  } else if (display_timing.horizontal_active_px * 9 == display_timing.vertical_active_lines * 16) {
    p->aspect_ratio = HDMI_ASPECT_RATIO_16x9;
  } else {
    p->aspect_ratio = HDMI_ASPECT_RATIO_NONE;
  }

  p->colorimetry = HDMI_COLORIMETRY_ITU601;
}

zx_status_t AmlHdmiDevice::InitializeHdcp14() {
  ZX_DEBUG_ASSERT(smc_.is_valid());

  static constexpr zx_smc_parameters_t params = {
      // Silicon Provider secure monitor call: "HDCP14_INIT".
      .func_id = 0x82000012,
  };
  zx_smc_result_t result = {};
  zx_status_t status = zx_smc_call(smc_.get(), &params, &result);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize HDCP 1.4: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

void AmlHdmiDevice::ModeSet(ModeSetRequestView request, ModeSetCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
  ZX_DEBUG_ASSERT(request->mode.has_color());
  ZX_DEBUG_ASSERT(request->mode.has_mode());
  const display::DisplayTiming display_timing = display::ToDisplayTiming(request->mode.mode());
  const designware_hdmi::ColorParam color_param =
      designware_hdmi::ToColorParam(request->mode.color());

  designware_hdmi::hdmi_param_tx p;
  CalculateTxParam(display_timing, &p);

  // Output normal TMDS Data
  WriteTopLevelReg(HDMITX_TOP_BIST_CNTL, 1 << 12);

  // Configure HDMI TX IP
  fbl::AutoLock lock(&dw_lock_);
  hdmi_dw_->ConfigHdmitx(color_param, display_timing, p);

  // Initialize HDCP 1.4.
  //
  // AMLogic-provided bringup code initializes HDCP before clearing
  // interrupts on the DesignWare HDMI IP's. Following the same sequence
  // would be difficult given our current layering, as we clear interrupts
  // in HdmiTransmitterController::ConfigHdmitx().
  //
  // Fortunately, experiments on VIM3 (using A311D) show that the HDCP
  // initialization SMC still works if invoked after the interrupts are
  // cleared.
  if (smc_.is_valid()) {
    InitializeHdcp14();
  } else {
    // TODO(fxbug.dev/123426): This could occur in tests where fake SMC
    // resource objects are not yet supported. Once fake SMC is supported, we
    // should enforce `smc_` to be always valid and always issue a
    // `zx_smc_call()` syscall.
    zxlogf(WARNING,
           "Secure monitor call (SMC) resource is not available. "
           "Skipping initializing HDCP 1.4.");
  }

  WriteTopLevelReg(HDMITX_TOP_INTR_STAT_CLR, 0x0000001f);
  hdmi_dw_->SetupInterrupts();
  WriteTopLevelReg(HDMITX_TOP_INTR_MASKN, 0x9f);
  hdmi_dw_->Reset();

  if (p.is4K) {
    // Setup TMDS Clocks (taken from recommended test pattern in DVI spec)
    WriteTopLevelReg(HDMITX_TOP_TMDS_CLK_PTTN_01, 0);
    WriteTopLevelReg(HDMITX_TOP_TMDS_CLK_PTTN_23, 0x03ff03ff);
  } else {
    WriteTopLevelReg(HDMITX_TOP_TMDS_CLK_PTTN_01, 0x001f001f);
    WriteTopLevelReg(HDMITX_TOP_TMDS_CLK_PTTN_23, 0x001f001f);
  }
  hdmi_dw_->SetFcScramblerCtrl(p.is4K);

  WriteTopLevelReg(HDMITX_TOP_TMDS_CLK_PTTN_CNTL, 0x1);
  usleep(2);
  WriteTopLevelReg(HDMITX_TOP_TMDS_CLK_PTTN_CNTL, 0x2);

  hdmi_dw_->SetupScdc(p.is4K);
  hdmi_dw_->ResetFc();

  completer.ReplySuccess();
}

void AmlHdmiDevice::EdidTransfer(EdidTransferRequestView request,
                                 EdidTransferCompleter::Sync& completer) {
  if (request->ops.count() < 1 || request->ops.count() > I2C_IMPL_MAX_RW_OPS) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  fbl::AllocChecker ac;
  fbl::Array<uint8_t> read_buffer(new (&ac) uint8_t[I2C_IMPL_MAX_TOTAL_TRANSFER],
                                  I2C_IMPL_MAX_TOTAL_TRANSFER);
  if (!ac.check()) {
    zxlogf(ERROR, "%s could not allocate read_buffer", __FUNCTION__);
    completer.ReplyError(ZX_ERR_INTERNAL);
    return;
  }
  fbl::Array<uint8_t> write_buffer(new (&ac) uint8_t[I2C_IMPL_MAX_TOTAL_TRANSFER],
                                   I2C_IMPL_MAX_TOTAL_TRANSFER);
  if (!ac.check()) {
    zxlogf(ERROR, "%s could not allocate write_buffer", __FUNCTION__);
    completer.ReplyError(ZX_ERR_INTERNAL);
    return;
  }

  i2c_impl_op_t op_list[I2C_IMPL_MAX_RW_OPS];
  size_t write_cnt = 0;
  size_t read_cnt = 0;
  uint8_t* p_writes = write_buffer.data();
  uint8_t* p_reads = read_buffer.data();
  for (size_t i = 0; i < request->ops.count(); ++i) {
    if (request->ops[i].is_write) {
      if (write_cnt >= request->write_segments_data.count()) {
        completer.ReplyError(ZX_ERR_INVALID_ARGS);
        return;
      }
      op_list[i].address = request->ops[i].address;
      memcpy(p_writes, request->write_segments_data[write_cnt].data(),
             request->write_segments_data[write_cnt].count());
      op_list[i].data_buffer = p_writes;
      op_list[i].data_size = request->write_segments_data[write_cnt].count();
      op_list[i].is_read = false;
      op_list[i].stop = false;
      p_writes += request->write_segments_data[write_cnt].count();
      write_cnt++;
    } else {
      if (read_cnt >= request->read_segments_length.count()) {
        completer.ReplyError(ZX_ERR_INVALID_ARGS);
        return;
      }
      op_list[i].address = request->ops[i].address;
      op_list[i].data_buffer = p_reads;
      op_list[i].data_size = request->read_segments_length[read_cnt];
      op_list[i].is_read = true;
      op_list[i].stop = false;
      p_reads += request->read_segments_length[read_cnt];
      read_cnt++;
    }
  }
  op_list[request->ops.count() - 1].stop = true;

  if (request->write_segments_data.count() != write_cnt) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (request->read_segments_length.count() != read_cnt) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  fbl::AutoLock lock(&dw_lock_);
  auto status = hdmi_dw_->EdidTransfer(op_list, request->ops.count());

  if (status == ZX_OK) {
    fidl::Arena allocator;
    fidl::VectorView<fidl::VectorView<uint8_t>> reads(allocator, read_cnt);
    size_t read_ops_cnt = 0;
    for (size_t i = 0; i < request->ops.count(); ++i) {
      if (!op_list[i].is_read) {
        continue;
      }
      reads[read_ops_cnt] = fidl::VectorView<uint8_t>::FromExternal(
          const_cast<uint8_t*>(op_list[i].data_buffer), op_list[i].data_size);
      read_ops_cnt++;
    }
    completer.ReplySuccess(std::move(reads));
  } else {
    completer.ReplyError(status);
  }
}

void AmlHdmiDevice::PrintRegister(const char* register_name, uint32_t register_address) {
  zxlogf(INFO, "%s (0x%04" PRIx32 "): %" PRIu32, register_name, register_address,
         ReadTopLevelReg(register_address));
}

void AmlHdmiDevice::PrintHdmiRegisters(PrintHdmiRegistersCompleter::Sync& completer) {
  zxlogf(INFO, "------------Top Registers------------");
  PrintRegister("HDMITX_TOP_SW_RESET", HDMITX_TOP_SW_RESET);
  PrintRegister("HDMITX_TOP_CLK_CNTL", HDMITX_TOP_CLK_CNTL);
  PrintRegister("HDMITX_TOP_INTR_MASKN", HDMITX_TOP_INTR_MASKN);
  PrintRegister("HDMITX_TOP_INTR_STAT_CLR", HDMITX_TOP_INTR_STAT_CLR);
  PrintRegister("HDMITX_TOP_BIST_CNTL", HDMITX_TOP_BIST_CNTL);
  PrintRegister("HDMITX_TOP_TMDS_CLK_PTTN_01", HDMITX_TOP_TMDS_CLK_PTTN_01);
  PrintRegister("HDMITX_TOP_TMDS_CLK_PTTN_23", HDMITX_TOP_TMDS_CLK_PTTN_23);
  PrintRegister("HDMITX_TOP_TMDS_CLK_PTTN_CNTL", HDMITX_TOP_TMDS_CLK_PTTN_CNTL);

  fbl::AutoLock lock(&dw_lock_);
  hdmi_dw_->PrintRegisters();

  completer.Reply();
}

// static
zx_status_t AmlHdmiDevice::Create(zx_device_t* parent) {
  fbl::AllocChecker alloc_checker;
  auto dev = fbl::make_unique_checked<aml_hdmi::AmlHdmiDevice>(&alloc_checker, parent);
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  auto status = dev->Bind();
  if (status == ZX_OK) {
    // devmgr now owns the memory for `dev`.
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

AmlHdmiDevice::AmlHdmiDevice(zx_device_t* parent)
    : DeviceType(parent), pdev_(parent), loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

AmlHdmiDevice::AmlHdmiDevice(zx_device_t* parent, fdf::MmioBuffer hdmitx_top_level_mmio,
                             std::unique_ptr<designware_hdmi::HdmiTransmitterController> hdmi_dw,
                             zx::resource smc)
    : DeviceType(parent),
      pdev_(parent),
      hdmi_dw_(std::move(hdmi_dw)),
      smc_(std::move(smc)),
      hdmitx_top_level_mmio_(std::move(hdmitx_top_level_mmio)),
      loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
  ZX_DEBUG_ASSERT(hdmi_dw_);
}

AmlHdmiDevice::~AmlHdmiDevice() = default;

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return AmlHdmiDevice::Create(parent); },
};

}  // namespace

}  // namespace aml_hdmi

ZIRCON_DRIVER(aml_hdmi, aml_hdmi::kDriverOps, "zircon", "0.1");
