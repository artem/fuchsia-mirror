// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/function.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/channel.h>
#include <lib/zx/fifo.h>
#include <lib/zx/vmo.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <threads.h>
#include <time.h>
#include <unistd.h>
#include <utime.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <algorithm>
#include <climits>
#include <iterator>
#include <limits>
#include <memory>
#include <new>
#include <utility>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <fbl/unique_fd.h>
#include <fbl/vector.h>
#include <ramdevice-client/ramdisk.h>
#include <zxtest/zxtest.h>

#include "lib/fidl/cpp/wire/internal/transport_channel.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/fvm/format.h"
#include "src/storage/fvm/fvm_check.h"
#include "src/storage/lib/block_client/cpp/client.h"
#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/fs_management/cpp/admin.h"
#include "src/storage/lib/fs_management/cpp/fvm.h"
#include "src/storage/lib/fs_management/cpp/mount.h"
#include "src/storage/minfs/format.h"

constexpr char kFvmDriverLib[] = "fvm.cm";
#define STRLEN(s) (sizeof(s) / sizeof((s)[0]))

namespace {

using VolumeManagerInfo = fuchsia_hardware_block_volume::wire::VolumeManagerInfo;

constexpr char kMountPath[] = "/test/minfs_test_mountpath";
constexpr char kTestDevPath[] = "/fake/dev";

// Returns the number of usable slices for a standard layout on a given-sized device.
size_t UsableSlicesCount(size_t disk_size, size_t slice_size) {
  return fvm::Header::FromDiskSize(fvm::kMaxUsablePartitions, disk_size, slice_size)
      .GetAllocationTableUsedEntryCount();
}

struct PartitionChannel {
  fidl::UnownedClientEnd<fuchsia_hardware_block::Block> as_block() {
    return fidl::UnownedClientEnd<fuchsia_hardware_block::Block>(partition.channel().borrow());
  }

  fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume> as_volume() {
    return fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume>(
        partition.channel().borrow());
  }

  static zx::result<PartitionChannel> Create(
      fidl::ClientEnd<fuchsia_device::Controller> controller) {
    PartitionChannel partition;
    partition.controller = std::move(controller);
    zx::result partition_server = fidl::CreateEndpoints(&partition.partition);
    if (partition_server.is_error()) {
      return partition_server.take_error();
    }
    if (fidl::OneWayStatus status = fidl::WireCall(partition.controller)
                                        ->ConnectToDeviceFidl(partition_server->TakeChannel());
        !status.ok()) {
      return zx::error(status.status());
    }
    return zx::ok(std::move(partition));
  }

  fidl::ClientEnd<fuchsia_device::Controller> controller;
  fidl::ClientEnd<fuchsia_hardware_block_partition::Partition> partition;
};

using driver_integration_test::IsolatedDevmgr;

class FvmTest : public zxtest::Test {
 protected:
  void SetUp() override {
    IsolatedDevmgr::Args args;
    args.disable_block_watcher = true;

    ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr_));
    ASSERT_OK(device_watcher::RecursiveWaitForFile(devfs_root_fd().get(),
                                                   "sys/platform/ram-disk/ramctl"));

    fdio_ns_t* name_space;
    ASSERT_OK(fdio_ns_get_installed(&name_space));

    ASSERT_OK(fdio_ns_bind_fd(name_space, kTestDevPath, devmgr_.devfs_root().get()));
  }

  const fbl::unique_fd& devfs_root_fd() const { return devmgr_.devfs_root(); }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> devfs_root() const {
    fdio_cpp::UnownedFdioCaller caller(devfs_root_fd());
    return component::Clone(caller.directory());
  }

  void TearDown() override {
    fdio_ns_t* name_space;
    ASSERT_OK(fdio_ns_get_installed(&name_space));
    ASSERT_OK(fdio_ns_unbind(name_space, kTestDevPath));
    ASSERT_OK(ramdisk_destroy(ramdisk_));
  }

  fbl::String fvm_path() const { return fxl::StringPrintf("%s/fvm", ramdisk_get_path(ramdisk_)); }

  zx::result<fbl::unique_fd> fvm_device_fd() const {
    fbl::unique_fd fd;
    zx_status_t status =
        fdio_open_fd_at(devfs_root_fd().get(), fvm_path().c_str(), 0, fd.reset_and_get_address());
    return zx::make_result(status, std::move(fd));
  }

  zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::VolumeManager>> fvm_device() const {
    zx::result devfs = devfs_root();
    if (devfs.is_error()) {
      return devfs.take_error();
    }
    return component::ConnectAt<fuchsia_hardware_block_volume::VolumeManager>(devfs.value(),
                                                                              fvm_path().c_str());
  }

  fidl::UnownedClientEnd<fuchsia_device::Controller> ramdisk_controller_interface() const {
    return fidl::UnownedClientEnd<fuchsia_device::Controller>(
        ramdisk_get_block_controller_interface(ramdisk_));
  }

  fidl::UnownedClientEnd<fuchsia_hardware_block::Block> ramdisk_block_interface() const {
    return fidl::UnownedClientEnd<fuchsia_hardware_block::Block>(
        ramdisk_get_block_interface(ramdisk_));
  }

  const ramdisk_client* ramdisk() const { return ramdisk_; }

  void FVMRebind();

  void CreateFVM(uint64_t block_size, uint64_t block_count, uint64_t slice_size);

  void CreateRamdisk(uint64_t block_size, uint64_t block_count);

  zx::result<PartitionChannel> OpenPartition(const fs_management::PartitionMatcher& matcher) const {
    zx::result controller =
        fs_management::OpenPartitionWithDevfs(devfs_root().value(), matcher, false);
    if (controller.is_error()) {
      return controller.take_error();
    }
    return PartitionChannel::Create(std::move(controller.value()));
  }

  zx::result<PartitionChannel> WaitForPartition(
      const fs_management::PartitionMatcher& matcher) const {
    zx::result controller =
        fs_management::OpenPartitionWithDevfs(devfs_root().value(), matcher, true);
    if (controller.is_error()) {
      return controller.take_error();
    }
    return PartitionChannel::Create(std::move(controller.value()));
  }

  struct AllocatePartitionRequest {
    size_t slice_count = 1;
    const uuid::Uuid& type;
    const uuid::Uuid& guid;
    const std::string_view& name;
    uint32_t flags = 0;
  };

  zx::result<PartitionChannel> AllocatePartition(AllocatePartitionRequest request) const {
    zx::result fvm = fvm_device();
    if (fvm.is_error()) {
      return fvm.take_error();
    }
    zx::result devfs = devfs_root();
    if (devfs.is_error()) {
      return devfs.take_error();
    }

    zx::result controller = fs_management::FvmAllocatePartitionWithDevfs(
        *devfs, *fvm, request.slice_count, request.type, request.guid, request.name, request.flags);
    if (controller.is_error()) {
      return controller.take_error();
    }
    return PartitionChannel::Create(std::move(controller.value()));
  }

 private:
  IsolatedDevmgr devmgr_;
  ramdisk_client_t* ramdisk_ = nullptr;
};

void FvmTest::CreateRamdisk(uint64_t block_size, uint64_t block_count) {
  if (ramdisk_ != nullptr) {
    ramdisk_destroy(ramdisk_);
  }
  ASSERT_OK(ramdisk_create_at(devfs_root_fd().get(), block_size, block_count, &ramdisk_));
}

void FvmTest::CreateFVM(uint64_t block_size, uint64_t block_count, uint64_t slice_size) {
  CreateRamdisk(block_size, block_count);

  ASSERT_OK(fs_management::FvmInitPreallocated(ramdisk_block_interface(), block_count * block_size,
                                               block_count * block_size, slice_size));

  auto resp = fidl::WireCall(ramdisk_controller_interface())->Bind(kFvmDriverLib);
  ASSERT_OK(resp.status());
  ASSERT_TRUE(resp->is_ok());

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devfs_root_fd().get(), fvm_path().c_str()));
}

void FvmTest::FVMRebind() {
  auto resp = fidl::WireCall(ramdisk_controller_interface())->Rebind(kFvmDriverLib);
  ASSERT_OK(resp.status());
  ASSERT_TRUE(resp->is_ok());

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devfs_root_fd().get(), fvm_path().c_str()));
}

void FVMCheckSliceSize(fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager> fvm,
                       size_t expected_slice_size) {
  auto volume_info_or = fs_management::FvmQuery(fvm);
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK, "Failed to query fvm\n");
  ASSERT_EQ(expected_slice_size, volume_info_or->slice_size, "Unexpected slice size\n");
}

void FVMCheckAllocatedCount(
    fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager> fvm,
    size_t expected_allocated, size_t expected_total) {
  auto volume_info_or = fs_management::FvmQuery(fvm);
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  ASSERT_EQ(volume_info_or->slice_count, expected_total);
  ASSERT_EQ(volume_info_or->assigned_slice_count, expected_allocated);
}

enum class ValidationResult {
  Valid,
  Corrupted,
};

void ValidateFVM(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device,
                 ValidationResult expected_result = ValidationResult::Valid) {
  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  fvm::Checker checker(device, block_info.block_size, true);
  switch (expected_result) {
    case ValidationResult::Valid:
      ASSERT_TRUE(checker.Validate());
      break;
    case ValidationResult::Corrupted:
      ASSERT_FALSE(checker.Validate());
      break;
  }
}

zx::result<std::string> GetPartitionPath(fidl::ClientEnd<fuchsia_device::Controller>& controller) {
  auto path = fidl::WireCall(controller)->GetTopologicalPath();
  if (!path.ok()) {
    return zx::error(path.status());
  }
  if (path->is_error()) {
    return zx::error(path->error_value());
  }
  // The partition doesn't know that the devmgr it's in is bound at "/fake".
  std::string topological_path = std::string("/fake") + path->value()->path.data();
  return zx::ok(std::move(topological_path));
}

/////////////////////// Helper functions, definitions

constexpr uuid::Uuid kTestUniqueGuid1 = {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
                                         0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f};
constexpr uuid::Uuid kTestUniqueGuid2 = {0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
                                         0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f};

// Intentionally avoid aligning these GUIDs with
// the actual system GUIDs; otherwise, limited versions
// of Fuchsia may attempt to actually mount these
// partitions automatically.

constexpr std::string_view kTestPartDataName = "data";
constexpr uuid::Uuid kTestPartDataGuid = {
    0xAA, 0xFF, 0xBB, 0x00, 0x33, 0x44, 0x88, 0x99, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
};

