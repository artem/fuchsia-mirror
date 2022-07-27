// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_TGL_PIPE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_TGL_PIPE_H_

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/edid/edid.h>
#include <lib/fit/function.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/vmo.h>

#include <cstdint>
#include <list>
#include <unordered_map>

#include <ddktl/device.h>
#include <region-alloc/region-alloc.h>

#include "gtt.h"
#include "lib/mmio/mmio-buffer.h"
#include "power.h"
#include "registers-ddi.h"
#include "registers-pipe.h"
#include "registers-transcoder.h"

namespace i915_tgl {

class Controller;
class DisplayDevice;

class Pipe {
 public:
  Pipe(fdf::MmioBuffer* mmio_space, tgl_registers::Pipe pipe, PowerWellRef pipe_power);
  virtual ~Pipe() = default;

  Pipe(i915_tgl::Pipe&&) = delete;
  Pipe(const i915_tgl::Pipe&) = delete;

  void AttachToDisplay(uint64_t display_id, bool is_edp);
  void Detach();

  void ApplyModeConfig(const display_mode_t& mode);

  using SetupGttImageFunc = fit::function<uint64_t(const image_t* image, uint32_t rotation)>;
  void ApplyConfiguration(const display_config_t* config, const config_stamp_t* config_stamp,
                          const SetupGttImageFunc& setup_gtt_image);

  // Reset pipe registers.
  void Reset();

  // Reset pipe transcoder registers associated with the transcoder.
  void ResetActiveTranscoder();

  // A helper method to reset |pipe| given its pipe number.
  static void ResetPipe(tgl_registers::Pipe pipe, fdf::MmioBuffer* mmio_space);
  // A helper method to reset |transcoder| given its transcoder number.
  static void ResetTrans(tgl_registers::Trans trans, fdf::MmioBuffer* mmio_space);

  void LoadActiveMode(display_mode_t* mode);

  tgl_registers::Pipe pipe() const { return pipe_; }
  virtual tgl_registers::Trans transcoder() const = 0;

  uint64_t attached_display_id() const { return attached_display_; }
  bool in_use() const { return attached_display_ != INVALID_DISPLAY_ID; }

  // Display device registers only store image handles / addresses. We should
  // convert the handles to corresponding config stamps using the existing
  // mapping updated in |ApplyConfig()|.
  std::optional<config_stamp_t> GetVsyncConfigStamp(const std::vector<uint64_t>& image_handles);

 protected:
  bool attached_edp() const { return attached_edp_; }

 private:
  // Borrowed reference to Controller instance
  fdf::MmioBuffer* mmio_space_ = nullptr;

  void ConfigurePrimaryPlane(uint32_t plane_num, const primary_layer_t* primary, bool enable_csc,
                             bool* scaler_1_claimed, tgl_registers::pipe_arming_regs* regs,
                             uint64_t config_stamp_seqno, const SetupGttImageFunc& setup_gtt_image);
  void ConfigureCursorPlane(const cursor_layer_t* cursor, bool enable_csc,
                            tgl_registers::pipe_arming_regs* regs, uint64_t config_stamp_seqno);
  void SetColorConversionOffsets(bool preoffsets, const float vals[3]);

  uint64_t attached_display_ = INVALID_DISPLAY_ID;
  bool attached_edp_ = false;

  tgl_registers::Pipe pipe_;

  PowerWellRef pipe_power_;

  // For any scaled planes, this contains the (1-based) index of the active scaler
  uint32_t scaled_planes_[tgl_registers::kPipeCount][tgl_registers::kImagePlaneCount] = {};

  // On each Vsync, the driver should return the stamp of the *oldest*
  // configuration that has been fully applied to the device. We use the
  // following way to keep track of images and config stamps:
  //
  // Config stamps can be of random values (per definition in display Controller
  // banjo protocol), so while we keep all the stamps in a linked list sorted
  // chronologically, we also keep a sequence number of the first config stamp
  // in the list.
  //
  // Every time a config is applied, a new stamp will be added to the list. An
  // config stamp is removed from the list when it is older than all the current
  // config stamps used in the display layers. In this case, the front old
  // stamps will be removed and |config_stamps_front_seqno_| will be updated
  // accordingly.
  //
  // A linked list of configuration stamps in chronological order.
  // Unused configuration stamps will be evicted from the list.
  std::list<config_stamp_t> config_stamps_;

  // Consecutive sequence numbers are assigned to each configuration applied to
  // the device; this keeps track the seqno of the front (oldest configuration)
  // that is still in the linked list |config_stamps_|.
  // If no configuration has been applied to the device, it stores
  // |std::nullopt|.
  std::optional<uint64_t> config_stamps_front_seqno_;

  // The pipe registers only store the handle (address) of the images that are
  // being displayed. In order to get the config stamp for each layer and for
  // each configuration, we need to keep a mapping from *image handle* to the
  // *seqno of the configuration* so that we can know which layer has the oldest
  // configuration.
  std::unordered_map<uintptr_t, uint64_t> latest_config_seqno_of_image_;
};

class SklPipe : public Pipe {
 public:
  SklPipe(fdf::MmioBuffer* mmio_space, tgl_registers::Pipe pipe, PowerWellRef pipe_power)
      : Pipe(mmio_space, pipe, std::move(pipe_power)) {}
  ~SklPipe() override = default;

  tgl_registers::Trans transcoder() const override {
    return attached_edp() ? tgl_registers::TRANS_EDP : static_cast<tgl_registers::Trans>(pipe());
  }
};

}  // namespace i915_tgl

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_TGL_PIPE_H_
