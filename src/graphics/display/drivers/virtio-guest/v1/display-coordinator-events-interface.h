
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_COORDINATOR_EVENTS_INTERFACE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_COORDINATOR_EVENTS_INTERFACE_H_

#include <fidl/fuchsia.sysmem/cpp/wire.h>
// TODO(https://fxbug.dev/42079190): Switch from Banjo to FIDL or api-types-cpp types.
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"

namespace virtio_display {

// The events in the [`fuchsia.hardware.display.engine/Engine`] FIDL interface.
//
// This abstract base class only represents the events in the FIDL interface.
// The methods are represented by `DisplayEngineInterface`.
//
// This abstract base class also represents the
// [`fuchsia.hardware.display.controller/DisplayControllerInterface`] Banjo
// interface.
class DisplayCoordinatorEventsInterface {
 public:
  DisplayCoordinatorEventsInterface() = default;

  DisplayCoordinatorEventsInterface(const DisplayCoordinatorEventsInterface&) = delete;
  DisplayCoordinatorEventsInterface(DisplayCoordinatorEventsInterface&&) = delete;
  DisplayCoordinatorEventsInterface& operator=(const DisplayCoordinatorEventsInterface&) = delete;
  DisplayCoordinatorEventsInterface& operator=(DisplayCoordinatorEventsInterface&&) = delete;

  // TODO(https://fxbug.dev/42079190): Switch from Banjo to FIDL or api-types-cpp types.
  virtual void OnDisplaysChanged(cpp20::span<const added_display_args_t> added_displays,
                                 cpp20::span<const display::DisplayId> removed_ids) = 0;

  virtual void OnDisplayVsync(display::DisplayId display_id, zx::time timestamp,
                              display::ConfigStamp config_stamp) = 0;

  virtual void OnCaptureComplete() = 0;

 protected:
  // Destruction via base class pointer is not supported intentionally.
  // Instances are not expected to be owned by pointers to base classes.
  ~DisplayCoordinatorEventsInterface() = default;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_COORDINATOR_EVENTS_INTERFACE_H_