constexpr std::string_view kTestPartBlobName = "blob";
constexpr uuid::Uuid kTestPartBlobGuid = {
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0xAA, 0xFF, 0xBB, 0x00, 0x33, 0x44, 0x88, 0x99,
};

constexpr std::string_view kTestPartSystemName = "system";
constexpr uuid::Uuid kTestPartSystemGuid = {
    0xEE, 0xFF, 0xBB, 0x00, 0x33, 0x44, 0x88, 0x99, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
};

const fs_management::PartitionMatcher kPartition1Matcher = {
    .type_guids = {kTestPartDataGuid},
    .instance_guids = {kTestUniqueGuid1},
};
const fs_management::PartitionMatcher kPartition2Matcher = {
    .type_guids = {kTestPartDataGuid},
    .instance_guids = {kTestUniqueGuid2},
};

class VmoBuf;

class VmoClient : public fbl::RefCounted<VmoClient> {
 public:
  explicit VmoClient(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device);
  ~VmoClient() = default;

  void CheckWrite(VmoBuf& vbuf, size_t buf_off, size_t dev_off, size_t len);
  void CheckRead(VmoBuf& vbuf, size_t buf_off, size_t dev_off, size_t len);
  void Transaction(block_fifo_request_t* requests, size_t count) {
    ASSERT_OK(client_->Transaction(requests, count));
  }
  zx::result<storage::OwnedVmoid> RegisterVmo(const zx::vmo& vmo) {
    return client_->RegisterVmo(vmo);
  }

  fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device() const { return device_; }

  static groupid_t group() { return 0; }

 private:
  const fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device_;
  uint32_t block_size_;
  std::unique_ptr<block_client::Client> client_;
};

class VmoBuf {
 public:
  VmoBuf(fbl::RefPtr<VmoClient> client, size_t size) : client_(std::move(client)) {
    buf_ = std::make_unique<uint8_t[]>(size);

    ASSERT_EQ(zx::vmo::create(size, 0, &vmo_), ZX_OK);
    zx::result vmoid = client_->RegisterVmo(vmo_);
    ASSERT_OK(vmoid);
    vmoid_ = std::move(vmoid.value());
  }

  ~VmoBuf() {
    if (vmo_.is_valid()) {
      block_fifo_request_t request = {
          .command = {.opcode = BLOCK_OPCODE_CLOSE_VMO, .flags = 0},
          .group = client_->group(),
          .vmoid = vmoid_.TakeId(),
      };
      client_->Transaction(&request, 1);
    }
  }

 private:
  friend VmoClient;

  fbl::RefPtr<VmoClient> client_;
  zx::vmo vmo_;
  std::unique_ptr<uint8_t[]> buf_;
  storage::OwnedVmoid vmoid_;
};

VmoClient::VmoClient(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device)
    : device_(device) {
  {
    const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    block_size_ = response.value()->info.block_size;
  }

  auto [session, server] = fidl::Endpoints<fuchsia_hardware_block::Session>::Create();

  const fidl::Status result = fidl::WireCall(device)->OpenSession(std::move(server));
  ASSERT_OK(result.status());

  const fidl::WireResult fifo_result = fidl::WireCall(session)->GetFifo();
  ASSERT_OK(fifo_result.status());
  const fit::result fifo_response = fifo_result.value();
  ASSERT_TRUE(fifo_response.is_ok(), "%s", zx_status_get_string(fifo_response.error_value()));

  client_ = std::make_unique<block_client::Client>(std::move(session),
                                                   std::move(fifo_response.value()->fifo));
}

void VmoClient::CheckWrite(VmoBuf& vbuf, size_t buf_off, size_t dev_off, size_t len) {
  // Write to the client-side buffer
  for (size_t i = 0; i < len; i++)
    vbuf.buf_[i + buf_off] = static_cast<uint8_t>(rand());

  // Write to the registered VMO
  ASSERT_EQ(vbuf.vmo_.write(&vbuf.buf_[buf_off], buf_off, len), ZX_OK);
  ASSERT_EQ(len % block_size_, 0);
  ASSERT_EQ(buf_off % block_size_, 0);
  ASSERT_EQ(dev_off % block_size_, 0);

  // Write to the block device
  block_fifo_request_t request = {
      .command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0},
      .group = group(),
      .vmoid = vbuf.vmoid_.get(),
      .length = static_cast<uint32_t>(len / block_size_),
      .vmo_offset = buf_off / block_size_,
      .dev_offset = dev_off / block_size_,
  };
  Transaction(&request, 1);
}

void VmoClient::CheckRead(VmoBuf& vbuf, size_t buf_off, size_t dev_off, size_t len) {
  // Create a comparison buffer
  fbl::AllocChecker ac;
  std::unique_ptr<uint8_t[]> out(new (&ac) uint8_t[len]);
  ASSERT_TRUE(ac.check());
  memset(out.get(), 0, len);

  ASSERT_EQ(len % block_size_, 0);
  ASSERT_EQ(buf_off % block_size_, 0);
  ASSERT_EQ(dev_off % block_size_, 0);

  // Read from the block device
  block_fifo_request_t request = {
      .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0},
      .group = group(),
      .vmoid = vbuf.vmoid_.get(),
      .length = static_cast<uint32_t>(len / block_size_),
      .vmo_offset = buf_off / block_size_,
      .dev_offset = dev_off / block_size_,
  };
  Transaction(&request, 1);

  // Read from the registered VMO
  ASSERT_EQ(vbuf.vmo_.read(out.get(), buf_off, len), ZX_OK);

  ASSERT_EQ(memcmp(&vbuf.buf_[buf_off], out.get(), len), 0);
}

void CheckWrite(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device, size_t off,
                size_t len, uint8_t* buf) {
  for (size_t i = 0; i < len; i++) {
    buf[i] = static_cast<uint8_t>(rand());
  }
  ASSERT_OK(block_client::SingleWriteBytes(device, buf, len, off));
}

void CheckRead(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device, size_t off, size_t len,
               const uint8_t* in) {
  fbl::AllocChecker ac;
  std::unique_ptr<uint8_t[]> out(new (&ac) uint8_t[len]);
  ASSERT_TRUE(ac.check());
  memset(out.get(), 0, len);
  ASSERT_OK(block_client::SingleReadBytes(device, out.get(), len, off));
  ASSERT_EQ(memcmp(in, out.get(), len), 0);
}

void CheckWriteReadBlock(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device, size_t block,
                         size_t count) {
  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  size_t len = block_info.block_size * count;
  size_t off = block_info.block_size * block;
  std::unique_ptr<uint8_t[]> in(new uint8_t[len]);
  ASSERT_NO_FATAL_FAILURE(CheckWrite(device, off, len, in.get()));
  ASSERT_NO_FATAL_FAILURE(CheckRead(device, off, len, in.get()));
}

void CheckWriteReadBytesFifo(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device,
                             size_t off, size_t len) {
  std::unique_ptr<uint8_t[]> write_buf(new uint8_t[len]);
  memset(write_buf.get(), 0xa3, len);

  ASSERT_OK(block_client::SingleWriteBytes(device, write_buf.get(), len, off));
  std::unique_ptr<uint8_t[]> read_buf(new uint8_t[len]);
  memset(read_buf.get(), 0, len);
  ASSERT_OK(block_client::SingleReadBytes(device, read_buf.get(), len, off));
  EXPECT_EQ(memcmp(write_buf.get(), read_buf.get(), len), 0);
}

void CheckNoAccessBlock(fidl::UnownedClientEnd<fuchsia_hardware_block::Block> device, size_t block,
                        size_t count) {
  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  size_t len = block_info.block_size * count;
  size_t off = block_info.block_size * block;
  std::unique_ptr<uint8_t[]> buf(new uint8_t[len]);
  for (size_t i = 0; i < len; i++) {
    buf[i] = static_cast<uint8_t>(rand());
  }
  ASSERT_STATUS(block_client::SingleWriteBytes(device, buf.get(), len, off), ZX_ERR_OUT_OF_RANGE);
  ASSERT_STATUS(block_client::SingleReadBytes(device, buf.get(), len, off), ZX_ERR_OUT_OF_RANGE);
}

void CheckDeadConnection(zx::channel& chan) {
  ASSERT_OK(chan.wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), nullptr));
}

void Upgrade(fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager> fvm,
             const uuid::Uuid& old_guid, const uuid::Uuid& new_guid, zx_status_t status) {
  fuchsia_hardware_block_partition::wire::Guid old_guid_fidl;
  std::copy(old_guid.cbegin(), old_guid.cend(), old_guid_fidl.value.begin());
  fuchsia_hardware_block_partition::wire::Guid new_guid_fidl;
  std::copy(new_guid.cbegin(), new_guid.cend(), new_guid_fidl.value.begin());

  const fidl::WireResult result = fidl::WireCall(fvm)->Activate(old_guid_fidl, new_guid_fidl);
  ASSERT_OK(result.status());
  const fidl::WireResponse response = result.value();
  ASSERT_STATUS(response.status, status);
}

/////////////////////// Actual tests:

// Test initializing the FVM on a partition that is smaller than a slice
TEST_F(FvmTest, TestTooSmall) {
  uint64_t block_size = 512;
  uint64_t block_count = (1 << 15);

  CreateRamdisk(block_size, block_count);
  size_t slice_size = block_size * block_count;
  ASSERT_EQ(fs_management::FvmInit(ramdisk_block_interface(), slice_size), ZX_ERR_NO_SPACE);
  ValidateFVM(ramdisk_block_interface(), ValidationResult::Corrupted);
}

// Test initializing the FVM on a large partition, with metadata size > the max transfer size
TEST_F(FvmTest, TestLarge) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount{UINT64_C(8) * (1 << 20)};
  CreateRamdisk(kBlockSize, kBlockCount);

  constexpr size_t kSliceSize{static_cast<size_t>(16) * (1 << 10)};
  fvm::Header fvm_header =
      fvm::Header::FromDiskSize(fvm::kMaxUsablePartitions, kBlockSize * kBlockCount, kSliceSize);

  const fidl::WireResult result = fidl::WireCall(ramdisk_block_interface())->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  ASSERT_LT(block_info.max_transfer_size, fvm_header.GetMetadataAllocatedBytes());

  ASSERT_EQ(fs_management::FvmInit(ramdisk_block_interface(), kSliceSize), ZX_OK);

  auto resp = fidl::WireCall(ramdisk_controller_interface())->Bind(kFvmDriverLib);
  ASSERT_OK(resp.status());
  ASSERT_TRUE(resp->is_ok());

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devfs_root_fd().get(), fvm_path().c_str()));
  ValidateFVM(ramdisk_block_interface());
}

