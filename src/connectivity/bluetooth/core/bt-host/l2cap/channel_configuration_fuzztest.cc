// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/channel_configuration.h"

namespace bt::l2cap::internal {

void fuzz(const uint8_t* data, size_t size) {
  DynamicByteBuffer buf(size);
  memcpy(buf.mutable_data(), data, size);
  ChannelConfiguration config;
  bool _result = config.ReadOptions(buf);
  // unused.
  (void)_result;
}

}  // namespace bt::l2cap::internal

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  bt::l2cap::internal::fuzz(data, size);
  return 0;
}
