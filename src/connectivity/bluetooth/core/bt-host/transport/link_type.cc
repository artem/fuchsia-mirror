// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/link_type.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/assert.h"

namespace bt {

std::string LinkTypeToString(LinkType type) {
  switch (type) {
    case LinkType::kACL:
      return "ACL";
    case LinkType::kSCO:
      return "SCO";
    case LinkType::kESCO:
      return "ESCO";
    case LinkType::kLE:
      return "LE";
  }

  BT_PANIC("invalid link type: %u", static_cast<unsigned int>(type));
  return "(invalid)";
}

}  // namespace bt