// Load and unload an empty FVM
TEST_F(FvmTest, TestEmpty) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);
  FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  ValidateFVM(ramdisk_block_interface());
}

// Test allocating a single partition
TEST_F(FvmTest, TestAllocateOne) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  // Allocate one VPart
  zx::result vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp = std::move(vp_or.value());

  // Check that the name matches what we provided
  const fidl::WireResult result = fidl::WireCall(vp.partition)->GetName();
  ASSERT_OK(result.status());
  const fidl::WireResponse response = result.value();
  ASSERT_OK(response.status);
  ASSERT_STREQ(response.name.get(), kTestPartDataName.data());

  // Check that we can read from / write to it.
  CheckWriteReadBlock(vp.as_block(), 0, 1);

  // Try accessing the block again after closing / re-opening it.
  vp = {};
  vp_or = WaitForPartition(kPartition1Matcher);
  ASSERT_EQ(vp_or.status_value(), ZX_OK, "Couldn't re-open Data VPart");
  vp = std::move(vp_or.value());
  CheckWriteReadBlock(vp.as_block(), 0, 1);
  vp = {};

  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);
  FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  ValidateFVM(ramdisk_block_interface());
}

// Test Reading and writing with RemoteBlockDevice helpers
TEST_F(FvmTest, TestReadWriteSingle) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  // Allocate one VPart
  zx::result vp = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_OK(vp);

  // Check that we can read from / write to it.
  CheckWriteReadBytesFifo(vp->as_block(), 0, kBlockSize);
  // Check with an offset
  CheckWriteReadBytesFifo(vp->as_block(), kBlockSize * 7, kBlockSize * 4);
  vp = {};

  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);
  FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  ValidateFVM(ramdisk_block_interface());
}

// Test allocating a collection of partitions
TEST_F(FvmTest, TestAllocateMany) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  // Test allocation of multiple VPartitions
  zx::result data_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(data_or.status_value(), ZX_OK);
  PartitionChannel data = *std::move(data_or);

  zx::result blob_or = AllocatePartition({
      .type = kTestPartBlobGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartBlobName,
  });
  ASSERT_EQ(blob_or.status_value(), ZX_OK);
  PartitionChannel blob = *std::move(blob_or);

  zx::result sys_or = AllocatePartition({
      .type = kTestPartSystemGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartSystemName,
  });
  ASSERT_EQ(sys_or.status_value(), ZX_OK);
  PartitionChannel sys = *std::move(sys_or);

  CheckWriteReadBlock(data.as_block(), 0, 1);
  CheckWriteReadBlock(blob.as_block(), 0, 1);
  CheckWriteReadBlock(sys.as_block(), 0, 1);

  data = {};
  blob = {};
  sys = {};

  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);
  FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  ValidateFVM(ramdisk_block_interface());
}

// Test allocating additional slices to a vpartition.
TEST_F(FvmTest, TestVPartitionExtend) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  size_t slice_size = volume_info_or->slice_size;
  constexpr uint64_t kDiskSize = kBlockSize * kBlockCount;
  size_t slices_total = UsableSlicesCount(kDiskSize, slice_size);
  size_t slices_left = slices_total;

  FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);

  // Allocate one VPart
  size_t slice_count = 1;
  auto vp_or = AllocatePartition({
      .slice_count = slice_count,
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK, "Couldn't open Volume");
  PartitionChannel vp = *std::move(vp_or);
  slices_left--;
  FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);

  // Confirm that the disk reports the correct number of slices
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
    ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);
  }

  // Try re-allocating an already allocated vslice
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(0, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);
  }

  {
    const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
    ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);
  }

  // Try again with a portion of the request which is unallocated
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(0, 2);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);
  }

  // Allocate OBSCENELY too many slices
  {
    const fidl::WireResult result =
        fidl::WireCall(vp.as_volume())->Extend(slice_count, std::numeric_limits<uint64_t>::max());
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);
  }

  // Allocate slices at a too-large offset
  {
    const fidl::WireResult result =
        fidl::WireCall(vp.as_volume())->Extend(std::numeric_limits<uint64_t>::max(), 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);
  }

  // Attempt to allocate slightly too many slices
  {
    const fidl::WireResult result =
        fidl::WireCall(vp.as_volume())->Extend(slice_count, slices_left + 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_NO_SPACE);
  }

  // The number of free slices should be unchanged.
  FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);

  // Allocate exactly the remaining number of slices
  {
    const fidl::WireResult result =
        fidl::WireCall(vp.as_volume())->Extend(slice_count, slices_left);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }

  slice_count += slices_left;
  slices_left = 0;
  FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);

  {
    const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
    ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);
  }

  // We can't allocate any more to this VPartition
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(slice_count, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_NO_SPACE);
  }

  // We can't allocate a new VPartition
  zx::result vp2_or = AllocatePartition({
      .type = kTestPartBlobGuid,
      .guid = kTestUniqueGuid2,
      .name = kTestPartBlobName,
  });
  ASSERT_NE(vp2_or.status_value(), ZX_OK, "Expected VPart allocation failure");

  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
    ValidateFVM(ramdisk_block_interface());
  }
}

// Test allocating very sparse VPartition
TEST_F(FvmTest, TestVPartitionExtendSparse) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  zx::result vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp = *std::move(vp_or);
  CheckWriteReadBlock(vp.as_block(), 0, 1);

  // Double check that we can access a block at this vslice address
  // (this isn't always possible; for certain slice sizes, blocks may be
  // allocatable / freeable, but not addressable).
  size_t bno = (fvm::kMaxVSlices - 1) * (kSliceSize / kBlockSize);
  ASSERT_EQ(bno / (kSliceSize / kBlockSize), (fvm::kMaxVSlices - 1), "bno overflowed");
  ASSERT_EQ((bno * kBlockSize) / kBlockSize, bno, "block access will overflow");

  // Try allocating at a location that's slightly too large
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(fvm::kMaxVSlices, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);
  }

  // Try allocating at the largest offset
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(fvm::kMaxVSlices - 1, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }

  CheckWriteReadBlock(vp.as_block(), bno, 1);

  // Try freeing beyond largest offset
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(fvm::kMaxVSlices, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);
  }

  CheckWriteReadBlock(vp.as_block(), bno, 1);

  // Try freeing at the largest offset
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(fvm::kMaxVSlices - 1, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }

  CheckNoAccessBlock(vp.as_block(), bno, 1);

  vp = {};
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);
  FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  ValidateFVM(ramdisk_block_interface());
}

// Test removing slices from a VPartition.
TEST_F(FvmTest, TestVPartitionShrink) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  size_t slice_size = volume_info_or->slice_size;
  const size_t kDiskSize = kBlockSize * kBlockCount;
  size_t slices_total = UsableSlicesCount(kDiskSize, slice_size);
  size_t slices_left = slices_total;

  FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);

  // Allocate one VPart
  size_t slice_count = 1;
  zx::result vp_or = AllocatePartition({
      .slice_count = slice_count,
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK, "Couldn't open Volume");
  PartitionChannel vp = *std::move(vp_or);
  slices_left--;

  // Confirm that the disk reports the correct number of slices
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
    ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);
    CheckWriteReadBlock(vp.as_block(), (slice_size / block_info.block_size) - 1, 1);
    CheckNoAccessBlock(vp.as_block(), (slice_size / block_info.block_size) - 1, 2);
    FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);
  }

  // Try shrinking the 0th vslice
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(0, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);
  }

  // Try no-op requests (length = 0).
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(1, 0);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(1, 0);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }

  {
    const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
    ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);
  }

  // Try again with a portion of the request which is unallocated
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(1, 2);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_INVALID_ARGS);
  }
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
    ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);
    FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);
  }

  // Allocate exactly the remaining number of slices
  {
    const fidl::WireResult result =
        fidl::WireCall(vp.as_volume())->Extend(slice_count, slices_left);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  slice_count += slices_left;
  slices_left = 0;

  {
    const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
    ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);
    CheckWriteReadBlock(vp.as_block(), (slice_size / block_info.block_size) - 1, 1);
    CheckWriteReadBlock(vp.as_block(), (slice_size / block_info.block_size) - 1, 2);
  }
  FVMCheckAllocatedCount(volume_manager.value(), slices_total - slices_left, slices_total);

  // We can't allocate any more to this VPartition
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(slice_count, 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_NO_SPACE);
  }

  // Try to shrink off the end (okay, since SOME of the slices are allocated)
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(1, slice_count + 3);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  FVMCheckAllocatedCount(volume_manager.value(), 1, slices_total);

  // The same request to shrink should now fail (NONE of the slices are
  // allocated)
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(1, slice_count - 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_STATUS(response.status, ZX_ERR_INVALID_ARGS);
  }
  FVMCheckAllocatedCount(volume_manager.value(), 1, slices_total);

  // ... unless we re-allocate and try again.
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(1, slice_count - 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(1, slice_count - 1);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }

  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
    ValidateFVM(ramdisk_block_interface());
  }
}

