// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/magma_service/mock/fake_address_space.h>
#include <lib/magma_service/mock/mock_bus_mapper.h>

#include <gtest/gtest.h>

#include "address_space.h"
#include "instructions.h"
#include "ringbuffer.h"

using AllocatingAddressSpace = FakeAllocatingAddressSpace<GpuMapping, AddressSpace>;

class TestRingbuffer {
 public:
  static uint32_t* vaddr(Ringbuffer* ringbuffer) { return ringbuffer->vaddr(); }
};

class TestInstructions : public testing::Test {
 public:
  class AddressSpaceOwner : public magma::AddressSpaceOwner {
   public:
    virtual ~AddressSpaceOwner() = default;
    magma::PlatformBusMapper* GetBusMapper() override { return &bus_mapper_; }

   private:
    MockBusMapper bus_mapper_;
  };

  void SetUp() override {
    ringbuffer_ = std::make_unique<Ringbuffer>(MsdIntelBuffer::Create(PAGE_SIZE, "test"));
    address_space_owner_ = std::make_unique<AddressSpaceOwner>();
    address_space_ = std::make_shared<AllocatingAddressSpace>(address_space_owner_.get(), 0x10000,
                                                              ringbuffer_->size());

    gpu_addr_t gpu_addr;
    ASSERT_TRUE(ringbuffer_->Map(address_space_, &gpu_addr));
  }

  // order of destruction important so gpu mappings can access the address space
  std::unique_ptr<AddressSpaceOwner> address_space_owner_;
  std::shared_ptr<AddressSpace> address_space_;
  std::unique_ptr<Ringbuffer> ringbuffer_;
};

TEST_F(TestInstructions, Noop) {
  uint32_t* vaddr = TestRingbuffer::vaddr(ringbuffer_.get());

  MiNoop::write(ringbuffer_.get());

  EXPECT_EQ(*vaddr, 0u);
}

TEST_F(TestInstructions, BatchBufferStart) {
  ASSERT_EQ((int)MiBatchBufferStart::kDwordCount, 3);

  uint32_t tail_start = ringbuffer_->tail();

  uint32_t* vaddr = TestRingbuffer::vaddr(ringbuffer_.get()) + tail_start / sizeof(uint32_t);

  gpu_addr_t gpu_addr = 0xabcd1234cafebeef;
  MiBatchBufferStart::write(ringbuffer_.get(), gpu_addr, ADDRESS_SPACE_PPGTT);

  EXPECT_EQ(ringbuffer_->tail() - tail_start, MiBatchBufferStart::kDwordCount * sizeof(uint32_t));
  EXPECT_EQ(*vaddr++, MiBatchBufferStart::kCommandType | (MiBatchBufferStart::kDwordCount - 2) |
                          MiBatchBufferStart::kAddressSpacePpgtt);
  EXPECT_EQ(*vaddr++, magma::lower_32_bits(gpu_addr));
  EXPECT_EQ(*vaddr++, magma::upper_32_bits(gpu_addr));

  gpu_addr = 0xaa00bb00cc00dd;

  MiBatchBufferStart::write(ringbuffer_.get(), gpu_addr, ADDRESS_SPACE_GGTT);
  EXPECT_EQ(ringbuffer_->tail() - tail_start,
            2 * MiBatchBufferStart::kDwordCount * sizeof(uint32_t));
  EXPECT_EQ(*vaddr++, MiBatchBufferStart::kCommandType | (MiBatchBufferStart::kDwordCount - 2));
  EXPECT_EQ(*vaddr++, magma::lower_32_bits(gpu_addr));
  EXPECT_EQ(*vaddr++, magma::upper_32_bits(gpu_addr));
}

