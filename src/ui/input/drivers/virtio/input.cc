// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "input.h"

#include <lib/ddk/debug.h>
#include <lib/fit/defer.h>
#include <limits.h>
#include <string.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/status.h>

#include <memory>
#include <utility>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>

#include "src/devices/bus/lib/virtio/trace.h"
#include "src/ui/input/drivers/virtio/input_kbd.h"
#include "src/ui/input/drivers/virtio/input_mouse.h"
#include "src/ui/input/drivers/virtio/input_touch.h"

#define LOCAL_TRACE 0

namespace virtio {

static bool IsQemuTouchscreen(const virtio_input_config_t& config) {
  if (config.u.ids.bustype == 0x06 && config.u.ids.vendor == 0x00 && config.u.ids.product == 0x00) {
    if (config.u.ids.version == 0x01 || config.u.ids.version == 0x00) {
      return true;
    }
  }
  return false;
}

InputDevice::InputDevice(zx_device_t* bus_device, zx::bti bti, std::unique_ptr<Backend> backend)
    : virtio::Device(std::move(bti), std::move(backend)),
      ddk::Device<InputDevice, ddk::Messageable<fuchsia_input_report::InputDevice>::Mixin>(
          bus_device) {
  metrics_root_ = inspector_.GetRoot().CreateChild("hid-input-report-touch");
  total_report_count_ = metrics_root_.CreateUint("total_report_count", 0);
  last_event_timestamp_ = metrics_root_.CreateUint("last_event_timestamp", 0);
}

InputDevice::~InputDevice() {}

zx_status_t InputDevice::Init() {
  LTRACEF("Device %p\n", this);

  fbl::AutoLock lock(&lock_);

  // Reset the device and read configuration
  DeviceReset();

  SelectConfig(VIRTIO_INPUT_CFG_ID_NAME, 0);
  LTRACEF_LEVEL(2, "name %s\n", config_.u.string);

  SelectConfig(VIRTIO_INPUT_CFG_ID_SERIAL, 0);
  LTRACEF_LEVEL(2, "serial %s\n", config_.u.string);

  SelectConfig(VIRTIO_INPUT_CFG_ID_DEVIDS, 0);
  if (config_.size >= sizeof(virtio_input_devids_t)) {
    LTRACEF_LEVEL(2, "bustype %d\n", config_.u.ids.bustype);
    LTRACEF_LEVEL(2, "vendor %d\n", config_.u.ids.vendor);
    LTRACEF_LEVEL(2, "product %d\n", config_.u.ids.product);
    LTRACEF_LEVEL(2, "version %d\n", config_.u.ids.version);
  }

  SelectConfig(VIRTIO_INPUT_CFG_EV_BITS, VIRTIO_INPUT_EV_KEY);
  uint8_t cfg_key_size = config_.size;
  SelectConfig(VIRTIO_INPUT_CFG_EV_BITS, VIRTIO_INPUT_EV_REL);
  uint8_t cfg_rel_size = config_.size;
  SelectConfig(VIRTIO_INPUT_CFG_EV_BITS, VIRTIO_INPUT_EV_ABS);
  uint8_t cfg_abs_size = config_.size;

  // At the moment we support keyboards and a specific touchscreen.
  // Support for more devices should be added here.
  SelectConfig(VIRTIO_INPUT_CFG_ID_DEVIDS, 0);
  if (IsQemuTouchscreen(config_)) {
    // QEMU MultiTouch Touchscreen
    SelectConfig(VIRTIO_INPUT_CFG_ABS_INFO, VIRTIO_INPUT_EV_MT_POSITION_X);
    virtio_input_absinfo_t x_info = config_.u.abs;
    SelectConfig(VIRTIO_INPUT_CFG_ABS_INFO, VIRTIO_INPUT_EV_MT_POSITION_Y);
    virtio_input_absinfo_t y_info = config_.u.abs;
    hid_device_ = std::make_unique<HidTouch>(x_info, y_info);
  } else if (cfg_rel_size > 0 || cfg_abs_size > 0) {
    // Mouse
    hid_device_ = std::make_unique<HidMouse>();
  } else if (cfg_key_size > 0) {
    // Keyboard
    hid_device_ = std::make_unique<HidKeyboard>();
  } else {
    return ZX_ERR_NOT_SUPPORTED;
  }

  DriverStatusAck();

  if (!(DeviceFeaturesSupported() & VIRTIO_F_VERSION_1)) {
    // Declaring non-support until there is a need in the future.
    zxlogf(ERROR, "Legacy virtio interface is not supported by this driver");
    return ZX_ERR_NOT_SUPPORTED;
  }
  DriverFeaturesAck(VIRTIO_F_VERSION_1);
  if (zx_status_t status = DeviceStatusFeaturesOk(); status != ZX_OK) {
    zxlogf(ERROR, "Feature negotiation failed: %s", zx_status_get_string(status));
    return status;
  }

  // Plan to clean up unless everything succeeds.
  auto cleanup = fit::defer([this]() { Release(); });

  // Allocate the main vring
  zx_status_t status = vring_.Init(0, kEventCount);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate vring: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate event buffers for the ring.
  // TODO: Avoid multiple allocations, allocate enough for all buffers once.
  for (uint16_t id = 0; id < kEventCount; ++id) {
    assert(sizeof(virtio_input_event_t) <= zx_system_get_page_size());
    status = io_buffer_init(&buffers_[id], bti_.get(), sizeof(virtio_input_event_t),
                            IO_BUFFER_RO | IO_BUFFER_CONTIG);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to allocate I/O buffers: %s", zx_status_get_string(status));
      return status;
    }
  }

