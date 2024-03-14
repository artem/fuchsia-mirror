// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_AMLOGIC_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_AMLOGIC_DISPLAY_H_

#include <fidl/fuchsia.hardware.amlogiccanvas/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/interrupt.h>
#include <unistd.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <memory>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/amlogic-display/capture.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/hot-plug-detection.h"
#include "src/graphics/display/drivers/amlogic-display/video-input-unit.h"
#include "src/graphics/display/drivers/amlogic-display/vout.h"
#include "src/graphics/display/drivers/amlogic-display/vpu.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

namespace amlogic_display {

struct ImageInfo : public fbl::DoublyLinkedListable<std::unique_ptr<ImageInfo>> {
  ~ImageInfo() {
    zxlogf(INFO, "Destroying image on canvas %d", canvas_idx);
    if (canvas.has_value()) {
      fidl::WireResult result = fidl::WireCall(canvas.value())->Free(canvas_idx);
      if (!result.ok()) {
        zxlogf(WARNING, "Failed to call Canvas Free: %s",
               result.error().FormatDescription().c_str());
      } else if (result->is_error()) {
        zxlogf(WARNING, "Canvas Free failed: %s", zx_status_get_string(result->error_value()));
      }
    }
    if (pmt) {
      pmt.unpin();
    }
  }
  std::optional<fidl::UnownedClientEnd<fuchsia_hardware_amlogiccanvas::Device>> canvas;
  uint8_t canvas_idx;
  uint32_t image_height;
  uint32_t image_width;

