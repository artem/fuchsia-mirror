// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/blob_verifier.h"

#include <memory>
#include <random>

#include <gtest/gtest.h>

#include "src/lib/digest/merkle-tree.h"
#include "src/storage/blobfs/blob_layout.h"
#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/blobfs/test/unit/utils.h"

namespace blobfs {
namespace {

struct BlockMerkleTreeInfo {
  fzl::OwnedVmoMapper blocks;
  // Points into |blocks|, will be offset according to blob format.
  uint8_t* merkle_data = nullptr;
  Digest root;

  cpp20::span<const uint8_t> GetMerkleDataBlocks() const {
    return cpp20::span(static_cast<const uint8_t*>(blocks.start()), blocks.size());
  }
};

class BlobVerifierTest : public testing::TestWithParam<BlobLayoutFormat> {
 public:
  std::shared_ptr<BlobfsMetrics> GetMetrics() { return metrics_; }

  void SetUp() override { srand(testing::UnitTest::GetInstance()->random_seed()); }

  // Creates a default blob layout for an uncompressed file of the given size.
  static std::unique_ptr<BlobLayout> GetBlobLayout(size_t size) {
    auto layout_or = BlobLayout::CreateFromSizes(GetParam(), size, size, kBlobfsBlockSize);
    EXPECT_TRUE(layout_or.is_ok());  // Should always succeed in this use.
    return std::move(*layout_or);
  }

  static std::unique_ptr<MerkleTreeInfo> GenerateTree(const uint8_t* data, size_t len) {
    return CreateMerkleTree(data, len, ShouldUseCompactMerkleTreeFormat(GetParam()));
  }

  // Like GenerateTree but puts the merkle data into a VMO that will have the merkle data aligned
  // inside of it as it would on disk according to the given layout.
  static BlockMerkleTreeInfo GenerateMerkleTreeBlocks(const BlobLayout& layout, const uint8_t* data,
                                                      size_t len) {
    std::unique_ptr<MerkleTreeInfo> normal_info = GenerateTree(data, len);

    BlockMerkleTreeInfo block_info;
    EXPECT_EQ(ZX_OK,
              block_info.blocks.CreateAndMap(layout.MerkleTreeBlockAlignedSize(), "Merkle blocks"));

    auto start_offset = layout.MerkleTreeOffsetWithinBlockOffset();
    EXPECT_LE(normal_info->merkle_tree_size + start_offset, block_info.blocks.size());

    block_info.merkle_data = &static_cast<uint8_t*>(block_info.blocks.start())[start_offset];
    memcpy(block_info.merkle_data, normal_info->merkle_tree.get(), normal_info->merkle_tree_size);

    block_info.root = normal_info->root;
    return block_info;
  }