  // Expose event buffers to the host
  vring_desc* desc = nullptr;
  uint16_t id;
  for (uint16_t i = 0; i < kEventCount; ++i) {
    desc = vring_.AllocDescChain(1, &id);
    if (desc == nullptr) {
      zxlogf(ERROR, "Failed to allocate descriptor chain");
      return ZX_ERR_NO_RESOURCES;
    }
    ZX_ASSERT(id < kEventCount);
    desc->addr = io_buffer_phys(&buffers_[id]);
    desc->len = sizeof(virtio_input_event_t);
    desc->flags |= VRING_DESC_F_WRITE;
    LTRACE_DO(virtio_dump_desc(desc));
    vring_.SubmitChain(id);
  }

  StartIrqThread();
  DriverStatusOk();

  status = DdkAdd(ddk::DeviceAddArgs("virtio-input").set_inspect_vmo(inspector_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: failed to add device: %s", tag(), zx_status_get_string(status));
    return status;
  }

  vring_.Kick();
  cleanup.cancel();
  return ZX_OK;
}

void InputDevice::DdkRelease() {
  fbl::AutoLock lock(&lock_);
  for (size_t i = 0; i < kEventCount; ++i) {
    if (io_buffer_is_valid(&buffers_[i])) {
      io_buffer_release(&buffers_[i]);
    }
  }
}

void InputDevice::ReceiveEvent(virtio_input_event_t* event) {
  hid_device_->ReceiveEvent(event);

  if (event->type == VIRTIO_INPUT_EV_SYN) {
    // TODO(fxbug.dev/64889): Currently we assume all input events are SYN_REPORT.
    // We need to handle other event codes like SYN_DROPPED as well.
    fbl::AutoLock lock(&lock_);
    total_report_count_.Add(1);
    last_event_timestamp_.Set(hid_device_->SendReportToAllReaders().get());
  }
}

void InputDevice::IrqRingUpdate() {
  auto free_chain = [this](vring_used_elem* used_elem) {
    uint16_t id = static_cast<uint16_t>(used_elem->id & 0xffff);
    vring_desc* desc = vring_.DescFromIndex(id);
    ZX_ASSERT(id < kEventCount);
    ZX_ASSERT(desc->len == sizeof(virtio_input_event_t));

    auto evt = static_cast<virtio_input_event_t*>(io_buffer_virt(&buffers_[id]));
    ReceiveEvent(evt);

    ZX_ASSERT((desc->flags & VRING_DESC_F_NEXT) == 0);
    vring_.FreeDesc(id);
  };

  vring_.IrqRingUpdate(free_chain);

  vring_desc* desc = nullptr;
  uint16_t id;
  bool need_kick = false;
  while ((desc = vring_.AllocDescChain(1, &id))) {
    desc->len = sizeof(virtio_input_event_t);
    vring_.SubmitChain(id);
    need_kick = true;
  }

  if (need_kick) {
    vring_.Kick();
  }
}

void InputDevice::IrqConfigChange() { LTRACEF("IrqConfigChange\n"); }

void InputDevice::SelectConfig(uint8_t select, uint8_t subsel) {
  WriteDeviceConfig(offsetof(virtio_input_config_t, select), select);
  WriteDeviceConfig(offsetof(virtio_input_config_t, subsel), subsel);
  CopyDeviceConfig(&config_, sizeof(config_));
}

}  // namespace virtio
