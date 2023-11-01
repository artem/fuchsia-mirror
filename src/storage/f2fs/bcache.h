// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file describes the in-memory structures which construct
// a F2FS filesystem.

#ifndef SRC_STORAGE_F2FS_BCACHE_H_
#define SRC_STORAGE_F2FS_BCACHE_H_

#include <lib/zx/vmo.h>
#include <zircon/compiler.h>

#include <fbl/algorithm.h>
#include <fbl/array.h>
#include <fbl/macros.h>
#include <fbl/unique_fd.h>
#include <storage/buffer/vmo_buffer.h>
#include <storage/buffer/vmoid_registry.h>

#include "src/storage/f2fs/common.h"
#include "src/storage/lib/block_client/cpp/block_device.h"
#include "src/storage/lib/vfs/cpp/transaction/device_transaction_handler.h"

namespace f2fs {

class BcacheMapper;
// This function is called at startup to serve the actual working f2fs.
zx::result<std::unique_ptr<BcacheMapper>> CreateBcacheMapper(
    fidl::ClientEnd<fuchsia_hardware_block::Block> device, bool* out_readonly = nullptr);
// This function is used to create a f2fs instance for test.
zx::result<std::unique_ptr<BcacheMapper>> CreateBcacheMapper(
    std::unique_ptr<block_client::BlockDevice> device, bool* out_readonly = nullptr);
// This function is used to test the multi device support of f2fs.
zx::result<std::unique_ptr<BcacheMapper>> CreateBcacheMapper(
    std::vector<std::unique_ptr<block_client::BlockDevice>> devices, bool* out_readonly = nullptr);

class Bcache : public fs::DeviceTransactionHandler, public storage::VmoidRegistry {
 public:
  Bcache(const Bcache&) = delete;
  Bcache& operator=(const Bcache&) = delete;
  Bcache(Bcache&&) = delete;
  Bcache& operator=(Bcache&&) = delete;

  uint64_t Maxblk() const { return max_blocks_; }
  block_t BlockSize() const { return block_size_; }

  zx_status_t RunRequests(const std::vector<storage::BufferedOperation>& operations) override {
    return DeviceTransactionHandler::RunRequests(operations);
  }

  static zx::result<std::unique_ptr<Bcache>> Create(
      std::unique_ptr<block_client::BlockDevice> device, uint64_t max_blocks, block_t block_size);

  zx_status_t Flush() override { return DeviceTransactionHandler::Flush(); }

  zx_status_t BlockAttachVmo(const zx::vmo& vmo, storage::Vmoid* out) final;
  zx_status_t BlockDetachVmo(storage::Vmoid vmoid) final;

  uint64_t BlockNumberToDevice(uint64_t block_num) const final {
    return block_num * BlockSize() / info_.block_size;
  }

  block_client::BlockDevice* GetDevice() final { return device_.get(); }

 private:
  Bcache(std::unique_ptr<block_client::BlockDevice> device, uint64_t max_blocks,
         block_t block_size);

  // Used during initialization of this object.
  zx_status_t VerifyDeviceInfo();

  const uint64_t max_blocks_;
  const block_t block_size_;
  fuchsia_hardware_block::wire::BlockInfo info_ = {};
  std::unique_ptr<block_client::BlockDevice> device_;  // The device, if owned.
};

class BcacheMapper : public storage::VmoidRegistry {
 public:
  BcacheMapper(const BcacheMapper&) = delete;
  BcacheMapper& operator=(const BcacheMapper&) = delete;
  BcacheMapper(BcacheMapper&&) = delete;
  BcacheMapper& operator=(BcacheMapper&&) = delete;
  static zx::result<std::unique_ptr<BcacheMapper>> Create(
      std::vector<std::unique_ptr<Bcache>> bcaches);

  // Make a read/write operation from/to a 4KiB block at |bno|.
  // |buffer_| is used as a block buffer.
  zx_status_t Readblk(block_t bno, void* data) __TA_EXCLUDES(buffer_mutex_);
  zx_status_t Writeblk(block_t bno, const void* data) __TA_EXCLUDES(buffer_mutex_);

  zx_status_t Trim(block_t start, block_t num);
  uint64_t Maxblk() const { return max_blocks_; }
  block_t BlockSize() const { return block_size_; }
  zx_status_t BlockGetInfo(fuchsia_hardware_block::wire::BlockInfo* out_info) const;

  zx_status_t RunRequests(const std::vector<storage::BufferedOperation>& operations);
  zx_status_t Flush();

  zx_status_t BlockAttachVmo(const zx::vmo& vmo, storage::Vmoid* out) override;
  zx_status_t BlockDetachVmo(storage::Vmoid vmoid) override;

  // This function is only used for test and InspectTree purposes.
  void ForEachBcache(fit::function<void(Bcache*)> func) {
    for (auto& bcache : bcaches_) {
      func(bcache.get());
    }
  }

 private:
  explicit BcacheMapper(std::vector<std::unique_ptr<Bcache>> bcaches, uint64_t max_blocks,
                        block_t block_size);

  zx::result<vmoid_t> FindFreeVmoId();

  zx_status_t CreateVmoBuffer() __TA_EXCLUDES(buffer_mutex_) {
    std::lock_guard lock(buffer_mutex_);
    return buffer_.Initialize(this, 1, info_.block_size, "scratch-block");
  }

  const std::vector<std::unique_ptr<Bcache>> bcaches_;

  // |vmoid_tree_| is a map between the virtual vmoid seen by F2FS and the vmoid actually registered
  // on the underlying device.
  std::map<vmoid_t, std::vector<storage::Vmoid>> vmoid_tree_;
  vmoid_t last_id_ = BLOCK_VMOID_INVALID + 1;

  std::mutex buffer_mutex_;
  // |buffer_| and |buffer_mutex_| are used in the "Readblk/Writeblk" methods.
  storage::VmoBuffer buffer_ __TA_GUARDED(buffer_mutex_);

  fuchsia_hardware_block::wire::BlockInfo info_ = {};
  const block_t block_size_;
  const uint64_t max_blocks_;
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_BCACHE_H_
