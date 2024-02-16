// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/audiotypes/c/banjo.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/inspect/cpp/inspect.h>

#include <cstdint>
#include <optional>
#include <queue>
#include <vector>

#include <fbl/array.h>
#include <fbl/ref_counted.h>
#include <fbl/string_printf.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/migration-util.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/api-types-cpp/image-id.h"
#include "src/graphics/display/lib/edid/edid.h"

namespace display {

class DisplayInfo : public IdMappable<fbl::RefPtr<DisplayInfo>, DisplayId>,
                    public fbl::RefCounted<DisplayInfo> {
 public:
  static zx::result<fbl::RefPtr<DisplayInfo>> Create(const added_display_args_t& info);

  // Should be called after init_done is set to true.
  void InitializeInspect(inspect::Node* parent_node);

  void GetPhysicalDimensions(uint32_t* horizontal_size_mm, uint32_t* vertical_size_mm) {
    if (edid.has_value()) {
      *horizontal_size_mm = edid->base.horizontal_size_mm();
      *vertical_size_mm = edid->base.vertical_size_mm();
    } else {
      *horizontal_size_mm = *vertical_size_mm = 0;
    }
  }

  // Get human readable identifiers for this display. Strings will only live as
  // long as the containing DisplayInfo, callers should copy these if they want
  // to retain them longer.
  void GetIdentifiers(const char** manufacturer_name, const char** monitor_name,
                      const char** monitor_serial) {
    if (edid.has_value()) {
      *manufacturer_name = edid->base.manufacturer_name();
      if (!strcmp("", *manufacturer_name)) {
        *manufacturer_name = edid->base.manufacturer_id();
      }
      *monitor_name = edid->base.monitor_name();
      *monitor_serial = edid->base.monitor_serial();
    } else {
      *manufacturer_name = *monitor_name = *monitor_serial = "";
    }
  }

  struct Edid {
    edid::Edid base;
    fbl::Vector<display::DisplayTiming> timings;
    fbl::Vector<audio_types_audio_stream_format_range_t> audio;
  };

  // Exactly one of `edid` and `mode` can be non-nullopt.
  std::optional<Edid> edid;
  std::optional<display_mode_t> mode;

  fbl::Vector<CoordinatorPixelFormat> pixel_formats;

  // Flag indicating that the display is ready to be published to clients.
  bool init_done = false;

  // A list of all images which have been sent to display driver.
  Image::DoublyLinkedList images;

  // The number of layers in the applied configuration.
  uint32_t layer_count;

  // Set when a layer change occurs on this display and cleared in vsync
  // when the new layers are all active.
  bool pending_layer_change;
  // If a configuration applied by Controller has layer change to occur on the
  // display (i.e. |pending_layer_change| is true), this stores the Controller's
  // config stamp for that configuration; otherwise it stores an invalid stamp.
  ConfigStamp pending_layer_change_controller_config_stamp;

  // Flag indicating that a new configuration was delayed during a layer change
  // and should be reapplied after the layer change completes.
  bool delayed_apply;

  // True when we're in the process of switching between display clients.
  bool switching_client = false;

  // |config_image_queue| stores image IDs for each display configurations
  // applied in chronological order.
  // This is used by OnVsync() display events where clients receive image
  // IDs of the latest applied configuration on each Vsync.
  //
  // A |ClientConfigImages| entry is added to the queue once the config is
  // applied, and will be evicted when the config (or a newer config) is
  // already presented on the display at Vsync time.
  //
  // TODO(https://fxbug.dev/42152065): Remove once we remove image IDs in OnVsync() events.
  struct ConfigImages {
    const ConfigStamp config_stamp;

    struct ImageMetadata {
      ImageId image_id;
      ClientId client_id;
    };
    std::vector<ImageMetadata> images;
  };

  std::queue<ConfigImages> config_image_queue;

 private:
  DisplayInfo() = default;
  void PopulateDisplayAudio();
  inspect::Node node;
  inspect::ValueList properties;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_