// Test splitting a contiguous slice extent into multiple parts
TEST_F(FvmTest, TestVPartitionSplit) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  size_t slice_size = volume_info_or->slice_size;

  // Allocate one VPart
  size_t slice_count = 5;
  zx::result vp_or = AllocatePartition({
      .slice_count = slice_count,
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp = *std::move(vp_or);

  // Confirm that the disk reports the correct number of slices
  const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * slice_count);

  size_t reset_offset = 1;
  size_t reset_length = slice_count - 1;
  size_t mid_offset = 2;
  size_t mid_length = 1;
  size_t start_offset = 1;
  size_t start_length = 1;
  size_t end_offset = 3;
  size_t end_length = slice_count - 3;

  auto verifyExtents = [&](bool start, bool mid, bool end) {
    size_t start_block = start_offset * (slice_size / block_info.block_size);
    size_t mid_block = mid_offset * (slice_size / block_info.block_size);
    size_t end_block = end_offset * (slice_size / block_info.block_size);

    if (start) {
      CheckWriteReadBlock(vp.as_block(), start_block, 1);
    } else {
      CheckNoAccessBlock(vp.as_block(), start_block, 1);
    }
    if (mid) {
      CheckWriteReadBlock(vp.as_block(), mid_block, 1);
    } else {
      CheckNoAccessBlock(vp.as_block(), mid_block, 1);
    }
    if (end) {
      CheckWriteReadBlock(vp.as_block(), end_block, 1);
    } else {
      CheckNoAccessBlock(vp.as_block(), end_block, 1);
    }
    return true;
  };

  auto doExtend = [&vp](size_t offset, size_t length) {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  };

  auto doShrink = [&vp](size_t offset, size_t length) {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Shrink(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  };

  // We should be able to split the extent.
  verifyExtents(true, true, true);
  doShrink(mid_offset, mid_length);
  verifyExtents(true, false, true);
  doShrink(start_offset, start_length);
  verifyExtents(false, false, true);
  doShrink(end_offset, end_length);
  verifyExtents(false, false, false);

  doExtend(reset_offset, reset_length);

  doShrink(start_offset, start_length);
  verifyExtents(false, true, true);
  doShrink(mid_offset, mid_length);
  verifyExtents(false, false, true);
  doShrink(end_offset, end_length);
  verifyExtents(false, false, false);

  doExtend(reset_offset, reset_length);

  doShrink(end_offset, end_length);
  verifyExtents(true, true, false);
  doShrink(mid_offset, mid_length);
  verifyExtents(true, false, false);
  doShrink(start_offset, start_length);
  verifyExtents(false, false, false);

  doExtend(reset_offset, reset_length);

  doShrink(end_offset, end_length);
  verifyExtents(true, true, false);
  doShrink(start_offset, start_length);
  verifyExtents(false, true, false);
  doShrink(mid_offset, mid_length);
  verifyExtents(false, false, false);

  // We should also be able to combine extents
  doExtend(mid_offset, mid_length);
  verifyExtents(false, true, false);
  doExtend(start_offset, start_length);
  verifyExtents(true, true, false);
  doExtend(end_offset, end_length);
  verifyExtents(true, true, true);

  doShrink(reset_offset, reset_length);

  doExtend(end_offset, end_length);
  verifyExtents(false, false, true);
  doExtend(mid_offset, mid_length);
  verifyExtents(false, true, true);
  doExtend(start_offset, start_length);
  verifyExtents(true, true, true);

  doShrink(reset_offset, reset_length);

  doExtend(end_offset, end_length);
  verifyExtents(false, false, true);
  doExtend(start_offset, start_length);
  verifyExtents(true, false, true);
  doExtend(mid_offset, mid_length);
  verifyExtents(true, true, true);

  vp = {};
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
    ValidateFVM(ramdisk_block_interface());
  }
}

// Test removing VPartitions within an FVM
TEST_F(FvmTest, TestVPartitionDestroy) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  // Test allocation of multiple VPartitions
  zx::result data_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(data_or.status_value(), ZX_OK);
  PartitionChannel data = *std::move(data_or);

  zx::result blob_or = AllocatePartition({
      .type = kTestPartBlobGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartBlobName,
  });
  ASSERT_EQ(blob_or.status_value(), ZX_OK);
  PartitionChannel blob = *std::move(blob_or);

  zx::result sys_or = AllocatePartition({
      .type = kTestPartSystemGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartSystemName,
  });
  ASSERT_EQ(sys_or.status_value(), ZX_OK);
  PartitionChannel sys = *std::move(sys_or);

  // We can access all three...
  CheckWriteReadBlock(data.as_block(), 0, 1);
  CheckWriteReadBlock(blob.as_block(), 0, 1);
  CheckWriteReadBlock(sys.as_block(), 0, 1);

  // But not after we destroy the blob partition.
  {
    const fidl::WireResult result = fidl::WireCall(blob.as_volume())->Destroy();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  CheckWriteReadBlock(data.as_block(), 0, 1);
  CheckWriteReadBlock(sys.as_block(), 0, 1);
  CheckDeadConnection(blob.partition.channel());

  // Destroy the other two VPartitions.
  {
    const fidl::WireResult result = fidl::WireCall(data.as_volume())->Destroy();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  CheckWriteReadBlock(sys.as_block(), 0, 1);
  CheckDeadConnection(data.partition.channel());

  {
    const fidl::WireResult result = fidl::WireCall(sys.as_volume())->Destroy();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  CheckDeadConnection(sys.partition.channel());

  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  }
}

TEST_F(FvmTest, TestVPartitionQuery) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  // Allocate partition
  zx::result part_or = AllocatePartition({
      .slice_count = 10,
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(part_or.status_value(), ZX_OK);
  PartitionChannel part = *std::move(part_or);

  // Create non-contiguous extent.
  uint64_t offset = 20;
  uint64_t length = 10;
  {
    const fidl::WireResult result = fidl::WireCall(part.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);

  // Query various vslice ranges
  uint64_t start_slices[6];
  start_slices[0] = 0;
  start_slices[1] = 10;
  start_slices[2] = 20;
  start_slices[3] = 50;
  start_slices[4] = 25;
  start_slices[5] = 15;

  // Check response from partition query
  {
    const fidl::WireResult result =
        fidl::WireCall(part.as_volume())
            ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(start_slices));
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    fidl::Array ranges = response.response;

    ASSERT_EQ(response.response_count, std::size(start_slices));
    ASSERT_TRUE(ranges[0].allocated);
    ASSERT_EQ(ranges[0].count, 10);
    ASSERT_FALSE(ranges[1].allocated);
    ASSERT_EQ(ranges[1].count, 10);
    ASSERT_TRUE(ranges[2].allocated);
    ASSERT_EQ(ranges[2].count, 10);
    ASSERT_FALSE(ranges[3].allocated);
    ASSERT_EQ(ranges[3].count, volume_info_or->max_virtual_slice - 50);
    ASSERT_TRUE(ranges[4].allocated);
    ASSERT_EQ(ranges[4].count, 5);
    ASSERT_FALSE(ranges[5].allocated);
    ASSERT_EQ(ranges[5].count, 5);
  }

  // Merge the extents!
  offset = 10;
  length = 10;
  {
    const fidl::WireResult result = fidl::WireCall(part.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }

  // Check partition query response again after extend
  {
    const fidl::WireResult result =
        fidl::WireCall(part.as_volume())
            ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(start_slices));
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    fidl::Array ranges = response.response;

    ASSERT_EQ(response.response_count, std::size(start_slices));
    ASSERT_TRUE(ranges[0].allocated);
    ASSERT_EQ(ranges[0].count, 30);
    ASSERT_TRUE(ranges[1].allocated);
    ASSERT_EQ(ranges[1].count, 20);
    ASSERT_TRUE(ranges[2].allocated);
    ASSERT_EQ(ranges[2].count, 10);
    ASSERT_FALSE(ranges[3].allocated);
    ASSERT_EQ(ranges[3].count, volume_info_or->max_virtual_slice - 50);
    ASSERT_TRUE(ranges[4].allocated);
    ASSERT_EQ(ranges[4].count, 5);
    ASSERT_TRUE(ranges[5].allocated);
    ASSERT_EQ(ranges[5].count, 15);
  }

  start_slices[0] = volume_info_or->max_virtual_slice + 1;
  const fidl::WireResult result =
      fidl::WireCall(part.as_volume())
          ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(start_slices));
  ASSERT_OK(result.status());
  const fidl::WireResponse response = result.value();
  ASSERT_STATUS(response.status, ZX_ERR_OUT_OF_RANGE);

  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  }
}

// Test allocating and accessing slices which are allocated contiguously.
TEST_F(FvmTest, TestSliceAccessContiguous) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  size_t slice_size = volume_info_or->slice_size;

  // Allocate one VPart
  zx::result vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp = *std::move(vp_or);
  fidl::UnownedClientEnd device = vp.as_block();

  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;

  // This is the last 'accessible' block.
  size_t last_block = (slice_size / block_info.block_size) - 1;

  {
    auto vc = fbl::MakeRefCounted<VmoClient>(device);
    VmoBuf vb(vc, block_info.block_size * static_cast<size_t>(2));
    vc->CheckWrite(vb, 0, block_info.block_size * last_block, block_info.block_size);
    vc->CheckRead(vb, 0, block_info.block_size * last_block, block_info.block_size);

    // Try writing out of bounds -- check that we don't have access.
    CheckNoAccessBlock(device, (slice_size / block_info.block_size) - 1, 2);
    CheckNoAccessBlock(device, slice_size / block_info.block_size, 1);

    // Attempt to access the next contiguous slice
    {
      const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(1, 1);
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
    }

    // Now we can access the next slice...
    vc->CheckWrite(vb, block_info.block_size, block_info.block_size * (last_block + 1),
                   block_info.block_size);
    vc->CheckRead(vb, block_info.block_size, block_info.block_size * (last_block + 1),
                  block_info.block_size);
    // ... We can still access the previous slice...
    vc->CheckRead(vb, 0, block_info.block_size * last_block, block_info.block_size);
    // ... And we can cross slices
    vc->CheckRead(vb, 0, block_info.block_size * last_block,
                  block_info.block_size * static_cast<size_t>(2));
  }

  vp = {};
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  }
}

// Test allocating and accessing multiple (3+) slices at once.
TEST_F(FvmTest, TestSliceAccessMany) {
  // The size of a slice must be carefully constructed for this test
  // so that we can hold multiple slices in memory without worrying
  // about hitting resource limits.
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 20;
  constexpr uint64_t kBlocksPerSlice = 256;
  constexpr uint64_t kSliceSize = kBlocksPerSlice * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  ASSERT_EQ(volume_info_or->slice_size, kSliceSize);

  // Allocate one VPart
  auto vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp = *std::move(vp_or);

  fidl::UnownedClientEnd device = vp.as_block();

  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  ASSERT_EQ(block_info.block_size, kBlockSize);

  {
    auto vc = fbl::MakeRefCounted<VmoClient>(device);
    VmoBuf vb(vc, kSliceSize * 3);

    // Access the first slice
    vc->CheckWrite(vb, 0, 0, kSliceSize);
    vc->CheckRead(vb, 0, 0, kSliceSize);

    // Try writing out of bounds -- check that we don't have access.
    CheckNoAccessBlock(device, kBlocksPerSlice - 1, 2);
    CheckNoAccessBlock(device, kBlocksPerSlice, 1);

    // Attempt to access the next contiguous slices
    uint64_t offset = 1;
    uint64_t length = 2;
    {
      const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(offset, length);
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
    }

    // Now we can access the next slices...
    vc->CheckWrite(vb, kSliceSize, kSliceSize, 2 * kSliceSize);
    vc->CheckRead(vb, kSliceSize, kSliceSize, 2 * kSliceSize);
    // ... We can still access the previous slice...
    vc->CheckRead(vb, 0, 0, kSliceSize);
    // ... And we can cross slices for reading.
    vc->CheckRead(vb, 0, 0, 3 * kSliceSize);

    // Also, we can cross slices for writing.
    vc->CheckWrite(vb, 0, 0, 3 * kSliceSize);
    vc->CheckRead(vb, 0, 0, 3 * kSliceSize);

    // Additionally, we can access "parts" of slices in a multi-slice
    // operation. Here, read one block into the first slice, and read
    // up to the last block in the final slice.
    vc->CheckWrite(vb, 0, kBlockSize, 3 * kSliceSize - 2 * kBlockSize);
    vc->CheckRead(vb, 0, kBlockSize, 3 * kSliceSize - 2 * kBlockSize);
  }

  vp = {};
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
    ValidateFVM(ramdisk_block_interface());
  }
}