  PixelFormatAndModifier pixel_format;
  bool is_afbc;
  zx::pmt pmt;
  zx_paddr_t paddr;
};

class AmlogicDisplay;

// AmlogicDisplay will implement only a few subset of Device.
using DeviceType = ddk::Device<AmlogicDisplay, ddk::GetProtocolable, ddk::Suspendable,
                               ddk::Resumable, ddk::ChildPreReleaseable>;
class AmlogicDisplay
    : public DeviceType,
      public ddk::DisplayControllerImplProtocol<AmlogicDisplay, ddk::base_protocol> {
 public:
  // Factory method used by the device manager glue code.
  static zx_status_t Create(zx_device_t* parent);

  explicit AmlogicDisplay(zx_device_t* parent);

  AmlogicDisplay(const AmlogicDisplay&) = delete;
  AmlogicDisplay(AmlogicDisplay&&) = delete;
  AmlogicDisplay& operator=(const AmlogicDisplay&) = delete;
  AmlogicDisplay& operator=(AmlogicDisplay&&) = delete;

  ~AmlogicDisplay();

  // Acquires parent resources, sets up display submodules, and binds itself to
  // the device node.
  //
  // Must be called once and only once by the device manager (via the Create()
  // factory method) during the driver lifetime.
  zx_status_t Bind();

  // Required functions needed to implement Display Controller Protocol
  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* intf);
  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplImportImage(const image_metadata_t* image_metadata,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index, uint64_t* out_image_handle);
  void DisplayControllerImplReleaseImage(uint64_t image_handle);
  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t** display_configs, size_t display_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual);
  void DisplayControllerImplApplyConfiguration(const display_config_t** display_config,
                                               size_t display_count,
                                               const config_stamp_t* banjo_config_stamp);
  void DisplayControllerImplSetEld(uint64_t display_id, const uint8_t* raw_eld_list,
                                   size_t raw_eld_count) {}  // No ELD required for non-HDA systems.
  zx_status_t DisplayControllerImplGetSysmemConnection(zx::channel connection);
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on);

  zx_status_t DisplayControllerImplSetDisplayCaptureInterface(
      const display_capture_interface_protocol_t* intf);
  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle);
  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle);
  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle);
  bool DisplayControllerImplIsCaptureCompleted() __TA_EXCLUDES(capture_mutex_);

  zx_status_t DisplayControllerImplSetMinimumRgb(uint8_t minimum_rgb);

  // Required functions for DeviceType
  void DdkSuspend(ddk::SuspendTxn txn);
  void DdkResume(ddk::ResumeTxn txn);
  void DdkRelease();
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_protocol);
  void DdkChildPreRelease(void* child_ctx) {
    fbl::AutoLock lock(&display_mutex_);
    dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient();
  }

  void Dump() { vout_->Dump(); }

  void SetFormatSupportCheck(fit::function<bool(fuchsia_images2::wire::PixelFormat)> fn) {
    format_support_check_ = std::move(fn);
  }

  void SetCanvasForTesting(fidl::ClientEnd<fuchsia_hardware_amlogiccanvas::Device> canvas) {
    canvas_.Bind(std::move(canvas));
  }

  void SetVoutForTesting(std::unique_ptr<Vout> vout) { vout_ = std::move(vout); }

  void SetVideoInputUnitForTesting(std::unique_ptr<VideoInputUnit> video_input_unit) {
    video_input_unit_ = std::move(video_input_unit);
  }

  void SetSysmemAllocatorForTesting(
      fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client) {
    sysmem_ = std::move(sysmem_allocator_client);
  }

 private:
  void VSyncThreadEntryPoint();
  void CaptureThreadEntryPoint();
  void PopulatePanelType() TA_REQ(display_mutex_);

  void OnHotPlugStateChange(HotPlugDetectionState current_state);
  void OnCaptureComplete();

  // TODO(https://fxbug.dev/42082357): Currently, AmlogicDisplay has a multi-step
  // initialization procedure when the device manager binds the driver to the
  // device node. This makes the initialization stateful and hard to maintain
  // (submodules may have dependencies on initialization). Instead, it's
  // preferred to turn the initialization procedure into a builder pattern,
  // where resources are acquired before the device is created, and the device
  // is guaranteed to be ready at creation time.

  // Acquires the common protocols (pdev, sysmem and amlogic-canvas) and
  // resources (Vsync interrupts, capture interrupts and BTIs) from the parent
  // nodes.
  //
  // Must be called once and only once per driver binding procedure, before
  // the protocols / resources mentioned above being used.
  zx_status_t GetCommonProtocolsAndResources();

  // Must be called once and only once per driver binding procedure.
  //
  // `GetCommonProtocolsAndResources()` must be called to acquire sysmem
  // protocol before this is called.
  zx_status_t InitializeSysmemAllocator();

  // Allocates and initializes the video output (Vout) hardware submodule of
  // the Video Processing Unit. The type of display (MIPI-DSI or HDMI) is based
  // on the panel metadata provided by the board driver.
  //
  // Must be called once and only once per driver binding procedure.
  // `vout_` must not be allocated nor initialized.
  zx_status_t InitializeVout();

  // A helper allocating and initializing the video output (Vout) hardware
  // submodule for MIPI-DSI displays.
  //
  // `vout_` must not be allocated nor initialized.
  zx_status_t InitializeMipiDsiVout(display_panel_t panel_info);

  // A helper allocating and initializing the video output (Vout) hardware
  // submodule for HDMI displays.
  //
  // `vout_` must not be allocated nor initialized.
  zx_status_t InitializeHdmiVout();

  // Starts the Vsync interrupt handler thread.
  //
  // Must be called once and only once per driver binding procedure.
  //  `GetCommonProtocolsAndResources()` must be called to acquire the Vsync
  // interrupt before this is called.
  zx_status_t StartVsyncInterruptHandlerThread();

  // Acquires the hotplug display detection hardware resources (GPIO protocol
  // and interrupt for the hotplug detection GPIO pin) from parent nodes, and
  // starts the interrupt handler thread.
  //
  // Must be called once and only once per driver binding procedure, if display
  // hotplug is supported.
  zx_status_t SetupHotplugDisplayDetection();

  // Power cycles the display engine, resets all configurations and re-brings up
  // the video output module. This will cause all the previous display hardware
  // state to be lost.
  //
  // Must only be called during `AmlogicDisplay` initialization, after `vpu_`
  // and `vout_` are initialized, but before any IRQ handler thread starts
  // running, as access to display resources (registers, IRQs) after powering
  // off the display engine will cause the system to crash.
  zx::result<> ResetDisplayEngine() TA_REQ(display_mutex_);

  bool fully_initialized() const { return full_init_done_.load(std::memory_order_relaxed); }
  void set_fully_initialized() { full_init_done_.store(true, std::memory_order_release); }

  // If true, the driver ignores the `mode` field in the
  // `fuchsia.hardware.display.controller/DisplayConfig` struct.
  //
  // `vout_` must be initialized.
  bool IgnoreDisplayMode() const;

  // Whether `timing` is a new display timing different from the timing
  // currently applied to the display.
  bool IsNewDisplayTiming(const display::DisplayTiming& timing) TA_REQ(display_mutex_);

  // Zircon handles
  zx::bti bti_;

  // Thread handles
  std::optional<thrd_t> vsync_thread_;

  // Protocol handles used in by this driver
  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev_;
  fidl::WireSyncClient<fuchsia_hardware_amlogiccanvas::Device> canvas_;
  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_;

  // Interrupts
  zx::interrupt vsync_irq_;

  // Locks used by the display driver
  fbl::Mutex display_mutex_;  // general display state (i.e. display_id)
  fbl::Mutex image_mutex_;    // used for accessing imported_images_
  fbl::Mutex capture_mutex_;  // general capture state

  // Relaxed is safe because full_init_done_ only ever moves from false to true.
  std::atomic<bool> full_init_done_ = false;

  // Display controller related data
  ddk::DisplayControllerInterfaceProtocolClient dc_intf_ TA_GUARDED(display_mutex_);

  // Display Capture interface protocol
  ddk::DisplayCaptureInterfaceProtocolClient capture_intf_ TA_GUARDED(capture_mutex_);

  // Points to the next capture target image to capture displayed contents into.
  // Stores nullptr if capture is not going to be performed.
  ImageInfo* current_capture_target_image_ TA_GUARDED(capture_mutex_);

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  // Imported Images
  fbl::DoublyLinkedList<std::unique_ptr<ImageInfo>> imported_images_ TA_GUARDED(image_mutex_);
  fbl::DoublyLinkedList<std::unique_ptr<ImageInfo>> imported_captures_ TA_GUARDED(capture_mutex_);

  // Objects: only valid if fully_initialized()
  std::unique_ptr<Vpu> vpu_;
  std::unique_ptr<VideoInputUnit> video_input_unit_;
  std::unique_ptr<Vout> vout_;

  // Monitoring. We create a named "amlogic-display" node to allow for easier filtering
  // of inspect tree when defining selectors and metrics.
  inspect::Inspector inspector_;
  inspect::Node root_node_;
  inspect::Node video_input_unit_node_;

  display::DisplayId display_id_ TA_GUARDED(display_mutex_) = kPanelDisplayId;
  bool display_attached_ TA_GUARDED(display_mutex_) = false;

  // The DisplayMode applied most recently to the display.
  //
  // Default-constructed if no configuration is applied to the display yet or
  // DisplayMode is ignored.
  display::DisplayTiming current_display_timing_ TA_GUARDED(display_mutex_) = {};

  std::unique_ptr<HotPlugDetection> hot_plug_detection_;
  std::unique_ptr<Capture> capture_;

  fit::function<bool(fuchsia_images2::wire::PixelFormat format)> format_support_check_ = nullptr;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_AMLOGIC_DISPLAY_H_
