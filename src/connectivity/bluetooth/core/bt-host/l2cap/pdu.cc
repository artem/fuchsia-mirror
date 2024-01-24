// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/pdu.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/acl_data_packet.h"

namespace bt::l2cap {

// NOTE: The order in which these are initialized matters, as
// other.ReleaseFragments() resets |other.fragment_count_|.
PDU::PDU(PDU&& other) : fragments_(other.ReleaseFragments()) {}

PDU& PDU::operator=(PDU&& other) {
  // NOTE: The order in which these are initialized matters, as
  // other.ReleaseFragments() resets |other.fragment_count_|.
  fragments_ = other.ReleaseFragments();
  return *this;
}

size_t PDU::Copy(MutableByteBuffer* out_buffer, size_t pos, size_t size) const {
  BT_DEBUG_ASSERT(out_buffer);
  BT_DEBUG_ASSERT(pos <= length());
  BT_DEBUG_ASSERT(is_valid());

  size_t remaining = std::min(size, length() - pos);
  BT_DEBUG_ASSERT(out_buffer->size() >= remaining);
  if (!remaining) {
    return 0;
  }

  bool found = false;
  size_t offset = 0u;
  for (auto iter = fragments_.begin(); iter != fragments_.end() && remaining; ++iter) {
    auto payload = (*iter)->view().payload_data();

    // Skip the Basic L2CAP header for the first fragment.
    if (iter == fragments_.begin()) {
      payload = payload.view(sizeof(BasicHeader));
    }

    // We first find the beginning fragment based on |pos|.
    if (!found) {
      size_t fragment_size = payload.size();
      if (pos >= fragment_size) {
        pos -= fragment_size;
        continue;
      }

      // The beginning fragment has been found.
      found = true;
    }

    // Calculate how much to read from the current fragment
    size_t write_size = std::min(payload.size() - pos, remaining);

    // Read the fragment into out_buffer->mutable_data() + offset.
    out_buffer->Write(payload.data() + pos, write_size, offset);

    // Clear |pos| after using it on the first fragment as all successive
    // fragments are read from the beginning.
    if (pos)
      pos = 0u;

    offset += write_size;
    remaining -= write_size;
  }

  return offset;
}

PDU::FragmentList PDU::ReleaseFragments() {
  auto out_list = std::move(fragments_);

  BT_DEBUG_ASSERT(!is_valid());
  return out_list;
}

const BasicHeader& PDU::basic_header() const {
  BT_DEBUG_ASSERT(!fragments_.empty());
  const auto& fragment = *fragments_.begin();

  BT_DEBUG_ASSERT(fragment->packet_boundary_flag() !=
                  hci_spec::ACLPacketBoundaryFlag::kContinuingFragment);
  return fragment->view().payload<BasicHeader>();
}

void PDU::AppendFragment(hci::ACLDataPacketPtr fragment) {
  BT_DEBUG_ASSERT(fragment);
  BT_DEBUG_ASSERT(!is_valid() ||
                  (*fragments_.begin())->connection_handle() == fragment->connection_handle());
  fragments_.push_back(std::move(fragment));
}

}  // namespace bt::l2cap