// Test allocating and accessing slices which are allocated
// virtually contiguously (they appear sequential to the client) but are
// actually noncontiguous on the FVM partition.
TEST_F(FvmTest, TestSliceAccessNonContiguousPhysical) {
  constexpr uint64_t kBlockSize{512};
  constexpr uint64_t kBlockCount{1 << 16};
  constexpr uint64_t kSliceSize{kBlockSize * 64};
  constexpr uint64_t kDiskSize{kBlockSize * kBlockCount};
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  ASSERT_EQ(fs_management::FvmQuery(volume_manager.value()).status_value(), ZX_OK);

  constexpr size_t kNumVParts = 3;
  constexpr size_t kSliceCount = 1;
  typedef struct vdata {
    PartitionChannel partition;
    const uuid::Uuid& guid;
    const std::string_view& name;
    size_t slices_used;
  } vdata_t;

  vdata_t vparts[kNumVParts] = {
      {{}, kTestPartDataGuid, kTestPartDataName, kSliceCount},
      {{}, kTestPartBlobGuid, kTestPartBlobName, kSliceCount},
      {{}, kTestPartSystemGuid, kTestPartSystemName, kSliceCount},
  };

  for (auto& vpart : vparts) {
    zx::result part_or = AllocatePartition({
        .slice_count = kSliceCount,
        .type = vpart.guid,
        .guid = kTestUniqueGuid1,
        .name = vpart.name,
    });
    ASSERT_EQ(part_or.status_value(), ZX_OK);
    vpart.partition = *std::move(part_or);
  }

  const fidl::WireResult result = fidl::WireCall(vparts[0].partition.as_block())->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;

  size_t usable_slices_per_vpart = UsableSlicesCount(kDiskSize, kSliceSize) / kNumVParts;
  size_t i = 0;
  while (vparts[i].slices_used < usable_slices_per_vpart) {
    // This is the last 'accessible' block.
    size_t last_block = (vparts[i].slices_used * (kSliceSize / block_info.block_size)) - 1;

    auto vc = fbl::MakeRefCounted<VmoClient>(vparts[i].partition.as_block());
    VmoBuf vb(vc, block_info.block_size * static_cast<size_t>(2));

    vc->CheckWrite(vb, 0, block_info.block_size * last_block, block_info.block_size);
    vc->CheckRead(vb, 0, block_info.block_size * last_block, block_info.block_size);

    // Try writing out of bounds -- check that we don't have access.
    CheckNoAccessBlock(vparts[i].partition.as_block(), last_block, 2);
    CheckNoAccessBlock(vparts[i].partition.as_block(), last_block + 1, 1);

    // Attempt to access the next contiguous slice
    uint64_t offset = vparts[i].slices_used;
    uint64_t length = 1;
    {
      const fidl::WireResult result =
          fidl::WireCall(vparts[i].partition.as_volume())->Extend(offset, length);
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
    }

    // Now we can access the next slice...
    vc->CheckWrite(vb, block_info.block_size, block_info.block_size * (last_block + 1),
                   block_info.block_size);
    vc->CheckRead(vb, block_info.block_size, block_info.block_size * (last_block + 1),
                  block_info.block_size);
    // ... We can still access the previous slice...
    vc->CheckRead(vb, 0, block_info.block_size * last_block, block_info.block_size);
    // ... And we can cross slices
    vc->CheckRead(vb, 0, block_info.block_size * last_block,
                  block_info.block_size * static_cast<size_t>(2));

    vparts[i].slices_used++;
    i = (i + 1) % kNumVParts;
  }

  for (size_t i = 0; i < kNumVParts; i++) {
    printf("Testing multi-slice operations on vslice %lu\n", i);

    // We need at least five slices, so we can access three "middle"
    // slices and jitter to test off-by-one errors.
    ASSERT_GE(vparts[i].slices_used, 5);

    {
      auto vc = fbl::MakeRefCounted<VmoClient>(vparts[i].partition.as_block());
      VmoBuf vb(vc, kSliceSize * 4);

      // Try accessing 3 noncontiguous slices at once, with the
      // addition of "off by one block".
      size_t dev_off_start = kSliceSize - block_info.block_size;
      size_t dev_off_end = kSliceSize + block_info.block_size;
      size_t len_start = kSliceSize * 3 - block_info.block_size;
      size_t len_end = kSliceSize * 3 + block_info.block_size;

      // Test a variety of:
      // Starting device offsets,
      size_t bsz = block_info.block_size;
      for (size_t dev_off = dev_off_start; dev_off <= dev_off_end; dev_off += bsz) {
        printf("  Testing non-contiguous write/read starting at offset: %zu\n", dev_off);
        // Operation lengths,
        for (size_t len = len_start; len <= len_end; len += bsz) {
          printf("    Testing operation of length: %zu\n", len);
          // and starting VMO offsets
          for (size_t vmo_off = 0; vmo_off < 3 * bsz; vmo_off += bsz) {
            // Try writing & reading the entire section (multiple
            // slices) at once.
            vc->CheckWrite(vb, vmo_off, dev_off, len);
            vc->CheckRead(vb, vmo_off, dev_off, len);

            // Try reading the section one slice at a time.
            // The results should be the same.
            size_t sub_off = 0;
            size_t sub_len = kSliceSize - (dev_off % kSliceSize);
            while (sub_off < len) {
              vc->CheckRead(vb, vmo_off + sub_off, dev_off + sub_off, sub_len);
              sub_off += sub_len;
              sub_len = std::min(kSliceSize, len - sub_off);
            }
          }
        }
      }
    }
    vparts[i].partition = {};
  }

  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
    ValidateFVM(ramdisk_block_interface());
  }
}

// Test allocating and accessing slices which are
// allocated noncontiguously from the client's perspective.
TEST_F(FvmTest, TestSliceAccessNonContiguousVirtual) {
  constexpr uint64_t kBlockSize{512};
  constexpr uint64_t kBlockCount{1 << 20};
  constexpr uint64_t kSliceSize{UINT64_C(64) * (1 << 20)};
  constexpr uint64_t kDiskSize{kBlockSize * kBlockCount};
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  ASSERT_EQ(fs_management::FvmQuery(volume_manager.value()).status_value(), ZX_OK);

  constexpr size_t kNumVParts = 3;
  constexpr size_t kSliceCount = 1;
  typedef struct vdata {
    PartitionChannel partition;
    const uuid::Uuid& guid;
    const std::string_view& name;
    size_t slices_used;
    size_t last_slice;
  } vdata_t;

  vdata_t vparts[kNumVParts] = {
      {{}, kTestPartDataGuid, kTestPartDataName, kSliceCount, kSliceCount},
      {{}, kTestPartBlobGuid, kTestPartBlobName, kSliceCount, kSliceCount},
      {{}, kTestPartSystemGuid, kTestPartSystemName, kSliceCount, kSliceCount},
  };

  for (auto& vpart : vparts) {
    zx::result part_or = AllocatePartition({
        .slice_count = kSliceCount,
        .type = vpart.guid,
        .guid = kTestUniqueGuid1,
        .name = vpart.name,
    });
    ASSERT_EQ(part_or.status_value(), ZX_OK);
    vpart.partition = *std::move(part_or);
  }

  const fidl::WireResult result = fidl::WireCall(vparts[0].partition.as_block())->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;

  size_t usable_slices_per_vpart = UsableSlicesCount(kDiskSize, kSliceSize) / kNumVParts;
  size_t i = 0;
  while (vparts[i].slices_used < usable_slices_per_vpart) {
    fidl::UnownedClientEnd device = vparts[i].partition.as_block();
    // This is the last 'accessible' block.
    size_t last_block = (vparts[i].last_slice * (kSliceSize / block_info.block_size)) - 1;
    CheckWriteReadBlock(device, last_block, 1);

    // Try writing out of bounds -- check that we don't have access.
    CheckNoAccessBlock(device, last_block, 2);
    CheckNoAccessBlock(device, last_block + 1, 1);

    // Attempt to access a non-contiguous slice
    uint64_t offset = vparts[i].last_slice + 2;
    uint64_t length = 1;
    {
      const fidl::WireResult result =
          fidl::WireCall(vparts[i].partition.as_volume())->Extend(offset, length);
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
    }

    // We still don't have access to the next slice...
    CheckNoAccessBlock(device, last_block, 2);
    CheckNoAccessBlock(device, last_block + 1, 1);

    // But we have access to the slice we asked for!
    size_t requested_block = (offset * kSliceSize) / block_info.block_size;
    CheckWriteReadBlock(device, requested_block, 1);

    vparts[i].slices_used++;
    vparts[i].last_slice = offset;
    i = (i + 1) % kNumVParts;
  }

  for (vdata_t& vpart : vparts) {
    vpart.partition = {};
  }

  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
    ValidateFVM(ramdisk_block_interface());
  }
}

