// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_ZIRCON_BIN_HWSTRESS_FLASH_STRESS_H_
#define SRC_ZIRCON_BIN_HWSTRESS_FLASH_STRESS_H_

#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fuchsia/hardware/block/cpp/fidl.h>
#include <lib/zx/fifo.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>

#include <string>

#include <src/lib/uuid/uuid.h>

#include "args.h"
#include "status.h"

namespace hwstress {

// The GPT partition type used for partitions created by the flash test.
constexpr uuid::Uuid kTestPartGUID = uuid::Uuid({0xC6, 0x24, 0xF5, 0xDD, 0x9D, 0x88, 0x4C, 0x81,
                                                 0x99, 0x87, 0xCA, 0x92, 0xD1, 0x1B, 0x28, 0x89});

// Creates and manages the lifetime of a new partition backed by a
// Fuchsia Volume Manager instance.
class TemporaryFvmPartition {
 public:
  ~TemporaryFvmPartition();

  // Create a new partition with the number of slices indicated in |slices_requested|.
  //
  // Returns nullptr on failure.
  static std::unique_ptr<TemporaryFvmPartition> Create(
      fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager> client_end,
      uint64_t slices_requested);

  // Get the path to the created partition.
  std::string GetPartitionPath();

 private:
  std::string partition_path_;
  uuid::Uuid unique_guid_;

  TemporaryFvmPartition(std::string partition_path, uuid::Uuid unique_guid);
};

// Start a stress test.
bool StressFlash(StatusLine* status, const CommandLineArgs& args, zx::duration duration);

// Delete any persistent flash test partitions
void DestroyFlashTestPartitions(StatusLine* status);

// Exposed for testing only.

struct BlockDevice {
  fuchsia::hardware::block::SessionSyncPtr device;  // Connection to the block device.
  zx::fifo fifo;                                    // FIFO used to read/write to the block device.
  fuchsia::hardware::block::BlockInfo info;         // Details about the block device.
  zx::vmo vmo;                                      // Shared VMO with the block device.
  zx_vaddr_t vmo_addr;                              // Where |vmo| is mapped into our address space.
  size_t vmo_size;                                  // Size of |vmo| in bytes.
  fuchsia::hardware::block::VmoId vmoid;            // Identifier the used to refer to the VMO
                                                    // when communicating with the block device.
};

zx_status_t SetupBlockFifo(const std::string& path, BlockDevice* device);

zx_status_t FlashIo(const BlockDevice& device, size_t bytes_to_test, size_t transfer_size,
                    bool is_write_test);

}  // namespace hwstress

#endif  // SRC_ZIRCON_BIN_HWSTRESS_FLASH_STRESS_H_
