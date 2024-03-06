// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_DSI_DSI_HOST_CONTROLLER_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_DSI_DSI_HOST_CONTROLLER_H_

#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <zircon/types.h>

#include <fbl/mutex.h>

namespace designware_dsi {

// The DesignWare Cores MIPI-DSI host controller IP core (also known as
// DWC_mipi_dsi_host).
class DsiHostController {
 public:
  explicit DsiHostController(fdf::MmioBuffer dsi_mmio);

  zx_status_t Config(const dsi_config_t* dsi_config, int64_t dphy_data_lane_bits_per_second);
  void PowerUp();
  void PowerDown();
  void SetMode(dsi_mode_t mode);
  zx_status_t SendCmd(const mipi_dsi_cmd_t* cmd_list, size_t cmd_count);
  void PhyPowerUp();
  void PhyPowerDown();
  void PhySendCode(uint32_t code, uint32_t parameter);
  zx_status_t PhyWaitForReady();

 private:
  inline bool IsPldREmpty() TA_REQ(command_lock_);
  inline bool IsPldRFull() TA_REQ(command_lock_);
  inline bool IsPldWEmpty() TA_REQ(command_lock_);
  inline bool IsPldWFull() TA_REQ(command_lock_);
  inline bool IsCmdEmpty() TA_REQ(command_lock_);
  inline bool IsCmdFull() TA_REQ(command_lock_);
  zx_status_t WaitforFifo(uint32_t bit, bool val) TA_REQ(command_lock_);
  zx_status_t WaitforPldWNotFull() TA_REQ(command_lock_);
  zx_status_t WaitforPldWEmpty() TA_REQ(command_lock_);
  zx_status_t WaitforPldRFull() TA_REQ(command_lock_);
  zx_status_t WaitforPldRNotEmpty() TA_REQ(command_lock_);
  zx_status_t WaitforCmdNotFull() TA_REQ(command_lock_);
  zx_status_t WaitforCmdEmpty() TA_REQ(command_lock_);
  void DumpCmd(const mipi_dsi_cmd_t& cmd);
  zx_status_t GenericPayloadRead(uint32_t* data) TA_REQ(command_lock_);
  zx_status_t GenericHdrWrite(uint32_t data) TA_REQ(command_lock_);
  zx_status_t GenericPayloadWrite(uint32_t data) TA_REQ(command_lock_);
  void EnableBta() TA_REQ(command_lock_);
  void DisableBta() TA_REQ(command_lock_);
  zx_status_t WaitforBtaAck() TA_REQ(command_lock_);
  zx_status_t GenWriteShort(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t DcsWriteShort(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t GenWriteLong(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t GenRead(const mipi_dsi_cmd_t& cmd) TA_REQ(command_lock_);
  zx_status_t SendCommand(const mipi_dsi_cmd_t& cmd);
  zx_status_t GetColorCode(color_code_t c, bool& packed, uint8_t& code);
  zx_status_t GetVideoMode(video_mode_t v, uint8_t& mode);

  fdf::MmioBuffer dsi_mmio_;

  fbl::Mutex command_lock_;
};

}  // namespace designware_dsi

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_DSI_DSI_HOST_CONTROLLER_H_