// Test that the FVM driver actually persists updates.
TEST_F(FvmTest, TestPersistenceSimple) {
  constexpr uint64_t kBlockSize{512};
  constexpr uint64_t kBlockCount{1 << 20};
  constexpr uint64_t kSliceSize{UINT64_C(64) * (1 << 20)};
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  constexpr uint64_t kDiskSize = kBlockSize * kBlockCount;
  size_t slices_left = UsableSlicesCount(kDiskSize, kSliceSize);
  const uint64_t kSliceCount = slices_left;

  ASSERT_EQ(fs_management::FvmQuery(volume_manager.value()).status_value(), ZX_OK);

  // Allocate one VPart
  auto vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp = *std::move(vp_or);
  slices_left--;

  fidl::UnownedClientEnd device = vp.as_block();

  // Check that the name matches what we provided
  {
    const fidl::WireResult result = fidl::WireCall(vp.partition)->GetName();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    ASSERT_STREQ(response.name.get(), kTestPartDataName.data());
  }

  fuchsia_hardware_block::wire::BlockInfo block_info;
  {
    const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    block_info = response.value()->info;
  }
  std::unique_ptr<uint8_t[]> buf(new uint8_t[block_info.block_size * static_cast<size_t>(2)]);

  // Check that we can read from / write to it
  CheckWrite(device, 0, block_info.block_size, buf.get());
  CheckRead(device, 0, block_info.block_size, buf.get());
  vp = {};

  // Check that it still exists after rebinding the driver
  FVMRebind();
  volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  vp_or = WaitForPartition(kPartition1Matcher);
  ASSERT_OK(vp_or.status_value());
  vp = *std::move(vp_or);
  device = vp.as_block();

  CheckRead(device, 0, block_info.block_size, buf.get());

  // Try extending the vpartition, and checking that the extension persists.
  // This is the last 'accessible' block.
  size_t last_block = (kSliceSize / block_info.block_size) - 1;
  CheckWrite(device, block_info.block_size * last_block, block_info.block_size, buf.get());
  CheckRead(device, block_info.block_size * last_block, block_info.block_size, buf.get());

  // Try writing out of bounds -- check that we don't have access.
  CheckNoAccessBlock(vp.as_block(), (kSliceSize / block_info.block_size) - 1, 2);
  CheckNoAccessBlock(vp.as_block(), kSliceSize / block_info.block_size, 1);

  uint64_t offset = 1;
  uint64_t length = 1;
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  slices_left--;

  vp = {};
  // FVMRebind will cause the rebind on ramdisk block device. The fvm device is child device
  // to ramdisk block device. Before issuing rebind make sure the fd is released.
  // Rebind the FVM driver, check the extension has succeeded.
  FVMRebind();
  volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  vp_or = WaitForPartition(kPartition1Matcher);
  ASSERT_OK(vp_or.status_value());
  vp = *std::move(vp_or);
  device = vp.as_block();

  // Now we can access the next slice...
  CheckWrite(device, block_info.block_size * (last_block + 1), block_info.block_size,
             &buf[block_info.block_size]);
  CheckRead(device, block_info.block_size * (last_block + 1), block_info.block_size,
            &buf[block_info.block_size]);
  // ... We can still access the previous slice...
  CheckRead(device, block_info.block_size * last_block, block_info.block_size, buf.get());
  // ... And we can cross slices
  CheckRead(device, block_info.block_size * last_block,
            block_info.block_size * static_cast<size_t>(2), buf.get());

  // Try allocating the rest of the slices, rebinding, and ensuring
  // that the size stays updated.
  {
    const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    block_info = response.value()->info;
  }
  ASSERT_EQ(block_info.block_count * block_info.block_size, kSliceSize * 2);

  offset = 2;
  length = slices_left;
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  {
    const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    block_info = response.value()->info;
  }
  ASSERT_EQ(block_info.block_count * block_info.block_size, kSliceSize * kSliceCount);

  vp = {};
  FVMRebind();
  volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  vp_or = WaitForPartition(kPartition1Matcher);
  ASSERT_OK(vp_or.status_value());
  vp = *std::move(vp_or);
  device = vp.as_block();

  {
    const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    block_info = response.value()->info;
  }
  ASSERT_EQ(block_info.block_count * block_info.block_size, kSliceSize * kSliceCount);

  vp = {};
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), 64lu * (1 << 20));
  }
}

void CorruptMountHelper(const fbl::unique_fd& devfs_root, const char* partition_path,
                        const fs_management::MountOptions& mounting_options,
                        fs_management::DiskFormat disk_format, const size_t* vslice_start,
                        size_t vslice_count) {
  fdio_cpp::UnownedFdioCaller devfs_caller(devfs_root.get());
  auto component = fs_management::FsComponent::FromDiskFormat(disk_format);
  // Format the VPart as |disk_format|.
  ASSERT_EQ(fs_management::Mkfs(partition_path, component, {}), ZX_OK);

  fuchsia_hardware_block_volume::wire::VsliceRange
      initial_ranges[fuchsia_hardware_block_volume::wire::kMaxSliceRequests];

  // Check initial slice allocation.
  {
    zx::result controller =
        fs_management::OpenPartitionWithDevfs(devfs_caller.directory(), kPartition1Matcher, true);
    ASSERT_OK(controller);
    zx::result vp = PartitionChannel::Create(std::move(controller.value()));
    ASSERT_OK(vp);

    const fidl::WireResult result = fidl::WireCall(vp->as_volume())
                                        ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(
                                            const_cast<size_t*>(vslice_start), vslice_count));
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    ASSERT_EQ(vslice_count, response.response_count);

    for (unsigned i = 0; i < response.response_count; i++) {
      ASSERT_TRUE(response.response[i].allocated);
      ASSERT_GT(response.response[i].count, 0);
      initial_ranges[i] = response.response[i];
    }

    // Manually shrink slices so FVM will differ from the partition.
    uint64_t offset = vslice_start[0] + response.response[0].count - 1;
    uint64_t length = 1;
    {
      const fidl::WireResult result = fidl::WireCall(vp->as_volume())->Shrink(offset, length);
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
    }

    // Check slice allocation after manual grow/shrink
    {
      const fidl::WireResult result = fidl::WireCall(vp->as_volume())
                                          ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(
                                              const_cast<size_t*>(vslice_start), vslice_count));
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
      ASSERT_EQ(vslice_count, response.response_count);
      ASSERT_FALSE(response.response[0].allocated);
      ASSERT_EQ(response.response[0].count, vslice_start[1] - vslice_start[0]);
    }

    // Try to mount the VPart. Since this mount call is supposed to fail, we wait for the spawned
    // fs process to finish and associated fidl channels to close before continuing to try and
    // prevent race conditions with the later mount call.
    fidl::ClientEnd device =
        fidl::ClientEnd<fuchsia_hardware_block::Block>(vp->partition.TakeChannel());
    ASSERT_NE(fs_management::Mount(std::move(device), component, mounting_options).status_value(),
              ZX_OK);

    // We can't reuse the component.
    component = fs_management::FsComponent::FromDiskFormat(disk_format);
  }

  {
    zx::result controller =
        fs_management::OpenPartitionWithDevfs(devfs_caller.directory(), kPartition1Matcher, true);
    ASSERT_OK(controller);
    zx::result vp = PartitionChannel::Create(std::move(controller.value()));
    ASSERT_OK(vp);

    // Grow back the slice we shrunk earlier.
    uint64_t offset = vslice_start[0];
    uint64_t length = 1;
    {
      const fidl::WireResult result = fidl::WireCall(vp->as_volume())->Extend(offset, length);
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
    }

    // Verify grow was successful.
    const fidl::WireResult result = fidl::WireCall(vp->as_volume())
                                        ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(
                                            const_cast<size_t*>(vslice_start), vslice_count));
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    ASSERT_EQ(vslice_count, response.response_count);
    ASSERT_TRUE(response.response[0].allocated);
    ASSERT_EQ(response.response[0].count, 1);

    // Now extend all extents by some number of additional slices.
    fuchsia_hardware_block_volume::wire::VsliceRange
        ranges_before_extend[fuchsia_hardware_block_volume::wire::kMaxSliceRequests];
    for (unsigned i = 0; i < vslice_count; i++) {
      ranges_before_extend[i] = response.response[i];
      uint64_t offset = vslice_start[i] + response.response[i].count;
      uint64_t length = vslice_count - i;
      {
        const fidl::WireResult result = fidl::WireCall(vp->as_volume())->Extend(offset, length);
        ASSERT_OK(result.status());
        const fidl::WireResponse response = result.value();
        ASSERT_OK(response.status);
      }
    }

    // Verify that the extensions were successful.
    {
      const fidl::WireResult result = fidl::WireCall(vp->as_volume())
                                          ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(
                                              const_cast<size_t*>(vslice_start), vslice_count));
      ASSERT_OK(result.status());
      const fidl::WireResponse response = result.value();
      ASSERT_OK(response.status);
      ASSERT_EQ(vslice_count, response.response_count);
      for (unsigned i = 0; i < vslice_count; i++) {
        ASSERT_TRUE(response.response[i].allocated);
        ASSERT_EQ(response.response[i].count, ranges_before_extend[i].count + vslice_count - i);
      }
    }

    // Try mount again.
    fidl::ClientEnd device =
        fidl::ClientEnd<fuchsia_hardware_block::Block>(vp->partition.TakeChannel());
    ASSERT_EQ(fs_management::Mount(std::move(device), component, mounting_options).status_value(),
              ZX_OK);
  }

  zx::result controller =
      fs_management::OpenPartitionWithDevfs(devfs_caller.directory(), kPartition1Matcher, true);
  ASSERT_OK(controller);
  zx::result vp = PartitionChannel::Create(std::move(controller.value()));
  ASSERT_OK(vp);

  // Verify that slices were fixed on mount.
  const fidl::WireResult result = fidl::WireCall(vp->as_volume())
                                      ->QuerySlices(fidl::VectorView<uint64_t>::FromExternal(
                                          const_cast<size_t*>(vslice_start), vslice_count));
  ASSERT_OK(result.status());
  const fidl::WireResponse response = result.value();
  ASSERT_OK(response.status);
  ASSERT_EQ(vslice_count, response.response_count);

  for (unsigned i = 0; i < vslice_count; i++) {
    ASSERT_TRUE(response.response[i].allocated);
    ASSERT_EQ(response.response[i].count, initial_ranges[i].count);
  }
}

TEST_F(FvmTest, TestCorruptMount) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  ASSERT_EQ(kSliceSize, volume_info_or->slice_size);

  // Allocate one VPart
  zx::result vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_OK(vp_or.status_value());

  zx::result partition_path = GetPartitionPath(vp_or.value().controller);
  ASSERT_OK(partition_path.status_value());

  size_t kMinfsBlocksPerSlice = kSliceSize / minfs::kMinfsBlockSize;
  size_t minfs_vslice_count = 4;
  size_t minfs_vslice_start[] = {
      minfs::kFVMBlockInodeBmStart / kMinfsBlocksPerSlice,
      minfs::kFVMBlockDataBmStart / kMinfsBlocksPerSlice,
      minfs::kFVMBlockInodeStart / kMinfsBlocksPerSlice,
      minfs::kFVMBlockDataStart / kMinfsBlocksPerSlice,
  };

  // Run the test for Minfs.
  fs_management::MountOptions mounting_options;
  CorruptMountHelper(devfs_root_fd(), partition_path->c_str(), mounting_options,
                     fs_management::kDiskFormatMinfs, minfs_vslice_start, minfs_vslice_count);

  size_t kBlobfsBlocksPerSlice = kSliceSize / blobfs::kBlobfsBlockSize;
  size_t blobfs_vslice_count = 3;
  size_t blobfs_vslice_start[] = {
      blobfs::kFVMBlockMapStart / kBlobfsBlocksPerSlice,
      blobfs::kFVMNodeMapStart / kBlobfsBlocksPerSlice,
      blobfs::kFVMDataStart / kBlobfsBlocksPerSlice,
  };

  // Run the test for Blobfs.
  CorruptMountHelper(devfs_root_fd(), partition_path->c_str(), mounting_options,
                     fs_management::kDiskFormatBlobfs, blobfs_vslice_start, blobfs_vslice_count);
}