TEST_F(TestInstructions, PipeControl) {
  ASSERT_EQ((int)MiPipeControl::kDwordCount, 6);

  uint32_t tail_start = ringbuffer_->tail();

  uint32_t* vaddr = TestRingbuffer::vaddr(ringbuffer_.get()) + tail_start / sizeof(uint32_t);

  gpu_addr_t gpu_addr = 0xabcd1234cafebeef;

  uint32_t sequence_number = 0xdeadbeef;
  uint32_t flags = MiPipeControl::kCommandStreamerStallEnableBit |
                   MiPipeControl::kIndirectStatePointersDisableBit |
                   MiPipeControl::kGenericMediaStateClearBit | MiPipeControl::kDcFlushEnableBit;

  MiPipeControl::write(ringbuffer_.get(), sequence_number, gpu_addr, flags);

  EXPECT_EQ(ringbuffer_->tail() - tail_start, MiPipeControl::kDwordCount * sizeof(uint32_t));
  EXPECT_EQ(0x7A000000u | (MiPipeControl::kDwordCount - 2), *vaddr++);
  EXPECT_EQ(
      flags | MiPipeControl::kPostSyncWriteImmediateBit | MiPipeControl::kAddressSpaceGlobalGttBit,
      *vaddr++);
  EXPECT_EQ(magma::lower_32_bits(gpu_addr), *vaddr++);
  EXPECT_EQ(magma::upper_32_bits(gpu_addr), *vaddr++);
  EXPECT_EQ(sequence_number, *vaddr++);
  EXPECT_EQ(0u, *vaddr++);
}

TEST_F(TestInstructions, Flush) {
  ASSERT_EQ(MiFlush::kDwordCount, 5u);

  constexpr uint64_t kSomeAddr = 0xabcd1234cafebef8 & ~(1 << 5);  // bit 5 set not allowed
  constexpr uint32_t kSomeData = 0xfecba098;

  {
    uint32_t tail_start = ringbuffer_->tail();
    uint32_t* vaddr = TestRingbuffer::vaddr(ringbuffer_.get()) + tail_start / sizeof(uint32_t);

    MiFlush::write(ringbuffer_.get(), kSomeData, ADDRESS_SPACE_GGTT, kSomeAddr);

    EXPECT_EQ(ringbuffer_->tail() - tail_start, MiFlush::kDwordCount * sizeof(uint32_t));
    EXPECT_EQ(*vaddr++, MiFlush::kCommandType | MiFlush::kCommandOpcode |
                            MiFlush::kPostSyncWriteImmediateBit | (MiFlush::kDwordCount - 2));
    EXPECT_EQ(*vaddr++, magma::lower_32_bits(kSomeAddr) | MiFlush::kAddressSpaceGlobalGttBit);
    EXPECT_EQ(*vaddr++, magma::upper_32_bits(kSomeAddr));
    EXPECT_EQ(*vaddr++, magma::lower_32_bits(kSomeData));
    EXPECT_EQ(*vaddr++, magma::upper_32_bits(kSomeData));
  }
  {
    uint32_t tail_start = ringbuffer_->tail();
    uint32_t* vaddr = TestRingbuffer::vaddr(ringbuffer_.get()) + tail_start / sizeof(uint32_t);

    MiFlush::write(ringbuffer_.get(), kSomeData, ADDRESS_SPACE_PPGTT, kSomeAddr);

    EXPECT_EQ(ringbuffer_->tail() - tail_start, MiFlush::kDwordCount * sizeof(uint32_t));
    EXPECT_EQ(*vaddr++, MiFlush::kCommandType | MiFlush::kCommandOpcode |
                            MiFlush::kPostSyncWriteImmediateBit | (MiFlush::kDwordCount - 2));
    EXPECT_EQ(*vaddr++, magma::lower_32_bits(kSomeAddr));
    EXPECT_EQ(*vaddr++, magma::upper_32_bits(kSomeAddr));
    EXPECT_EQ(*vaddr++, magma::lower_32_bits(kSomeData));
    EXPECT_EQ(*vaddr++, magma::upper_32_bits(kSomeData));
  }
}
