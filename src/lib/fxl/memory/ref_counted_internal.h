// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Internal implementation details for ref_counted.h.

#ifndef SRC_LIB_FXL_MEMORY_REF_COUNTED_INTERNAL_H_
#define SRC_LIB_FXL_MEMORY_REF_COUNTED_INTERNAL_H_

#include <zircon/assert.h>

#include <atomic>

#include "src/lib/fxl/macros.h"

namespace fxl {
namespace internal {

// See ref_counted.h for comments on the public methods.
class RefCountedThreadSafeBase {
 public:
  void AddRef() const {
#ifndef NDEBUG
    ZX_DEBUG_ASSERT(!adoption_required_);
    ZX_DEBUG_ASSERT(!destruction_started_);
#endif
    ref_count_.fetch_add(1u, std::memory_order_relaxed);
  }

  bool HasOneRef() const { return ref_count_.load(std::memory_order_acquire) == 1u; }

  void AssertHasOneRef() const { ZX_DEBUG_ASSERT(HasOneRef()); }

 protected:
  RefCountedThreadSafeBase();
  ~RefCountedThreadSafeBase();

  // Returns true if the object should self-delete.
  bool Release() const {
#ifndef NDEBUG
    ZX_DEBUG_ASSERT(!adoption_required_);
    ZX_DEBUG_ASSERT(!destruction_started_);
#endif
    ZX_DEBUG_ASSERT(ref_count_.load(std::memory_order_acquire) != 0u);
    // TODO(vtl): We could add the following:
    //     if (ref_count_.load(std::memory_order_relaxed) == 1u) {
    // #ifndef NDEBUG
    //       destruction_started_= true;
    // #endif
    //       return true;
    //     }
    // This would be correct. On ARM (an Nexus 4), in *single-threaded* tests,
    // this seems to make the destruction case marginally faster (barely
    // measurable), and while the non-destruction case remains about the same
    // (possibly marginally slower, but my measurements aren't good enough to
    // have any confidence in that). I should try multithreaded/multicore tests.
    if (ref_count_.fetch_sub(1u, std::memory_order_release) == 1u) {
      std::atomic_thread_fence(std::memory_order_acquire);
#ifndef NDEBUG
      destruction_started_ = true;
#endif
      return true;
    }
    return false;
  }

#ifndef NDEBUG
  void Adopt() {
    ZX_DEBUG_ASSERT(adoption_required_);
    adoption_required_ = false;
  }
#endif

 private:
  mutable std::atomic_uint_fast32_t ref_count_;

#ifndef NDEBUG
  mutable bool adoption_required_;
  mutable bool destruction_started_;
#endif

  FXL_DISALLOW_COPY_AND_ASSIGN(RefCountedThreadSafeBase);
};

inline RefCountedThreadSafeBase::RefCountedThreadSafeBase()
    : ref_count_(1u)
#ifndef NDEBUG
      ,
      adoption_required_(true),
      destruction_started_(false)
#endif
{
}

inline RefCountedThreadSafeBase::~RefCountedThreadSafeBase() {
#ifndef NDEBUG
  ZX_DEBUG_ASSERT(!adoption_required_);
  // Should only be destroyed as a result of |Release()|.
  ZX_DEBUG_ASSERT(destruction_started_);
#endif
}

}  // namespace internal
}  // namespace fxl

#endif  // SRC_LIB_FXL_MEMORY_REF_COUNTED_INTERNAL_H_
