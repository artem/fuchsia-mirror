// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "src/devices/lib/fidl-metadata/spi.h"

#include <fidl/fuchsia.hardware.spi.businfo/cpp/wire.h>

namespace fidl_metadata::spi {
zx::result<std::vector<uint8_t>> SpiChannelsToFidl(const uint32_t bus_id,
                                                   const cpp20::span<const Channel> channels) {
  fidl::Arena allocator;
  fidl::VectorView<fuchsia_hardware_spi_businfo::wire::SpiChannel> spi_channels(allocator,
                                                                                channels.size());

  for (size_t i = 0; i < channels.size(); i++) {
    auto& chan = spi_channels[i];
    chan.Allocate(allocator);

    chan.set_cs(channels[i].cs);
    chan.set_pid(channels[i].pid);
    chan.set_did(channels[i].did);
    chan.set_vid(channels[i].vid);
  }

  fuchsia_hardware_spi_businfo::wire::SpiBusMetadata metadata(allocator);
  metadata.set_bus_id(bus_id);
  metadata.set_channels(allocator, spi_channels);

  return zx::result<std::vector<uint8_t>>{
      fidl::Persist(metadata).map_error(std::mem_fn(&fidl::Error::status))};
}
}  // namespace fidl_metadata::spi