TEST_F(FvmTest, TestVPartitionUpgrade) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  // Allocate two VParts, one active, and one inactive.
  {
    auto vp_fd_or = AllocatePartition({
        .type = kTestPartDataGuid,
        .guid = kTestUniqueGuid1,
        .name = kTestPartDataName,
        .flags = fuchsia_hardware_block_volume::wire::kAllocatePartitionFlagInactive,
    });
    ASSERT_EQ(vp_fd_or.status_value(), ZX_OK, "Couldn't open Volume");
  }

  {
    auto vp_fd_or = AllocatePartition({
        .type = kTestPartDataGuid,
        .guid = kTestUniqueGuid2,
        .name = kTestPartBlobName,
    });
    ASSERT_OK(vp_fd_or.status_value(), "Couldn't open volume");
  }

  // Release FVM device that we opened earlier
  FVMRebind();

  // The active partition should still exist.
  ASSERT_OK(WaitForPartition(kPartition2Matcher).status_value());
  // The inactive partition should be gone.
  ASSERT_STATUS(OpenPartition(kPartition1Matcher).status_value(), ZX_ERR_NOT_FOUND);

  // Reallocate GUID1 as inactive.

  {
    auto vp_fd_or = AllocatePartition({
        .type = kTestPartDataGuid,
        .guid = kTestUniqueGuid1,
        .name = kTestPartDataName,
        .flags = fuchsia_hardware_block_volume::wire::kAllocatePartitionFlagInactive,
    });
    ASSERT_OK(vp_fd_or.status_value(), "Couldn't open new volume");
  }

  // Atomically set GUID1 as active and GUID2 as inactive.
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    Upgrade(volume_manager.value(), kTestUniqueGuid2, kTestUniqueGuid1, ZX_OK);
  }
  // After upgrading, we should be able to open both partitions
  ASSERT_OK(WaitForPartition(kPartition1Matcher).status_value());
  ASSERT_OK(WaitForPartition(kPartition2Matcher).status_value());

  // Rebind the FVM driver, check that the upgrade has succeeded.
  // The original (GUID2) should be deleted, and the new partition (GUID)
  // should exist.
  FVMRebind();

  ASSERT_OK(WaitForPartition(kPartition1Matcher).status_value());
  ASSERT_STATUS(OpenPartition(kPartition2Matcher).status_value(), ZX_ERR_NOT_FOUND);

  // Try upgrading when the "new" version doesn't exist.
  // (It should return an error and have no noticeable effect).
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    Upgrade(volume_manager.value(), kTestUniqueGuid1, kTestUniqueGuid2, ZX_ERR_NOT_FOUND);
  }

  // Release FVM device that we opened earlier
  FVMRebind();

  ASSERT_OK(WaitForPartition(kPartition1Matcher).status_value());
  ASSERT_STATUS(OpenPartition(kPartition2Matcher).status_value(), ZX_ERR_NOT_FOUND);

  // Try upgrading when the "old" version doesn't exist.
  {
    auto vp_fd_or = AllocatePartition({
        .type = kTestPartDataGuid,
        .guid = kTestUniqueGuid2,
        .name = kTestPartBlobName,
        .flags = fuchsia_hardware_block_volume::wire::kAllocatePartitionFlagInactive,
    });
    ASSERT_EQ(vp_fd_or.status_value(), ZX_OK, "Couldn't open volume");
  }

  uuid::Uuid fake_guid = {};
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    Upgrade(volume_manager.value(), fake_guid, kTestUniqueGuid2, ZX_OK);
  }

  FVMRebind();

  // We should be able to open both partitions again.
  zx::result vp_or = WaitForPartition(kPartition1Matcher);
  ASSERT_OK(vp_or.status_value());
  ASSERT_OK(WaitForPartition(kPartition2Matcher).status_value());

  // Destroy and reallocate the first partition as inactive.
  {
    const fidl::WireResult result = fidl::WireCall(vp_or.value().as_volume())->Destroy();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  {
    zx::result vp_or = AllocatePartition({
        .type = kTestPartDataGuid,
        .guid = kTestUniqueGuid1,
        .name = kTestPartDataName,
        .flags = fuchsia_hardware_block_volume::wire::kAllocatePartitionFlagInactive,
    });
    ASSERT_EQ(vp_or.status_value(), ZX_OK, "Couldn't open volume");
  }

  // Upgrade the partition with old_guid == new_guid.
  // This should activate the partition.
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    Upgrade(volume_manager.value(), kTestUniqueGuid1, kTestUniqueGuid1, ZX_OK);
  }

  FVMRebind();

  // We should be able to open both partitions again.
  ASSERT_OK(WaitForPartition(kPartition1Matcher).status_value());
  ASSERT_OK(WaitForPartition(kPartition2Matcher).status_value());
}

// Test that the FVM driver can mount filesystems.
TEST_F(FvmTest, TestMounting) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);

  // Allocate one VPart
  size_t slice_count = 5;
  zx::result vp_or = AllocatePartition({
      .slice_count = slice_count,
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp(*std::move(vp_or));

  // Format the VPart as minfs
  zx::result partition_path = GetPartitionPath(vp.controller);
  ASSERT_OK(partition_path.status_value());
  auto component = fs_management::FsComponent::FromDiskFormat(fs_management::kDiskFormatMinfs);
  ASSERT_EQ(fs_management::Mkfs(partition_path->c_str(), component, fs_management::MkfsOptions()),
            ZX_OK);

  fidl::ClientEnd device =
      fidl::ClientEnd<fuchsia_hardware_block::Block>(vp.partition.TakeChannel());

  // Mount the VPart
  fs_management::MountOptions mounting_options;
  auto mounted_filesystem = fs_management::Mount(std::move(device), component, mounting_options);
  ASSERT_EQ(mounted_filesystem.status_value(), ZX_OK);
  auto data = mounted_filesystem->DataRoot();
  ASSERT_EQ(data.status_value(), ZX_OK);
  auto binding = fs_management::NamespaceBinding::Create(kMountPath, std::move(*data));
  ASSERT_EQ(binding.status_value(), ZX_OK);

  // Verify that the mount was successful.
  fbl::unique_fd rootfd;
  ASSERT_OK(fdio_open_fd(kMountPath, static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                         rootfd.reset_and_get_address()));
  fdio_cpp::FdioCaller caller(std::move(rootfd));
  auto result = fidl::WireCall(caller.directory())->QueryFilesystem();
  ASSERT_TRUE(result.ok());
  const char* kFsName = "minfs";
  const char* name = reinterpret_cast<const char*>(result.value().info->name.data());
  ASSERT_EQ(strncmp(name, kFsName, strlen(kFsName)), 0, "Unexpected filesystem mounted");

  // Verify that MinFS does not try to use more of the VPartition than
  // was originally allocated.
  ASSERT_LE(result.value().info->total_bytes, kSliceSize * slice_count);

  // Clean up.
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  }
}

