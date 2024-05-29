// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/stdcompat/bit.h>
#include <zircon/assert.h>

#include <array>
#include <cstddef>
#include <limits>
#include <memory>
#include <vector>

#include <fuzzer/FuzzedDataProvider.h>

#include "test.h"

namespace {

constexpr uint64_t kMax = std::numeric_limits<uint64_t>::max();

enum class Action {
  kAllocate,
  kUpdateFreeRamSubranges,
  kFree,
  kResize,
  kMaxValue,  // Required by FuzzedDataProvider::ConsumeEnum().
};

bool IsValidPoolInitInput(cpp20::span<memalloc::Range> ranges) {
  // The valid input spaces of Pool::Init() and FindNormalizedRanges()
  // coincide. Since the latter returns an error, we use that as a proxy to
  // vet inputs to the former (taking that it works as expected for granted).
  constexpr auto noop = [](const memalloc::Range& range) { return true; };
  const size_t scratch_size = memalloc::FindNormalizedRangesScratchSize(ranges.size());
  auto scratch = std::make_unique<void*[]>(scratch_size);
  return memalloc::FindNormalizedRanges({ranges}, {scratch.get(), scratch_size}, noop).is_ok();
}

}  // namespace

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  FuzzedDataProvider provider(data, size);

  size_t num_range_bytes = provider.ConsumeIntegralInRange<size_t>(0, provider.remaining_bytes());
  std::vector<std::byte> bytes = provider.ConsumeBytes<std::byte>(num_range_bytes);
  cpp20::span<memalloc::Range> ranges = RangesFromBytes(bytes);

  if (!IsValidPoolInitInput(ranges)) {
    return 0;
  }

  const uint64_t default_min_addr = provider.ConsumeIntegral<uint64_t>();
  const uint64_t default_max_addr =
      provider.ConsumeIntegralInRange<uint64_t>(default_min_addr, kMax);
  PoolContext ctx;
  if (auto result = ctx.pool.Init(std::array{ranges}, default_min_addr, default_max_addr);
      result.is_error()) {
    return 0;
  }

  ZX_ASSERT_MSG(std::is_sorted(ctx.pool.begin(), ctx.pool.end()),
                "pool ranges are not sorted:\n%s\noriginal ranges:\n%s",
                ToString(ctx.pool.begin(), ctx.pool.end()).c_str(), ToString(ranges).c_str());

  // Tracks the non-bookkeeping allocations made that have yet to be partially
  // freed; this will serve as a means of generating valid inputs to Free().
  std::vector<memalloc::Range> allocations;

  while (provider.remaining_bytes()) {
    switch (provider.ConsumeEnum<Action>()) {
      case Action::kAllocate: {
        auto type = static_cast<memalloc::Type>(provider.ConsumeIntegralInRange<uint64_t>(
            memalloc::kMinExtendedTypeValue, memalloc::kMaxExtendedTypeValue));
        uint64_t size = provider.ConsumeIntegralInRange<uint64_t>(1, kMax);
        uint64_t alignment = uint64_t{1} << provider.ConsumeIntegralInRange<size_t>(0, 63);

        std::optional<uint64_t> local_min_addr, local_max_addr;
        bool use_default_min_addr = provider.ConsumeBool();
        bool use_default_max_addr = provider.ConsumeBool();
        if (!use_default_min_addr) {
          if (use_default_max_addr) {
            local_min_addr = provider.ConsumeIntegralInRange<uint64_t>(0, default_max_addr);
          } else {
            local_min_addr = provider.ConsumeIntegral<uint64_t>();
          }
        }
        if (!use_default_max_addr) {
          local_max_addr = provider.ConsumeIntegralInRange<uint64_t>(
              local_min_addr.value_or(default_min_addr), kMax);
        }

        if (auto result = ctx.pool.Allocate(type, size, alignment, local_min_addr, local_max_addr);
            result.is_ok()) {
          // We cannot free Free() bookkeeping ranges.
          if (type != memalloc::Type::kPoolBookkeeping) {
            allocations.emplace_back(
                memalloc::Range{.addr = result.value(), .size = size, .type = type});
          }
        }
        break;
      }
      case Action::kUpdateFreeRamSubranges: {
        auto type = static_cast<memalloc::Type>(provider.ConsumeIntegralInRange<uint64_t>(
            memalloc::kMinExtendedTypeValue, memalloc::kMaxExtendedTypeValue));
        uint64_t addr = provider.ConsumeIntegral<uint64_t>();
        uint64_t size = provider.ConsumeIntegralInRange<uint64_t>(0, kMax - addr);
        (void)ctx.pool.UpdateFreeRamSubranges(type, addr, size);
        break;
      }
      case Action::kFree: {
        if (allocations.empty()) {
          break;
        }

        // Pick a subrange of the last allocation.
        const auto& allocation = allocations.back();
        uint64_t addr =
            provider.ConsumeIntegralInRange<uint64_t>(allocation.addr, allocation.end());
        uint64_t size = provider.ConsumeIntegralInRange<uint64_t>(0, allocation.end() - addr);
        allocations.pop_back();
        (void)ctx.pool.Free(addr, size);
        break;
      }
      case Action::kResize: {
        if (allocations.empty()) {
          break;
        }
        // Resize the last allocation.
        auto& allocation = allocations.back();
        uint64_t new_size = provider.ConsumeIntegralInRange<uint64_t>(1, kMax);
        uint64_t min_alignment = uint64_t{1} << provider.ConsumeIntegralInRange<size_t>(
                                     0, cpp20::countr_zero(allocation.addr));
        if (auto result = ctx.pool.Resize(allocation, new_size, min_alignment); result.is_ok()) {
          allocation.addr = result.value();
          allocation.size = new_size;
        }
        break;
      }
      case Action::kMaxValue:
        break;
    }
  }

  return 0;
}
