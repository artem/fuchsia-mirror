// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.inspect/cpp/fidl.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/executor.h>
#include <lib/fzl/time.h>
#include <lib/inspect/component/cpp/service.h>
#include <lib/inspect/component/cpp/testing.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <unistd.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <array>
#include <thread>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

#include "src/storage/blobfs/blobfs_metrics.h"
#include "src/storage/blobfs/format.h"

namespace blobfs {
namespace {

constexpr int kNumOperations = 5;
constexpr size_t kNumThreads = 5;
constexpr size_t MB = 1 << 20;
const zx_ticks_t ms = fzl::NsToTicks(zx::nsec(zx::msec(1).to_nsecs())).get();

TEST(ReadMetricsTest, UncompressedDiskRead) {
  inspect::Node metrics_node;
  ReadMetrics read_metrics(&metrics_node);

  auto stats = read_metrics.GetSnapshot(CompressionAlgorithm::kUncompressed);
  EXPECT_EQ(stats.read_bytes, 0u);
  EXPECT_EQ(stats.read_ticks, 0);

  constexpr uint64_t kReadBytes = 1 * MB;
  const zx_ticks_t kReadDuration = 10 * ms;

  for (int i = 0; i < kNumOperations; i++) {
    read_metrics.IncrementDiskRead(CompressionAlgorithm::kUncompressed, kReadBytes,
                                   zx::ticks(kReadDuration));
  }

  stats = read_metrics.GetSnapshot(CompressionAlgorithm::kUncompressed);
  EXPECT_EQ(stats.read_bytes, kReadBytes * kNumOperations);
  EXPECT_EQ(stats.read_ticks, kReadDuration * kNumOperations);
}

TEST(ReadMetricsTest, ChunkedDecompression) {
  inspect::Node metrics_node;
  ReadMetrics read_metrics(&metrics_node);

  auto stats = read_metrics.GetSnapshot(CompressionAlgorithm::kChunked);
  EXPECT_EQ(stats.decompress_bytes, 0u);
  EXPECT_EQ(stats.decompress_ticks, 0);

  constexpr uint64_t kDecompressBytes = 1 * MB;
  const zx_ticks_t kDecompressDuration = 10 * ms;

  for (int i = 0; i < kNumOperations; i++) {
    read_metrics.IncrementDecompression(CompressionAlgorithm::kChunked, kDecompressBytes,
                                        zx::ticks(kDecompressDuration));
  }

  stats = read_metrics.GetSnapshot(CompressionAlgorithm::kChunked);
  EXPECT_EQ(stats.decompress_bytes, kDecompressBytes * kNumOperations);
  EXPECT_EQ(stats.decompress_ticks, kDecompressDuration * kNumOperations);
  EXPECT_EQ(read_metrics.GetRemoteDecompressions(), static_cast<uint64_t>(kNumOperations));
}

TEST(VerificationMetricsTest, MerkleVerifyMultithreaded) {
  VerificationMetrics verification_metrics;

  auto stats = verification_metrics.Get();
  EXPECT_EQ(stats.blobs_verified, 0ul);
  EXPECT_EQ(stats.data_size, 0ul);
  EXPECT_EQ(stats.merkle_size, 0ul);
  EXPECT_EQ(stats.verification_time, 0);

  constexpr uint64_t kDataBytes = 10 * MB, kMerkleBytes = 1 * MB;
  const zx_ticks_t kDuration = 2 * ms;

  std::array<std::thread, kNumThreads> threads;
  for (auto& thread : threads) {
    thread = std::thread(
        [&]() { verification_metrics.Increment(kDataBytes, kMerkleBytes, zx::ticks(kDuration)); });
  }

  for (auto& thread : threads) {
    thread.join();
  }

  stats = verification_metrics.Get();
  EXPECT_EQ(stats.blobs_verified, kNumThreads);
  EXPECT_EQ(stats.data_size, kDataBytes * kNumThreads);
  EXPECT_EQ(stats.merkle_size, kMerkleBytes * kNumThreads);
  EXPECT_EQ(stats.verification_time, static_cast<zx_ticks_t>(kDuration * kNumThreads));
}

class MetricsTest : public gtest::RealLoopFixture {
 public:
  MetricsTest() : executor_(dispatcher()) {}

  inspect::Hierarchy TakeSnapshot(inspect::testing::TreeClient tree) {
    bool done = false;
    inspect::Hierarchy hierarchy;

    executor_.schedule_task(inspect::testing::ReadFromTree(tree, dispatcher())
                                .and_then([&](inspect::Hierarchy& result) {
                                  hierarchy = std::move(result);
                                  done = true;
                                }));

    RunLoopUntil([&] { return done; });

    return hierarchy;
  }

  async::Executor executor_;
};

TEST_F(MetricsTest, PageInMetrics) {
  // Create the Metrics object (with page-in recording enabled) and record a page-in
  BlobfsMetrics metrics{true};
  metrics.IncrementPageIn("0123456789", 8192, 100);

  // Setup a connection to the Inspect VMO
  auto endpoints = fidl::CreateEndpoints<fuchsia_inspect::Tree>();
  inspect::TreeServer::StartSelfManagedServer(
      *metrics.inspector(),
      inspect::TreeHandlerSettings{.snapshot_behavior = inspect::TreeServerSendPreference::Frozen(
                                       inspect::TreeServerSendPreference::Type::DeepCopy)},
      dispatcher(), std::move(endpoints->server));

  // Take a snapshot of the tree and verify the hierarchy
  const inspect::Hierarchy hierarchy =
      TakeSnapshot(inspect::testing::TreeClient{std::move(endpoints->client), dispatcher()});

  const inspect::Hierarchy* blob_frequencies =
      hierarchy.GetByPath({"page_in_frequency_stats", "0123456789"});
  EXPECT_NE(blob_frequencies, nullptr);

  // Block index is counted in multiples of 8192
  const inspect::UintPropertyValue* frequency =
      blob_frequencies->node().get_property<inspect::UintPropertyValue>("1");
  EXPECT_NE(frequency, nullptr);
  EXPECT_EQ(frequency->value(), 1ul);
}

}  // namespace
}  // namespace blobfs
