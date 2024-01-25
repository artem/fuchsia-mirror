// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/fvm/metadata.h"

#include <lib/stdcompat/span.h>
#include <zircon/errors.h>

#include <random>
#include <vector>

#include <zxtest/zxtest.h>

#include "src/storage/fvm/format.h"
#include "src/storage/fvm/fvm.h"

namespace fvm {

static std::default_random_engine rand(::zxtest::Runner::GetInstance()->random_seed());

SliceEntry CreateSliceEntry(uint16_t vpart) {
  // Slices are 1-indexed.
  // TODO(https://fxbug.dev/42138108) include the zero entry, too.
  static uint64_t vslice = 1;
  return SliceEntry(vpart, vslice++);
}

VPartitionEntry CreatePartitionEntry(size_t slices) {
  uint8_t type[sizeof(VPartitionEntry::type)];
  uint8_t guid[sizeof(VPartitionEntry::type)];
  uint8_t name[sizeof(VPartitionEntry::unsafe_name)];

  std::uniform_int_distribution<uint16_t> d('a', 'z');
  auto g = [&]() { return static_cast<uint8_t>(d(rand)); };
  std::generate(std::begin(type), std::end(type), g);
  std::generate(std::begin(guid), std::end(guid), g);
  std::generate(std::begin(name), std::end(name), g);
  name[sizeof(name) - 1] = '\0';

  std::uniform_int_distribution<uint32_t> d2;
  uint32_t flags = d2(rand);

  return VPartitionEntry(type, guid, static_cast<uint32_t>(slices),
                         VPartitionEntry::StringFromArray(name), flags);
}

void ValidateMetadata(Metadata& metadata, const std::vector<VPartitionEntry>& expected_partitions,
                      const std::vector<SliceEntry>& expected_slices) {
  const Header& header = metadata.GetHeader();

  // Zeroth entry must not be used.
  EXPECT_TRUE(metadata.GetPartitionEntry(0).IsFree());
  unsigned i = 0;
  ASSERT_GE(header.GetPartitionTableEntryCount(), expected_partitions.size());
  for (; i < expected_partitions.size(); ++i) {
    EXPECT_BYTES_EQ(&expected_partitions[i], &metadata.GetPartitionEntry(i + 1),
                    sizeof(VPartitionEntry));
  }
  i++;  // We already checked [i+1] in the above loop
  for (; i < header.GetPartitionTableEntryCount(); ++i) {
    EXPECT_TRUE(metadata.GetPartitionEntry(i).IsFree());
  }

  // Zeroth entry must not be used.
  EXPECT_TRUE(metadata.GetSliceEntry(0).IsFree());
  ASSERT_GE(header.GetAllocationTableUsedEntryCount(), expected_slices.size());
  for (i = 0; i < expected_slices.size(); ++i) {
    EXPECT_BYTES_EQ(&expected_slices[i], &metadata.GetSliceEntry(i + 1), sizeof(SliceEntry));
  }
  i++;  // We already checked [i+1] in the above loop
  for (; i < header.GetAllocationTableUsedEntryCount(); ++i) {
    EXPECT_TRUE(metadata.GetSliceEntry(i).IsFree());
  }
}

void CheckMetadataContainSameEntries(const Metadata& a, const Metadata& b) {
  const Header& header_a = a.GetHeader();
  const Header& header_b = b.GetHeader();

  size_t i = 1, j = 1;
  for (; i < header_a.GetPartitionTableEntryCount() && j < header_b.GetPartitionTableEntryCount();
       ++i, ++j) {
    EXPECT_BYTES_EQ(&a.GetPartitionEntry(i), &b.GetPartitionEntry(j), sizeof(VPartitionEntry));
  }
  for (; i < header_a.GetPartitionTableEntryCount(); ++i) {
    EXPECT_FALSE(a.GetPartitionEntry(i).IsAllocated());
  }
  for (; j < header_b.GetPartitionTableEntryCount(); ++j) {
    EXPECT_FALSE(b.GetPartitionEntry(j).IsAllocated());
  }

  i = 1, j = 1;
  for (; i < header_a.GetAllocationTableUsedEntryCount() &&
         j < header_b.GetAllocationTableUsedEntryCount();
       ++i, ++j) {
    EXPECT_BYTES_EQ(&a.GetSliceEntry(i), &b.GetSliceEntry(j), sizeof(SliceEntry));
  }
  for (; i < header_a.GetAllocationTableUsedEntryCount(); ++i) {
    EXPECT_FALSE(a.GetSliceEntry(i).IsAllocated());
  }
  for (; j < header_b.GetAllocationTableUsedEntryCount(); ++j) {
    EXPECT_FALSE(b.GetSliceEntry(j).IsAllocated());
  }
}

// TODO(https://fxbug.dev/42116137): Re-enable this test when partition table size is configurable.
/*
TEST(CreateMetadata, HeaderPartitionTableCapacityTooSmallFails) {
  constexpr size_t kSliceSize = 32 * 1024;
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(0, kSlices, kSliceSize);

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(0u),
  };
  std::vector<SliceEntry> slices;
  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_NE(result.status_value(), ZX_OK);
}
*/

TEST(CreateMetadata, HeaderSliceTableCapacityTooSmallFails) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, 0, kSliceSize);

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(1u),
  };
  std::vector<SliceEntry> slices{
      CreateSliceEntry(1u),
  };
  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_NE(result.status_value(), ZX_OK);
}