// Test that FVM-aware filesystem can be reformatted.
TEST_F(FvmTest, TestMkfs) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);

  // Allocate one VPart.
  size_t slice_count = 5;
  zx::result vp_or = AllocatePartition({
      .slice_count = slice_count,
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp(*std::move(vp_or));

  // Format the VPart as minfs.
  zx::result partition_path = GetPartitionPath(vp.controller);
  ASSERT_OK(partition_path.status_value());
  auto minfs_component =
      fs_management::FsComponent::FromDiskFormat(fs_management::kDiskFormatMinfs);
  ASSERT_EQ(
      fs_management::Mkfs(partition_path->c_str(), minfs_component, fs_management::MkfsOptions()),
      ZX_OK);

  // Format it as MinFS again, even though it is already formatted.
  ASSERT_EQ(
      fs_management::Mkfs(partition_path->c_str(), minfs_component, fs_management::MkfsOptions()),
      ZX_OK);

  // Now try reformatting as blobfs.
  auto blobfs_component =
      fs_management::FsComponent::FromDiskFormat(fs_management::kDiskFormatBlobfs);
  ASSERT_EQ(fs_management::Mkfs(partition_path->c_str(), blobfs_component, {}), ZX_OK);

  // Demonstrate that mounting as minfs will fail, but mounting as blobfs
  // is successful.

  {
    fidl::ClientEnd device =
        fidl::ClientEnd<fuchsia_hardware_block::Block>(vp.partition.TakeChannel());
    fs_management::MountOptions mounting_options;
    ASSERT_NE(
        fs_management::Mount(std::move(device), minfs_component, mounting_options).status_value(),
        ZX_OK);
  }

  // We can't reuse the component.
  minfs_component = fs_management::FsComponent::FromDiskFormat(fs_management::kDiskFormatMinfs);

  {
    zx::result device = component::Connect<fuchsia_hardware_block::Block>(partition_path.value());
    ASSERT_OK(device);
    ASSERT_EQ(fs_management::Mount(std::move(device.value()), blobfs_component, {}).status_value(),
              ZX_OK);
  }

  // ... and reformat back to MinFS again.
  ASSERT_EQ(
      fs_management::Mkfs(partition_path->c_str(), minfs_component, fs_management::MkfsOptions()),
      ZX_OK);

  // Mount the VPart.
  zx::result device = component::Connect<fuchsia_hardware_block::Block>(partition_path.value());
  ASSERT_OK(device);
  fs_management::MountOptions mounting_options;
  auto mounted_filesystem =
      fs_management::Mount(std::move(device.value()), minfs_component, mounting_options);
  ASSERT_EQ(mounted_filesystem.status_value(), ZX_OK);
  auto data = mounted_filesystem->DataRoot();
  ASSERT_EQ(data.status_value(), ZX_OK);
  auto binding = fs_management::NamespaceBinding::Create(kMountPath, std::move(*data));
  ASSERT_EQ(binding.status_value(), ZX_OK);

  // Verify that the mount was successful.
  fbl::unique_fd rootfd;
  ASSERT_OK(fdio_open_fd(kMountPath, static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                         rootfd.reset_and_get_address()));
  ASSERT_TRUE(rootfd);
  fdio_cpp::FdioCaller caller(std::move(rootfd));
  auto result = fidl::WireCall(caller.directory())->QueryFilesystem();
  ASSERT_TRUE(result.ok());
  const char* kFsName = "minfs";
  const char* name = reinterpret_cast<const char*>(result.value().info->name.data());
  ASSERT_EQ(strncmp(name, kFsName, strlen(kFsName)), 0, "Unexpected filesystem mounted");

  // Verify that MinFS does not try to use more of the VPartition than
  // was originally allocated.
  ASSERT_LE(result.value().info->total_bytes, kSliceSize * slice_count);

  // Clean up.
  FVMCheckSliceSize(volume_manager.value(), kSliceSize);
}

// Test that the FVM can recover when one copy of
// metadata becomes corrupt.
TEST_F(FvmTest, TestCorruptionOk) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);

  // Allocate one VPart (writes to backup)
  zx::result vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp(*std::move(vp_or));

  // Extend the vpart (writes to primary)
  uint64_t offset = 1;
  uint64_t length = 1;
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  ASSERT_EQ(block_info.block_count * block_info.block_size, kSliceSize * 2);

  // Initial slice access
  CheckWriteReadBlock(vp.as_block(), 0, 1);
  // Extended slice access
  CheckWriteReadBlock(vp.as_block(), kSliceSize / block_info.block_size, 1);

  vp = {};

  // Corrupt the (backup) metadata and rebind.
  // The 'primary' was the last one written, so it'll be used.
  fvm::Header header =
      fvm::Header::FromDiskSize(fvm::kMaxUsablePartitions, kBlockSize * kBlockCount, kSliceSize);
  auto off = static_cast<off_t>(header.GetSuperblockOffset(fvm::SuperblockType::kSecondary));
  uint8_t buf[fvm::kBlockSize];
  fidl::UnownedClientEnd device = ramdisk_block_interface();
  ASSERT_OK(block_client::SingleReadBytes(device, buf, sizeof(buf), off));
  // Modify an arbitrary byte (not the magic bits; we still want it to mount!)
  buf[128]++;
  ASSERT_OK(block_client::SingleWriteBytes(device, buf, sizeof(buf), off));

  FVMRebind();

  vp_or = WaitForPartition(kPartition1Matcher);
  ASSERT_EQ(vp_or.status_value(), ZX_OK, "Couldn't re-open Data VPart");
  vp = *std::move(vp_or);

  // The slice extension is still accessible.
  CheckWriteReadBlock(vp.as_block(), 0, 1);
  CheckWriteReadBlock(vp.as_block(), kSliceSize / block_info.block_size, 1);

  // Clean up
  vp = {};

  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  }
}

TEST_F(FvmTest, TestCorruptionRegression) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);
  zx::result volume_manager = fvm_device();
  ASSERT_OK(volume_manager);

  auto volume_info_or = fs_management::FvmQuery(volume_manager.value());
  ASSERT_EQ(volume_info_or.status_value(), ZX_OK);
  size_t slice_size = volume_info_or->slice_size;

  // Allocate one VPart (writes to backup)
  zx::result vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp(*std::move(vp_or));

  // Extend the vpart (writes to primary)
  uint64_t offset = 1;
  uint64_t length = 1;
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  ASSERT_EQ(block_info.block_count * block_info.block_size, slice_size * 2);

  // Initial slice access
  CheckWriteReadBlock(vp.as_block(), 0, 1);
  // Extended slice access
  CheckWriteReadBlock(vp.as_block(), slice_size / block_info.block_size, 1);

  vp = {};

  // Corrupt the (primary) metadata and rebind.
  // The 'primary' was the last one written, so the backup will be used.
  off_t off = 0;
  uint8_t buf[fvm::kBlockSize];
  fidl::UnownedClientEnd device = ramdisk_block_interface();
  ASSERT_OK(block_client::SingleReadBytes(device, buf, sizeof(buf), off));
  buf[128]++;
  ASSERT_OK(block_client::SingleWriteBytes(device, buf, sizeof(buf), off));

  FVMRebind();

  vp_or = WaitForPartition(kPartition1Matcher);
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  vp = *std::move(vp_or);

  // The slice extension is no longer accessible
  CheckWriteReadBlock(vp.as_block(), 0, 1);
  CheckNoAccessBlock(vp.as_block(), slice_size / block_info.block_size, 1);

  // Clean up
  vp = {};
  {
    zx::result volume_manager = fvm_device();
    ASSERT_OK(volume_manager);
    FVMCheckSliceSize(volume_manager.value(), kSliceSize);
  }
}

TEST_F(FvmTest, TestCorruptionUnrecoverable) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  // Allocate one VPart (writes to backup)
  zx::result vp_or = AllocatePartition({
      .type = kTestPartDataGuid,
      .guid = kTestUniqueGuid1,
      .name = kTestPartDataName,
  });
  ASSERT_EQ(vp_or.status_value(), ZX_OK);
  PartitionChannel vp(*std::move(vp_or));

  // Extend the vpart (writes to primary)
  uint64_t offset = 1;
  uint64_t length = 1;
  {
    const fidl::WireResult result = fidl::WireCall(vp.as_volume())->Extend(offset, length);
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
  }
  const fidl::WireResult result = fidl::WireCall(vp.as_block())->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  ASSERT_EQ(block_info.block_count * block_info.block_size, kSliceSize * 2);

  // Initial slice access
  CheckWriteReadBlock(vp.as_block(), 0, 1);
  // Extended slice access
  CheckWriteReadBlock(vp.as_block(), kSliceSize / block_info.block_size, 1);

  vp = {};

  // Corrupt both copies of the metadata.
  // The 'primary' was the last one written, so the backup will be used.
  off_t off = 0;
  uint8_t buf[fvm::kBlockSize];
  fidl::UnownedClientEnd device = ramdisk_block_interface();
  ASSERT_OK(block_client::SingleReadBytes(device, buf, sizeof(buf), off));
  buf[128]++;
  ASSERT_OK(block_client::SingleWriteBytes(device, buf, sizeof(buf), off));

  fvm::Header header =
      fvm::Header::FromDiskSize(fvm::kMaxUsablePartitions, kBlockSize * kBlockCount, kSliceSize);
  off = static_cast<off_t>(header.GetSuperblockOffset(fvm::SuperblockType::kSecondary));
  ASSERT_OK(block_client::SingleReadBytes(device, buf, sizeof(buf), off));
  buf[128]++;
  ASSERT_OK(block_client::SingleWriteBytes(device, buf, sizeof(buf), off));

  ValidateFVM(ramdisk_block_interface(), ValidationResult::Corrupted);
}

// Tests the FVM checker against a just-initialized FVM.
TEST_F(FvmTest, TestCheckNewFVM) {
  CreateFVM(512, 1 << 20, 64LU * (1 << 20));
  fidl::UnownedClientEnd device = ramdisk_block_interface();
  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  const fuchsia_hardware_block::wire::BlockInfo& block_info = response.value()->info;
  fvm::Checker checker(device, block_info.block_size, true);
  ASSERT_TRUE(checker.Validate());
}

TEST_F(FvmTest, TestAbortDriverLoadSmallDevice) {
  constexpr uint64_t kMB = 1 << 20;
  constexpr uint64_t kGB = 1 << 30;
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 50 * kMB / kBlockSize;
  constexpr uint64_t kSliceSize = kMB;
  constexpr uint64_t kFvmPartitionSize = 4 * kGB;

  CreateRamdisk(kBlockSize, kBlockCount);

  // Init fvm with a partition bigger than the underlying disk.
  fs_management::FvmInitWithSize(ramdisk_block_interface(), kFvmPartitionSize, kSliceSize);

  // Try to bind an fvm to the disk.
  //
  // Bind should return ZX_ERR_IO when the load of a driver fails.
  auto resp = fidl::WireCall(ramdisk_controller_interface())->Bind(kFvmDriverLib);
  ASSERT_OK(resp.status());
  ASSERT_FALSE(resp->is_ok());
  ASSERT_EQ(resp->error_value(), ZX_ERR_INTERNAL);

  CreateRamdisk(kBlockSize, kBlockCount);
  fs_management::FvmInitWithSize(ramdisk_block_interface(), kFvmPartitionSize, kSliceSize);
  // Grow the ramdisk to the appropiate size and bind should succeed.
  ASSERT_OK(ramdisk_grow(ramdisk(), kFvmPartitionSize));
  // Use Controller::Call::Rebind because the driver might still be
  // when init fails. Driver removes the device and will eventually be
  // unloaded but Controller::Bind above does not wait until
  // the device is removed. Controller::Rebind ensures nothing is
  // bound to the device, before it tries to bind the driver again.
  auto resp2 = fidl::WireCall(ramdisk_controller_interface())->Rebind(kFvmDriverLib);
  ASSERT_OK(resp2.status());
  ASSERT_TRUE(resp2->is_ok());
  ASSERT_OK(device_watcher::RecursiveWaitForFile(devfs_root_fd().get(), fvm_path().c_str()));
}

TEST_F(FvmTest, TestPreventDuplicateDeviceNames) {
  constexpr uint64_t kBlockSize = 512;
  constexpr uint64_t kBlockCount = 1 << 16;
  constexpr uint64_t kSliceSize = 64 * kBlockSize;
  CreateFVM(kBlockSize, kBlockCount, kSliceSize);

  // When a partition is destroyed, the slot in FVM is synchronously freed but the device is
  // asynchronously removed. DFv2 prevents multiple child devices with the same name from being
  // bound. This test rapidly allocates and destroys the same partition to try and get a race
  // between the new device being bound and the old device being removed to try and get FVM to bind
  // multiple devices with the same name.
  for (int i = 0; i < 10; ++i) {
    zx::result vp_or = AllocatePartition({
        .type = kTestPartDataGuid,
        .guid = kTestUniqueGuid1,
        .name = kTestPartDataName,
    });
    ASSERT_OK(vp_or.status_value());
    auto result = fidl::WireCall(vp_or.value().as_volume())->Destroy();
    ASSERT_OK(result.status());
    ASSERT_OK(result->status);
  }
}

}  // namespace