 private:
  std::shared_ptr<BlobfsMetrics> metrics_ = std::make_shared<BlobfsMetrics>(false);
};

void FillWithRandom(uint8_t* buf, size_t len) {
  for (unsigned i = 0; i < len; ++i) {
    buf[i] = static_cast<uint8_t>(rand());
  }
}

TEST_P(BlobVerifierTest, CreateAndVerify_NullBlob) {
  auto merkle_tree = GenerateTree(nullptr, 0);

  auto verifier_or = BlobVerifier::CreateWithoutTree(merkle_tree->root, GetMetrics(), 0ul);
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  EXPECT_EQ(verifier->Verify(nullptr, 0ul, 0ul), ZX_OK);
  EXPECT_EQ(verifier->VerifyPartial(nullptr, 0ul, 0ul, 0ul), ZX_OK);
}

TEST_P(BlobVerifierTest, CreateAndVerify_SmallBlob) {
  uint8_t buf[8192];
  FillWithRandom(buf, sizeof(buf));

  auto merkle_tree = GenerateTree(buf, sizeof(buf));

  auto verifier_or = BlobVerifier::CreateWithoutTree(merkle_tree->root, GetMetrics(), sizeof(buf));
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  EXPECT_EQ(verifier->Verify(buf, sizeof(buf), sizeof(buf)), ZX_OK);

  EXPECT_EQ(verifier->VerifyPartial(buf, 8192, 0, 8192), ZX_OK);

  // Partial ranges
  EXPECT_EQ(verifier->VerifyPartial(buf, 8191, 0, 8191), ZX_ERR_INVALID_ARGS);

  // Verify past the end
  EXPECT_EQ(
      verifier->VerifyPartial(buf, static_cast<size_t>(2) * 8192, 0, static_cast<size_t>(2) * 8192),
      ZX_ERR_INVALID_ARGS);
}

TEST_P(BlobVerifierTest, CreateAndVerify_SmallBlob_DataCorrupted) {
  uint8_t buf[8192];
  FillWithRandom(buf, sizeof(buf));

  auto merkle_tree = GenerateTree(buf, sizeof(buf));

  // Invert one character
  buf[42] = ~(buf[42]);

  auto verifier_or = BlobVerifier::CreateWithoutTree(merkle_tree->root, GetMetrics(), sizeof(buf));
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  EXPECT_EQ(verifier->Verify(buf, sizeof(buf), sizeof(buf)), ZX_ERR_IO_DATA_INTEGRITY);
  EXPECT_EQ(verifier->VerifyPartial(buf, 8192, 0, 8192), ZX_ERR_IO_DATA_INTEGRITY);
}

TEST_P(BlobVerifierTest, CreateAndVerify_BigBlob) {
  size_t sz = 1 << 16;
  fbl::Array<uint8_t> buf(new uint8_t[sz], sz);
  FillWithRandom(buf.get(), sz);

  auto layout = GetBlobLayout(sz);
  BlockMerkleTreeInfo info = GenerateMerkleTreeBlocks(*layout, buf.get(), sz);

  auto verifier_or =
      BlobVerifier::Create(info.root, GetMetrics(), info.GetMerkleDataBlocks(), *layout);
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  EXPECT_EQ(verifier->Verify(buf.get(), sz, sz), ZX_OK);
  EXPECT_EQ(verifier->VerifyPartial(buf.get(), sz, 0, sz), ZX_OK);

  // Block-by-block
  for (size_t i = 0; i < sz; i += 8192) {
    EXPECT_EQ(verifier->VerifyPartial(buf.get() + i, 8192, i, 8192), ZX_OK);
  }

  // Partial ranges
  EXPECT_EQ(verifier->VerifyPartial(buf.data(), 8191, 0, 8191), ZX_ERR_INVALID_ARGS);

  // Verify past the end
  EXPECT_EQ(verifier->VerifyPartial(buf.data() + (sz - 8192), static_cast<size_t>(2) * 8192,
                                    sz - 8192, static_cast<size_t>(2) * 8192),
            ZX_ERR_INVALID_ARGS);
}

TEST_P(BlobVerifierTest, CreateAndVerify_BigBlob_DataCorrupted) {
  size_t sz = 1 << 16;
  fbl::Array<uint8_t> buf(new uint8_t[sz], sz);
  FillWithRandom(buf.get(), sz);

  auto layout = GetBlobLayout(sz);
  BlockMerkleTreeInfo info = GenerateMerkleTreeBlocks(*layout, buf.get(), sz);

  // Invert a char in the first block. All other blocks are still valid.
  buf.get()[42] = ~(buf.get()[42]);

  auto verifier_or =
      BlobVerifier::Create(info.root, GetMetrics(), info.GetMerkleDataBlocks(), *layout);
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  EXPECT_EQ(verifier->Verify(buf.get(), sz, sz), ZX_ERR_IO_DATA_INTEGRITY);
  EXPECT_EQ(verifier->VerifyPartial(buf.get(), sz, 0, sz), ZX_ERR_IO_DATA_INTEGRITY);

  // Block-by-block -- first block fails, rest succeed
  for (size_t i = 0; i < sz; i += 8192) {
    zx_status_t status = verifier->VerifyPartial(buf.get() + i, 8192, i, 8192);
    if (i == 0) {
      EXPECT_EQ(status, ZX_ERR_IO_DATA_INTEGRITY);
    } else {
      EXPECT_EQ(status, ZX_OK);
    }
  }
}

TEST_P(BlobVerifierTest, CreateAndVerify_BigBlob_MerkleCorrupted) {
  size_t sz = 1 << 16;
  fbl::Array<uint8_t> buf(new uint8_t[sz], sz);
  FillWithRandom(buf.get(), sz);

  auto layout = GetBlobLayout(sz);
  BlockMerkleTreeInfo info = GenerateMerkleTreeBlocks(*layout, buf.get(), sz);

  // Invert a char in the tree.
  info.merkle_data[0] ^= 0xff;

  auto verifier_or =
      BlobVerifier::Create(info.root, GetMetrics(), info.GetMerkleDataBlocks(), *layout);
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  EXPECT_EQ(verifier->Verify(buf.get(), sz, sz), ZX_ERR_IO_DATA_INTEGRITY);
  EXPECT_EQ(verifier->VerifyPartial(buf.get(), sz, 0, sz), ZX_ERR_IO_DATA_INTEGRITY);

  // Block-by-block -- everything fails
  for (size_t i = 0; i < sz; i += 8192) {
    EXPECT_EQ(verifier->VerifyPartial(buf.get() + i, 8192, i, 8192), ZX_ERR_IO_DATA_INTEGRITY);
  }
}

TEST_P(BlobVerifierTest, NonZeroTailCausesVerifyToFail) {
  constexpr int kBlobSize = 8000;
  uint8_t buf[kBlobfsBlockSize];
  FillWithRandom(buf, kBlobSize);
  // Zero the tail.
  memset(&buf[kBlobSize], 0, kBlobfsBlockSize - kBlobSize);

  auto merkle_tree = GenerateTree(buf, kBlobSize);

  auto verifier_or = BlobVerifier::CreateWithoutTree(merkle_tree->root, GetMetrics(), kBlobSize);
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  EXPECT_EQ(verifier->Verify(buf, kBlobSize, sizeof(buf)), ZX_OK);

  buf[kBlobSize] = 1;
  EXPECT_EQ(verifier->Verify(buf, kBlobSize, sizeof(buf)), ZX_ERR_IO_DATA_INTEGRITY);
}

TEST_P(BlobVerifierTest, NonZeroTailCausesVerifyPartialToFail) {
  constexpr unsigned kBlobSize = (1 << 16) - 100;
  std::vector<uint8_t> buf(fbl::round_up(kBlobSize, kBlobfsBlockSize));
  FillWithRandom(buf.data(), kBlobSize);

  auto layout = GetBlobLayout(kBlobSize);
  BlockMerkleTreeInfo info = GenerateMerkleTreeBlocks(*layout, buf.data(), kBlobSize);

  auto verifier_or =
      BlobVerifier::Create(info.root, GetMetrics(), info.GetMerkleDataBlocks(), *layout);
  ASSERT_TRUE(verifier_or.is_ok());
  BlobVerifier* verifier = verifier_or.value().get();

  constexpr int kVerifyOffset = kBlobSize - kBlobSize % kBlobfsBlockSize;
  EXPECT_EQ(verifier->VerifyPartial(&buf[kVerifyOffset], kBlobSize - kVerifyOffset, kVerifyOffset,
                                    buf.size() - kVerifyOffset),
            ZX_OK);

  buf[kBlobSize] = 1;
  EXPECT_EQ(verifier->VerifyPartial(&buf[kVerifyOffset], kBlobSize - kVerifyOffset, kVerifyOffset,
                                    buf.size() - kVerifyOffset),
            ZX_ERR_IO_DATA_INTEGRITY);
}

std::string GetTestName(const testing::TestParamInfo<BlobLayoutFormat>& param) {
  return GetBlobLayoutFormatNameForTests(param.param);
}

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, BlobVerifierTest,
                         ::testing::Values(BlobLayoutFormat::kDeprecatedPaddedMerkleTreeAtStart,
                                           BlobLayoutFormat::kCompactMerkleTreeAtEnd),
                         GetTestName);

}  // namespace
}  // namespace blobfs
