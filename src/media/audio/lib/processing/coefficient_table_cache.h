// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_LIB_PROCESSING_COEFFICIENT_TABLE_CACHE_H_
#define SRC_MEDIA_AUDIO_LIB_PROCESSING_COEFFICIENT_TABLE_CACHE_H_

#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>

#include <map>
#include <memory>
#include <mutex>
#include <utility>

#include "src/lib/fxl/synchronization/thread_annotations.h"
#include "src/media/audio/lib/processing/coefficient_table.h"

namespace media_audio {

// A cache of `CoefficientTables`. These use a lot of memory so we try to reuse them as much as
// possible. For example, different filters might use the same underlying coefficient table with
// slightly different filter parameters. Additionally, different samplers might use the same filter.
//
// `InputT` defines the set of inputs that are used to construct the CoefficientTable.
template <class InputT>
class CoefficientTableCache {
 public:
  // Thread-safe reference-counted pointer to a cached `CoefficientTable`.
  // This is like a `std::shared_ptr`, except the destructor runs atomically with a `Get` call to
  // simplify cache garbage collection.
  class SharedPtr {
   public:
    SharedPtr() = default;
    SharedPtr(SharedPtr&& r) noexcept : ptr_(r.ptr_), drop_(std::move(r.drop_)) {
      r.ptr_ = nullptr;
    }

    ~SharedPtr() {
      if (ptr_) {
        drop_();
      }
    }

    SharedPtr(const SharedPtr& r) = delete;
    SharedPtr& operator=(const SharedPtr& r) = delete;
    SharedPtr& operator=(SharedPtr&& r) noexcept {
      if (ptr_) {
        drop_();
      }
      ptr_ = r.ptr_;
      drop_ = std::move(r.drop_);
      r.ptr_ = nullptr;
      return *this;
    }

    CoefficientTable* get() const { return ptr_; }
    explicit operator bool() const { return ptr_ != nullptr; }

   private:
    friend class CoefficientTableCache<InputT>;
    SharedPtr(CoefficientTable* p, fit::function<void()> drop) : ptr_(p), drop_(std::move(drop)) {
      FX_CHECK(p);
    }

    CoefficientTable* ptr_{nullptr};
    fit::function<void()> drop_;
  };

  explicit CoefficientTableCache(fit::function<CoefficientTable*(const InputT&)> create_table)
      : create_table_(std::move(create_table)) {}

  // Returns a cached table for the given inputs, or if a cached tabled does not exist, a new table
  // is created and stored in the cache.
  SharedPtr Get(InputT inputs) {
    return AddEntry(inputs, [this, inputs]() { return create_table_(inputs); });
  }

  // Similar to `Get`, but uses the given table rather than creating a new one.
  SharedPtr Add(InputT inputs, CoefficientTable* table) {
    return AddEntry(inputs, [table]() { return table; });
  }

 private:
  SharedPtr AddEntry(InputT inputs, fit::function<CoefficientTable*()> create_table) {
    // Don't use a locker here so we can release mutex_ before creating a new table.
    // This allows multiple threads to create tables concurrently.
    mutex_.lock();

    // `std::map` guarantees that iterators are not invalidated until erased.
    // We hold onto this iterator until the reference is dropped.
    auto lookup_result = cache_.insert(std::make_pair(inputs, std::make_unique<Entry>()));
    auto it = lookup_result.first;

    std::lock_guard<std::mutex> entry_locker(it->second->mutex);
    mutex_.unlock();

    it->second->ref_cnt++;
    if (!it->second->table) {
      FX_DCHECK(lookup_result.second);
      FX_DCHECK(it->second->ref_cnt == 1);
      it->second->table = create_table();
    } else {
      FX_DCHECK(!lookup_result.second);
    }

    return SharedPtr(it->second->table, [this, it]() {
      std::lock_guard<std::mutex> cache_locker(mutex_);
      it->second->mutex.lock();
      it->second->ref_cnt--;
      if (it->second->ref_cnt == 0) {
        delete it->second->table;
        it->second->mutex.unlock();
        cache_.erase(it);
      } else {
        it->second->mutex.unlock();
      }
    });
  }

  struct Entry {
    std::mutex mutex;
    size_t ref_cnt FXL_GUARDED_BY(mutex) = 0;
    CoefficientTable* table FXL_GUARDED_BY(mutex) = nullptr;
  };

  std::mutex mutex_;
  std::map<InputT, std::unique_ptr<Entry>> cache_ FXL_GUARDED_BY(mutex_);
  fit::function<CoefficientTable*(InputT)> create_table_;
};

// `LazySharedCoefficientTable` is a wrapper around `CoefficientTables` that are constructed lazily.
// This is a simple way to construct a `CoefficientTable` table in any thread (such as the FIDL loop
// thread) but delay the potentially-expensive step of building the table until the table is
// actually needed, possibly on another thread.
template <class InputT>
class LazySharedCoefficientTable {
 public:
  using CacheT = CoefficientTableCache<InputT>;
  LazySharedCoefficientTable(CacheT* cache, InputT inputs) : cache_(cache), inputs_(inputs) {}

  LazySharedCoefficientTable(const LazySharedCoefficientTable&) = delete;
  LazySharedCoefficientTable& operator=(const LazySharedCoefficientTable&) = delete;

  CoefficientTable* get() {
    if (unlikely(!ptr_)) {
      ptr_ = cache_->Get(inputs_);
    }
    return ptr_.get();
  }

  CoefficientTable& operator*() { return *get(); }

 private:
  CacheT* cache_;
  InputT inputs_;
  typename CacheT::SharedPtr ptr_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_LIB_PROCESSING_COEFFICIENT_TABLE_CACHE_H_
