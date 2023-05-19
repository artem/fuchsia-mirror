// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LAZY_INIT_INTERNAL_H_
#define LIB_LAZY_INIT_INTERNAL_H_

#include <type_traits>
#include <utility>

#include "options.h"

namespace lazy_init::internal {

constexpr void Assert(bool condition) {
  if (!condition) {
    __builtin_abort();
  }
}

// Empty type that is trivially constructible/destructible.
struct Empty {};

// Lazy-initialized storage type for trivially destructible value types.
template <typename T, bool = std::is_trivially_destructible_v<T>>
union LazyInitStorage {
  constexpr LazyInitStorage() : empty{} {}

  // Trivial destructor required so that the overall union is also trivially
  // destructible.
  ~LazyInitStorage() = default;

  constexpr T& operator*() { return value; }
  constexpr T* operator->() { return &value; }
  constexpr T* GetStorageAddress() { return &value; }

  constexpr const T& operator*() const { return value; }
  constexpr const T* operator->() const { return &value; }
  constexpr const T* GetStorageAddress() const { return &value; }

  Empty empty;
  T value;
};

// Lazy-initialized storage type for non-trivially destructible value types.
template <typename T>
union LazyInitStorage<T, false> {
  constexpr LazyInitStorage() : empty{} {}

  // Non-trivial destructor required when at least one variant is non-
  // trivially destructible, making the overall union also non-trivially
  // destructible.
  ~LazyInitStorage() {}

  constexpr T& operator*() { return value; }
  constexpr T* operator->() { return &value; }
  constexpr T* GetStorageAddress() { return &value; }

  constexpr const T& operator*() const { return value; }
  constexpr const T* operator->() const { return &value; }
  constexpr const T* GetStorageAddress() const { return &value; }

  Empty empty;
  T value;
};

}  // namespace lazy_init::internal

#endif  // LIB_LAZY_INIT_INTERNAL_H_
