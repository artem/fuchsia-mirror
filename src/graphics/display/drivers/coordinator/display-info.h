// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/audiotypes/c/banjo.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/inspect/cpp/inspect.h>

#include <cstdint>
#include <optional>
#include <queue>
#include <string_view>
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

  DisplayInfo(const DisplayInfo&) = delete;
  DisplayInfo(DisplayInfo&&) = delete;
  DisplayInfo& operator=(const DisplayInfo&) = delete;
  DisplayInfo& operator=(DisplayInfo&&) = delete;

  ~DisplayInfo();

  // Should be called after init_done is set to true.
  void InitializeInspect(inspect::Node* parent_node);

  // Returns zero if the information is not available.
  uint32_t GetHorizontalSizeMm() const;

  // Returns zero if the information is not available.
  uint32_t GetVerticalSizeMm() const;

  // Returns an empty view if the information is not available.
  std::string_view GetManufacturerName() const;

  // Returns an empty view if the information is not available.
  std::string_view GetMonitorName() const;

  // Returns an empty string if the information is not available.
  std::string_view GetMonitorSerial() const;

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
  DisplayInfo();
  void PopulateDisplayAudio();
  inspect::Node node;
  inspect::ValueList properties;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DISPLAY_INFO_H_