TEST(CreateMetadata, HeaderHasZeroSizedSlicesFails) {
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, 0u);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_NE(result.status_value(), ZX_OK);
}

TEST(CreateMetadata, HeaderHasBadMagicFails) {
  constexpr size_t kSlices = 1024;
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);
  header.magic = 0u;

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_NE(result.status_value(), ZX_OK);
}

TEST(CreateMetadata, HeaderHasBadVersionFails) {
  constexpr size_t kSlices = 1024;
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);
  header.major_version = kCurrentMajorVersion + 1;

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_NE(result.status_value(), ZX_OK);
}

TEST(CreateMetadata, ZeroSizedSliceTable) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, 0, kSliceSize);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_EQ(result.status_value(), ZX_OK);
  EXPECT_EQ(result->GetHeader().GetAllocationTableUsedEntryCount(), 0);
}

TEST(CreateMetadata, NoPartitionsAndSlices) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<VPartitionEntry> partitions;
  std::vector<SliceEntry> slices;
  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());
  ValidateMetadata(result.value(), partitions, slices);
}

TEST(CreateMetadata, OnePartitionNoSlices) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices;
  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(0),
  };
  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());
  ValidateMetadata(result.value(), partitions, slices);
}

TEST(CreateMetadata, OnePartitionNoSlicesOverloadCheck) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices;
  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(0),
  };
  auto result = Metadata::Synthesize(header, partitions, slices);
  ASSERT_TRUE(result.is_ok());
  ValidateMetadata(result.value(), partitions, slices);
}

TEST(CreateMetadata, SeveralPartitionsAndSlices) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices(8);
  std::generate(slices.begin(), slices.begin() + 5, []() { return CreateSliceEntry(1u); });
  std::generate(slices.begin() + 5, slices.end(), []() { return CreateSliceEntry(3u); });

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(5),
      CreatePartitionEntry(0),
      CreatePartitionEntry(3),
  };

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());
  ValidateMetadata(result.value(), partitions, slices);
}

TEST(MoveMetadata, EmptyInstance) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, 0, kSliceSize);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_TRUE(result.is_ok());
  ASSERT_NE(result->Get(), nullptr);

  Metadata metadata = std::move(result.value());
  EXPECT_EQ(result->Get(), nullptr);
  EXPECT_NE(metadata.Get(), nullptr);
}

TEST(MoveMetadata, NonemptyInstance) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices(8);
  std::generate(slices.begin(), slices.begin() + 5, []() { return CreateSliceEntry(1u); });
  std::generate(slices.begin() + 5, slices.end(), []() { return CreateSliceEntry(3u); });

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(5),
      CreatePartitionEntry(0),
      CreatePartitionEntry(3),
  };

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());
  ASSERT_NE(result->Get(), nullptr);

  Metadata metadata = std::move(result.value());
  EXPECT_EQ(result->Get(), nullptr);
  EXPECT_NE(metadata.Get(), nullptr);
  ValidateMetadata(metadata, partitions, slices);
}

TEST(CopyMetadata, SmallerDimensionsFails) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, kSlices - 1, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_error());
}

TEST(CopyMetadata, MetadataWithZeroSlicesToSameDimensions) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, 0, kSliceSize);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, 0, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
}

