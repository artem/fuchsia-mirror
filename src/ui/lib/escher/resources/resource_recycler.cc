// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/lib/escher/resources/resource_recycler.h"

#include <vector>

#include "src/ui/lib/escher/escher.h"

namespace escher {

ResourceRecycler::ResourceRecycler(EscherWeakPtr escher)
    : ResourceManager(std::move(escher)), weak_factory_(this) {
  // Register ourselves for sequence number updates. Register() is defined in
  // our superclass CommandBufferSequenceListener.
  this->escher()->command_buffer_sequencer()->AddListener(this);
}

ResourceRecycler::~ResourceRecycler() {
  {
    std::scoped_lock lock(lock_);
    FX_DCHECK(unused_resources_.empty());
  }
  // Unregister ourselves. Unregister() is defined in our superclass
  // CommandBufferSequenceListener.
  escher()->command_buffer_sequencer()->RemoveListener(this);
}

void ResourceRecycler::OnReceiveOwnable(std::unique_ptr<Resource> resource) {
  if (resource->sequence_number() <= last_finished_sequence_number_) {
    // Recycle immediately.
    RecycleResource(std::move(resource));
  } else {
    // Defer recycling.
    std::scoped_lock lock(lock_);
    unused_resources_[resource.get()] = std::move(resource);
  }
}

void ResourceRecycler::OnCommandBufferFinished(uint64_t sequence_number) {
  FX_DCHECK(sequence_number > last_finished_sequence_number_);
  last_finished_sequence_number_ = sequence_number;

  // The sequence number allows us to find all unused resources that are no
  // longer referenced by a pending command-buffer; destroy these.
  std::vector<std::unique_ptr<Resource>> resources_to_recycle;
  {
    std::scoped_lock lock(lock_);
    auto it = unused_resources_.begin();
    while (it != unused_resources_.end()) {
      if (it->second->sequence_number() <= last_finished_sequence_number_) {
        resources_to_recycle.push_back(std::move(it->second));
        it = unused_resources_.erase(it);
      } else {
        ++it;
      }
    }
  }

  for (auto& resource : resources_to_recycle) {
    RecycleResource(std::move(resource));
  }
}

}  // namespace escher
