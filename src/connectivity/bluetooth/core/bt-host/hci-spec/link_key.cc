// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/link_key.h"

namespace bt::hci_spec {

LinkKey::LinkKey() : rand_(0), ediv_(0) { value_.fill(0); }

LinkKey::LinkKey(const UInt128& ltk, uint64_t rand, uint16_t ediv)
    : value_(ltk), rand_(rand), ediv_(ediv) {}

}  // namespace bt::hci_spec