TEST(CopyMetadata, MetadataWithZeroSlicesToBiggerDimensions) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, 0, kSliceSize);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, 1024, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
  ASSERT_EQ(copy_result->GetHeader().GetAllocationTableUsedEntryCount(), 1024);
}

TEST(CopyMetadata, EmptyMetadataSameDimensions) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
}

TEST(CopyMetadata, EmptyMetadataBiggerDimensions) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  auto result = Metadata::Synthesize(header, nullptr, 0, nullptr, 0);
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, 2 * kSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
}

TEST(CopyMetadata, NonemptyMetadataSameDimensions) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices(8);
  std::generate(slices.begin(), slices.begin() + 5, []() { return CreateSliceEntry(1u); });
  std::generate(slices.begin() + 5, slices.end(), []() { return CreateSliceEntry(3u); });

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(5),
      CreatePartitionEntry(0),
      CreatePartitionEntry(3),
  };

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
}

TEST(CopyMetadata, NonemptyMetadataBiggerDimensions) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices(8);
  std::generate(slices.begin(), slices.begin() + 5, []() { return CreateSliceEntry(1u); });
  std::generate(slices.begin() + 5, slices.end(), []() { return CreateSliceEntry(3u); });

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(5),
      CreatePartitionEntry(0),
      CreatePartitionEntry(3),
  };

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, 2 * kSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
}

TEST(CopyMetadata, CopyAllocationTableWithEnoughPadding) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  constexpr size_t kMaxSlices{4 * kSlices};
  Header header =
      Header::FromGrowableSliceCount(kMaxUsablePartitions, kSlices, kMaxSlices, kSliceSize);

  std::vector<SliceEntry> slices(8);
  std::generate(slices.begin(), slices.begin() + 5, []() { return CreateSliceEntry(1u); });
  std::generate(slices.begin() + 5, slices.end(), []() { return CreateSliceEntry(3u); });

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(5),
      CreatePartitionEntry(0),
      CreatePartitionEntry(3),
  };

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, kMaxSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
  EXPECT_EQ(copy_result->GetHeader().GetMetadataAllocatedBytes(),
            header.GetMetadataAllocatedBytes());
  EXPECT_GT(copy_result->GetHeader().GetMetadataUsedBytes(), header.GetMetadataUsedBytes());
}

TEST(CopyMetadata, CopyAllocationTableWithoutEnoughPadding) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  constexpr size_t kMaxSlices{4 * kSlices};
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices(8);
  std::generate(slices.begin(), slices.begin() + 5, []() { return CreateSliceEntry(1u); });
  std::generate(slices.begin() + 5, slices.end(), []() { return CreateSliceEntry(3u); });

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(5),
      CreatePartitionEntry(0),
      CreatePartitionEntry(3),
  };

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, kMaxSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
  EXPECT_GT(copy_result->GetHeader().GetMetadataAllocatedBytes(),
            header.GetMetadataAllocatedBytes());
  EXPECT_GT(copy_result->GetHeader().GetMetadataUsedBytes(), header.GetMetadataUsedBytes());
}

TEST(CopyMetadata, CopyFullPartitionTable) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices;

  // Technically none of these partitions have any slices in the allocation table, but FVM doesn't
  // check this.
  std::vector<VPartitionEntry> partitions(kMaxUsablePartitions);
  std::generate(partitions.begin(), partitions.end(), []() { return CreatePartitionEntry(1); });

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
}

TEST(CopyMetadata, CopyFullAllocationTable) {
  constexpr size_t kSliceSize{static_cast<size_t>(32) * 1024};
  constexpr size_t kSlices = 1024;
  Header header = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);

  std::vector<SliceEntry> slices(kSlices);
  std::generate(slices.begin(), slices.end(), []() { return CreateSliceEntry(1u); });

  std::vector<VPartitionEntry> partitions{
      CreatePartitionEntry(kSlices),
  };

  auto result = Metadata::Synthesize(header, partitions.data(), partitions.size(), slices.data(),
                                     slices.size());
  ASSERT_TRUE(result.is_ok());

  Header dimensions = Header::FromSliceCount(kMaxUsablePartitions, kSlices, kSliceSize);
  auto copy_result = result.value().CopyWithNewDimensions(dimensions);
  ASSERT_TRUE(copy_result.is_ok());
  CheckMetadataContainSameEntries(result.value(), copy_result.value());
}

}  // namespace fvm
