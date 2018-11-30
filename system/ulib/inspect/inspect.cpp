// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/inspect/inspect.h>

namespace inspect {

namespace {
constexpr size_t kDefaultCapacityBytes = 4 << 10;
constexpr size_t kDefaultMaxSizeBytes = 1 << 20;
constexpr char kVmoName[] = "inspect-vmo";
constexpr char kRootObjectName[] = "objects";
} // namespace

using internal::Heap;

Inspector::Inspector()
    : Inspector(kDefaultCapacityBytes, kDefaultMaxSizeBytes) {}

Inspector::Inspector(size_t capacity, size_t max_size) {
    fbl::unique_ptr<fzl::ResizeableVmoMapper> vmo =
        fzl::ResizeableVmoMapper::Create(capacity, kVmoName);
    if (!vmo) {
        return;
    }

    fbl::AllocChecker ac;
    fbl::unique_ptr<Heap> heap(new (&ac) internal::Heap(std::move(vmo), max_size));
    if (!ac.check()) {
        return;
    }

    state_ = internal::State::Create(std::move(heap));
    if (!state_) {
        return;
    }

    root_object_ = state_->CreateObject(kRootObjectName, 0);
}

} // namespace inspect
