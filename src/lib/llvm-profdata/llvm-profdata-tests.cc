// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/note.h>
#include <lib/elfldltl/self.h>
#include <lib/llvm-profdata/llvm-profdata.h>
#include <lib/stdcompat/span.h>

#include <cstdint>
#include <memory>

#include <gtest/gtest.h>

#include "coverage-example.h"

namespace {

// The compiler doesn't support relocatable mode on macOS.
#ifdef __APPLE__
constexpr bool kRelocatableCounters = false;
#else
constexpr bool kRelocatableCounters = true;
#endif

cpp20::span<const std::byte> MyBuildId() {
  // TODO(mcgrathr): For these unit tests, it doesn't matter what the ID is.
  // For end-to-end tests using the offline tools, this will need to be the
  // real build ID of the test module.
  static constexpr std::byte kId[] = {std::byte{0xaa}, std::byte{0xbb}};
  return kId;
}

TEST(LlvmProfdataTests, SizeBytes) {
  LlvmProfdata data;
  data.Init(MyBuildId());
  EXPECT_GT(data.size_bytes(), size_t{0});
}

TEST(LlvmProfdataTests, CountersOffsetAndSizeBytes) {
  LlvmProfdata data;
  data.Init(MyBuildId());
  EXPECT_GT(data.counters_offset(), size_t{0});
  EXPECT_GT(data.counters_size_bytes(), size_t{0});
  EXPECT_LT(data.counters_offset(), data.size_bytes());
  EXPECT_LT(data.counters_size_bytes(), data.size_bytes() - data.counters_offset());
}

TEST(LlvmProfdataTests, FixedData) {
  LlvmProfdata data;
  data.Init(MyBuildId());

  const size_t buffer_size = data.size_bytes();
  ASSERT_GT(buffer_size, size_t{0});
  std::unique_ptr<std::byte[]> buffer(new std::byte[buffer_size]);
  const cpp20::span buffer_span(buffer.get(), buffer_size);

  auto live_data = data.WriteFixedData(buffer_span);
  ASSERT_FALSE(live_data.counters.empty());

  EXPECT_TRUE(data.Match(buffer_span));

  auto matched_data = data.VerifyMatch(buffer_span);
  EXPECT_EQ(matched_data.counters.data(), live_data.counters.data());
  EXPECT_EQ(matched_data.counters.size_bytes(), live_data.counters.size_bytes());
}

TEST(LlvmProfdataTests, CopyLiveData) {
  LlvmProfdata data;
  data.Init(MyBuildId());

  const size_t buffer_size = data.size_bytes();
  ASSERT_GT(buffer_size, size_t{0});
  std::unique_ptr<std::byte[]> buffer(new std::byte[buffer_size]);
  const cpp20::span buffer_span(buffer.get(), buffer_size);

  auto live_data = data.WriteFixedData(buffer_span);
  ASSERT_FALSE(live_data.counters.empty());

  const cpp20::span<uint64_t> counters{
      reinterpret_cast<uint64_t*>(live_data.counters.data()),
      live_data.counters.size_bytes() / sizeof(uint64_t),
  };

  // Fill the buffer with unreasonable counter values.
  std::fill(counters.begin(), counters.end(), ~uint64_t{});

  // Now copy out the current values.
  data.CopyLiveData({
      .counters = cpp20::as_writable_bytes(counters),
      .bitmap = live_data.bitmap,
  });

  // None of the real values should be the unreasonable value.
  for (size_t i = 0; i < counters.size(); ++i) {
    EXPECT_NE(counters[i], ~uint64_t{}) << "counter " << i;
  }

  // In case the normal profile runtime is also active, reset the bias.
  LlvmProfdata::UseLinkTimeLiveData();

  // Now run some instrumented code that should be sure to touch a counter.
  RunTimeCoveredFunction();

  std::unique_ptr<uint64_t[]> new_buffer(new uint64_t[counters.size()]);
  const cpp20::span new_counters(new_buffer.get(), counters.size());

  // Fill the buffer with unreasonable counter values.
  std::fill(new_counters.begin(), new_counters.end(), ~uint64_t{});

  // Now copy out the new values after running covered code.
  data.CopyLiveData({
      .counters = cpp20::as_writable_bytes(new_counters),
      .bitmap = live_data.bitmap,
  });

  uint64_t increase = 0;
  for (size_t i = 0; i < counters.size(); ++i) {
    // None of the real values should be the unreasonable value.
    EXPECT_NE(new_counters[i], ~uint64_t{}) << "counter " << i;

    // No counter should have decreased.
    EXPECT_GE(new_counters[i], counters[i]);

    // Accumulate all the increased hit counts together.
    increase += new_counters[i] - counters[i];
  }

  // At least one counter in RunTimeCoveredFunction should have increased.
  EXPECT_GT(increase, uint64_t{0});
}

TEST(LlvmProfdataTests, MergeLiveData) {
  static constexpr uint64_t kOldCounters[] = {1, 2, 3, 4};
  uint64_t new_counters[] = {5, 6, 7, 8};
  static_assert(std::size(kOldCounters) == std::size(new_counters));

  static constexpr uint8_t kOldBitmap[] = {0, 0x01, 0x02, 0x03};
  uint8_t new_bitmap[] = {1, 0x11, 0x20, 0x31};

  constexpr auto as_falsely_writable = [](auto span) {
    cpp20::span<const std::byte> bytes = cpp20::as_bytes(span);
    return cpp20::span<std::byte>{
        const_cast<std::byte*>(bytes.data()),
        bytes.size(),
    };
  };

  LlvmProfdata::MergeLiveData(
      {
          .counters = cpp20::as_writable_bytes(cpp20::span(new_counters)),
          .bitmap = cpp20::as_writable_bytes(cpp20::span(new_bitmap)),
      },
      {
          .counters = as_falsely_writable(cpp20::span(kOldCounters)),
          .bitmap = as_falsely_writable(cpp20::span(kOldBitmap)),
      });

  EXPECT_EQ(new_counters[0], 6u);
  EXPECT_EQ(new_counters[1], 8u);
  EXPECT_EQ(new_counters[2], 10u);
  EXPECT_EQ(new_counters[3], 12u);

  EXPECT_EQ(new_bitmap[0], 0x01u);
  EXPECT_EQ(new_bitmap[1], 0x11u);
  EXPECT_EQ(new_bitmap[2], 0x22u);
  EXPECT_EQ(new_bitmap[3], 0x33u);

  LlvmProfdata data;
  data.Init(MyBuildId());

  const size_t buffer_size = data.size_bytes();
  ASSERT_GT(buffer_size, size_t{0});
  std::unique_ptr<std::byte[]> buffer(new std::byte[buffer_size]);
  const cpp20::span buffer_span(buffer.get(), buffer_size);

  auto live_data = data.WriteFixedData(buffer_span);
  ASSERT_FALSE(live_data.counters.empty());

  const cpp20::span<uint64_t> counters{
      reinterpret_cast<uint64_t*>(live_data.counters.data()),
      live_data.counters.size_bytes() / sizeof(uint64_t),
  };

  // In case the normal profile runtime is also active, reset the bias.
  LlvmProfdata::UseLinkTimeLiveData();

  // Run some instrumented code that should be sure to touch a counter.
  RunTimeCoveredFunction();

  // Set initial values for each counter in our buffer.
  for (size_t i = 0; i < counters.size(); ++i) {
    counters[i] = i;
  }

  // Now merge the current data into our synthetic starting data.
  data.MergeLiveData({.counters = cpp20::as_writable_bytes(counters), .bitmap = live_data.bitmap});

  uint64_t increase = 0;
  for (size_t i = 0; i < counters.size(); ++i) {
    // No counter should have decreased.
    EXPECT_GE(counters[i], i);

    // Accumulate all the increased hit counts together.
    increase += counters[i] - i;
  }

  // At least one counter in RunTimeCoveredFunction should have increased.
  EXPECT_GT(increase, uint64_t{0});
}

TEST(LlvmProfdataTests, UseLiveData) {
  LlvmProfdata data;
  data.Init(MyBuildId());

  const size_t buffer_size = data.size_bytes();
  ASSERT_GT(buffer_size, size_t{0});
  std::unique_ptr<std::byte[]> buffer(new std::byte[buffer_size]);
  const cpp20::span buffer_span(buffer.get(), buffer_size);

  auto live_data = data.WriteFixedData(buffer_span);
  ASSERT_FALSE(live_data.counters.empty());

  const cpp20::span<uint64_t> counters{
      reinterpret_cast<uint64_t*>(live_data.counters.data()),
      live_data.counters.size_bytes() / sizeof(uint64_t),
  };

  // Start all counters at zero.
  std::fill(counters.begin(), counters.end(), 0);

  if (kRelocatableCounters) {
    LlvmProfdata::UseLiveData(live_data);

    // Now run some instrumented code that should be sure to touch a counter.
    RunTimeCoveredFunction();

    // Go back to writing into the statically-allocated data.  Note that if the
    // normal profile runtime is enabled and using relocatable mode (as it
    // always does on Fuchsia), this will skew down the coverage numbers for
    // this test code itself.
    LlvmProfdata::UseLinkTimeLiveData();

    uint64_t hits = 0;
    for (uint64_t count : counters) {
      hits += count;
    }

    // At least one counter in RunTimeCoveredFunction should have increased.
    EXPECT_GT(hits, uint64_t{0});
  }
}

}  // namespace
