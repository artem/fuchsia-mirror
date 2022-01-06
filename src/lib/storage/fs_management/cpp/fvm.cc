// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/fs_management/cpp/fvm.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fuchsia/hardware/block/c/fidl.h>
#include <fuchsia/hardware/block/partition/c/fidl.h>
#include <fuchsia/hardware/block/volume/c/fidl.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/limits.h>
#include <lib/fdio/vfs.h>
#include <lib/fdio/watcher.h>
#include <lib/fit/defer.h>
#include <string.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/device/block.h>
#include <zircon/device/vfs.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

#include <memory>
#include <utility>

#include <fbl/string_printf.h>
#include <fbl/unique_fd.h>

#include "src/lib/storage/fs_management/cpp/fvm_internal.h"
#include "src/storage/fvm/fvm.h"

namespace fs_management {

bool PartitionMatches(zx::unowned_channel& partition_channel, const PartitionMatcher& matcher) {
  ZX_ASSERT(matcher.type_guid || matcher.instance_guid || matcher.num_labels > 0);
  if (matcher.num_labels > 0) {
    ZX_ASSERT(matcher.labels);
  }

  fuchsia_hardware_block_partition_GUID guid;
  zx_status_t io_status, status;
  if (matcher.type_guid) {
    io_status = fuchsia_hardware_block_partition_PartitionGetTypeGuid(partition_channel->get(),
                                                                      &status, &guid);
    if (io_status != ZX_OK || status != ZX_OK ||
        memcmp(guid.value, matcher.type_guid, BLOCK_GUID_LEN) != 0) {
      return false;
    }
  }
  if (matcher.instance_guid) {
    io_status = fuchsia_hardware_block_partition_PartitionGetInstanceGuid(partition_channel->get(),
                                                                          &status, &guid);
    if (io_status != ZX_OK || status != ZX_OK ||
        memcmp(guid.value, matcher.instance_guid, BLOCK_GUID_LEN) != 0) {
      return false;
    }
  }
  if (matcher.num_labels > 0) {
    char name[fuchsia_hardware_block_partition_NAME_LENGTH];
    size_t name_len;
    io_status = fuchsia_hardware_block_partition_PartitionGetName(partition_channel->get(), &status,
                                                                  name, sizeof(name), &name_len);
    if (io_status != ZX_OK || status != ZX_OK || name_len == 0) {
      return false;
    }
    bool matches_label = false;
    for (size_t i = 0; i < matcher.num_labels; ++i) {
      if (!strncmp(name, matcher.labels[i], name_len)) {
        matches_label = true;
        break;
      }
    }
    if (!matches_label) {
      return false;
    }
  }
  return true;
}

}  // namespace fs_management

namespace {

constexpr char kBlockDevPath[] = "/dev/class/block/";
constexpr char kBlockDevRelativePath[] = "class/block/";
// Overwrites the FVM and waits for it to disappear from devfs.
//
// devfs_root_fd: (OPTIONAL) A connection to devfs. If supplied, |path| is relative to this root.
// parent_fd: An fd to the parent of the FVM device.
// path: The path to the FVM device. Relative to |devfs_root_fd| if supplied.
zx_status_t DestroyFVMAndWait(int devfs_root_fd, fbl::unique_fd parent_fd, fbl::unique_fd driver_fd,
                              const char* path) {
  fuchsia_hardware_block_volume_VolumeManagerInfo volume_info;
  zx_status_t status = fvm_query(driver_fd.get(), &volume_info);
  if (status != ZX_OK) {
    return ZX_ERR_WRONG_TYPE;
  }

  struct fvm_destroyer {
    int devfs_root_fd;
    uint64_t slice_size;
    const char* path;
    bool destroyed;
  } destroyer;
  destroyer.devfs_root_fd = devfs_root_fd;
  destroyer.slice_size = volume_info.slice_size;
  destroyer.path = path;
  destroyer.destroyed = false;

  auto cb = [](int dirfd, int event, const char* fn, void* cookie) {
    auto destroyer = static_cast<fvm_destroyer*>(cookie);
    if (event == WATCH_EVENT_WAITING) {
      zx_status_t status = ZX_ERR_INTERNAL;
      if (destroyer->devfs_root_fd != -1) {
        status = fvm_overwrite_with_devfs(destroyer->devfs_root_fd, destroyer->path,
                                          destroyer->slice_size);
      } else {
        status = fvm_overwrite(destroyer->path, destroyer->slice_size);
      }
      destroyer->destroyed = true;
      return status;
    }
    if ((event == WATCH_EVENT_REMOVE_FILE) && !strcmp(fn, "fvm")) {
      return ZX_ERR_STOP;
    }
    return ZX_OK;
  };
  status = fdio_watch_directory(parent_fd.get(), cb, zx::time::infinite().get(), &destroyer);
  if (status != ZX_ERR_STOP) {
    return status;
  }
  return ZX_OK;
}

}  // namespace

__EXPORT
zx_status_t fvm_init_preallocated(int fd, uint64_t initial_volume_size, uint64_t max_volume_size,
                                  size_t slice_size) {
  if (slice_size % fvm::kBlockSize != 0) {
    // Alignment
    return ZX_ERR_INVALID_ARGS;
  } else if ((slice_size * fvm::kMaxVSlices) / fvm::kMaxVSlices != slice_size) {
    // Overflow
    return ZX_ERR_INVALID_ARGS;
  } else if (initial_volume_size > max_volume_size || initial_volume_size == 0 ||
             max_volume_size == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  fvm::Header header = fvm::Header::FromGrowableDiskSize(
      fvm::kMaxUsablePartitions, initial_volume_size, max_volume_size, slice_size);
  if (header.pslice_count == 0) {
    return ZX_ERR_NO_SPACE;
  }

  // This buffer needs to hold both copies of the metadata.
  // TODO(fxbug.dev/60709): Eliminate layout assumptions.
  size_t metadata_allocated_bytes = header.GetMetadataAllocatedBytes();
  size_t dual_metadata_bytes = metadata_allocated_bytes * 2;
  std::unique_ptr<uint8_t[]> mvmo(new uint8_t[dual_metadata_bytes]);
  // Clear entire primary copy of metadata
  memset(mvmo.get(), 0, metadata_allocated_bytes);

  // Save the header to our primary metadata.
  memcpy(mvmo.get(), &header, sizeof(fvm::Header));
  size_t metadata_used_bytes = header.GetMetadataUsedBytes();
  fvm::UpdateHash(mvmo.get(), metadata_used_bytes);

  // Copy the new primary metadata to the backup copy.
  void* backup = mvmo.get() + header.GetSuperblockOffset(fvm::SuperblockType::kSecondary);
  memcpy(backup, mvmo.get(), metadata_allocated_bytes);

  // Validate our new state.
  if (!fvm::PickValidHeader(mvmo.get(), backup, metadata_used_bytes)) {
    return ZX_ERR_BAD_STATE;
  }

  if (lseek(fd, 0, SEEK_SET) < 0) {
    return ZX_ERR_BAD_STATE;
  }
  // Write to primary copy.
  if (write(fd, mvmo.get(), metadata_allocated_bytes) !=
      static_cast<ssize_t>(metadata_allocated_bytes)) {
    return ZX_ERR_BAD_STATE;
  }
  // Write to secondary copy, to overwrite any previous FVM metadata copy that
  // could be here.
  if (write(fd, mvmo.get(), metadata_allocated_bytes) !=
      static_cast<ssize_t>(metadata_allocated_bytes)) {
    return ZX_ERR_BAD_STATE;
  }

  return ZX_OK;
}

__EXPORT
zx_status_t fvm_init_with_size(int fd, uint64_t volume_size, size_t slice_size) {
  return fvm_init_preallocated(fd, volume_size, volume_size, slice_size);
}

__EXPORT
zx_status_t fvm_init(int fd, size_t slice_size) {
  // The metadata layout of the FVM is dependent on the
  // size of the FVM's underlying partition.
  fuchsia_hardware_block_BlockInfo block_info;
  fdio_cpp::UnownedFdioCaller disk_connection(fd);
  zx_status_t status;
  zx_status_t io_status =
      fuchsia_hardware_block_BlockGetInfo(disk_connection.borrow_channel(), &status, &block_info);
  if (io_status != ZX_OK) {
    return io_status;
  } else if (status != ZX_OK) {
    return status;
  } else if (slice_size == 0 || slice_size % block_info.block_size) {
    return ZX_ERR_BAD_STATE;
  }

  return fvm_init_with_size(fd, block_info.block_count * block_info.block_size, slice_size);
}

// Helper function to overwrite FVM given the slice_size
zx_status_t fvm_overwrite_impl(const fbl::unique_fd& fd, size_t slice_size) {
  fuchsia_hardware_block_BlockInfo block_info;
  fdio_cpp::UnownedFdioCaller disk_connection(fd.get());
  zx::unowned_channel channel(disk_connection.borrow_channel());
  zx_status_t status;
  zx_status_t io_status = fuchsia_hardware_block_BlockGetInfo(channel->get(), &status, &block_info);
  if (io_status != ZX_OK) {
    return io_status;
  } else if (status != ZX_OK) {
    return status;
  }

  size_t disk_size = block_info.block_count * block_info.block_size;
  fvm::Header header = fvm::Header::FromDiskSize(fvm::kMaxUsablePartitions, disk_size, slice_size);

  // Overwrite all the metadata from the beginning of the device to the start of the data.
  // TODO(jfsulliv) Use MetadataBuffer::BytesNeeded() when that's ready.
  size_t metadata_size = header.GetDataStartOffset();
  std::unique_ptr<uint8_t[]> buf(new uint8_t[metadata_size]);
  memset(buf.get(), 0, metadata_size);

  if (lseek(fd.get(), 0, SEEK_SET) < 0) {
    return ZX_ERR_IO;
  }
  if (write(fd.get(), buf.get(), metadata_size) != static_cast<ssize_t>(metadata_size)) {
    fprintf(stderr, "fvm_overwrite_impl: Failed to write metadata\n");
    return ZX_ERR_IO;
  }

  io_status = fuchsia_hardware_block_BlockRebindDevice(channel->get(), &status);
  if (io_status != ZX_OK) {
    return io_status;
  }
  return status;
}

__EXPORT
zx_status_t fvm_overwrite(const char* path, size_t slice_size) {
  fbl::unique_fd fd(open(path, O_RDWR));
  if (!fd) {
    fprintf(stderr, "fvm_overwrite: Failed to open block device\n");
    return ZX_ERR_BAD_STATE;
  }
  return fvm_overwrite_impl(fd, slice_size);
}

__EXPORT
zx_status_t fvm_overwrite_with_devfs(int devfs_root_fd, const char* relative_path,
                                     size_t slice_size) {
  fbl::unique_fd fd(openat(devfs_root_fd, relative_path, O_RDWR));
  if (!fd) {
    fprintf(stderr, "fvm_overwrite_with_devfs: Failed to open block device\n");
    return ZX_ERR_BAD_STATE;
  }
  return fvm_overwrite_impl(fd, slice_size);
}

// Helper function to destroy FVM
__EXPORT
zx_status_t fvm_destroy(const char* path) {
  fbl::String driver_path = fbl::StringPrintf("%s/fvm", path);

  fbl::unique_fd parent_fd(open(path, O_RDONLY | O_DIRECTORY));
  if (!parent_fd) {
    return ZX_ERR_NOT_FOUND;
  }
  fbl::unique_fd fvm_fd(open(driver_path.c_str(), O_RDWR));
  if (!fvm_fd) {
    return ZX_ERR_NOT_FOUND;
  }
  return DestroyFVMAndWait(-1, std::move(parent_fd), std::move(fvm_fd), path);
}

__EXPORT
zx_status_t fvm_destroy_with_devfs(int devfs_root_fd, const char* relative_path) {
  fbl::String driver_path = fbl::StringPrintf("%s/fvm", relative_path);

  fbl::unique_fd parent_fd(openat(devfs_root_fd, relative_path, O_RDONLY | O_DIRECTORY));
  if (!parent_fd) {
    return ZX_ERR_NOT_FOUND;
  }
  fbl::unique_fd fvm_fd(openat(devfs_root_fd, driver_path.c_str(), O_RDWR));
  if (!fvm_fd) {
    return ZX_ERR_NOT_FOUND;
  }
  return DestroyFVMAndWait(devfs_root_fd, std::move(parent_fd), std::move(fvm_fd), relative_path);
}

int fvm_allocate_partition_impl(int fvm_fd, const alloc_req_t* request) {
  fdio_cpp::UnownedFdioCaller caller(fvm_fd);

  fuchsia_hardware_block_partition::wire::Guid type_guid;
  memcpy(type_guid.value.data(), request->type, BLOCK_GUID_LEN);
  fuchsia_hardware_block_partition::wire::Guid instance_guid;
  memcpy(instance_guid.value.data(), request->guid, BLOCK_GUID_LEN);

  // TODO(fxbug.dev/52757): Add name_size to alloc_req_t.
  //
  // Here, we rely on request->name being a C-style string terminated by \x00,
  // but no greater than BLOCK_NAME_LEN. Instead, we should add a name_size
  // field to the alloc_req_t object to pass this explicitely.
  size_t request_name_size = BLOCK_NAME_LEN;
  for (size_t i = 0; i < BLOCK_NAME_LEN; i++) {
    if (request->name[i] == 0) {
      request_name_size = i;
      break;
    }
  }
  fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager> client(
      caller.borrow_channel());
  auto response = fidl::WireCall(client)->AllocatePartition(
      request->slice_count, type_guid, instance_guid,
      fidl::StringView::FromExternal(request->name, request_name_size), request->flags);
  if (response.status() != ZX_OK || response->status != ZX_OK) {
    return -1;
  }
  return 0;
}

// Helper function to allocate, find, and open VPartition.
__EXPORT
int fvm_allocate_partition(int fvm_fd, const alloc_req_t* request) {
  int alloc_status = fvm_allocate_partition_impl(fvm_fd, request);
  if (alloc_status != 0) {
    return alloc_status;
  }
  PartitionMatcher matcher{
      .type_guid = request->type,
      .instance_guid = request->guid,
  };
  return open_partition(&matcher, ZX_SEC(10), nullptr);
}

__EXPORT
int fvm_allocate_partition_with_devfs(int devfs_root_fd, int fvm_fd, const alloc_req_t* request) {
  int alloc_status = fvm_allocate_partition_impl(fvm_fd, request);
  if (alloc_status != 0) {
    return alloc_status;
  }
  PartitionMatcher matcher{
      .type_guid = request->type,
      .instance_guid = request->guid,
  };
  return open_partition_with_devfs(devfs_root_fd, &matcher, ZX_SEC(10), nullptr);
}

__EXPORT
zx_status_t fvm_query(int fvm_fd, fuchsia_hardware_block_volume_VolumeManagerInfo* out) {
  fdio_cpp::UnownedFdioCaller caller(fvm_fd);

  auto response =
      fidl::WireCall(fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager>(
                         caller.borrow_channel()))
          ->GetInfo();

  if (response.status() != ZX_OK)
    return response.status();
  if (response->status != ZX_OK)
    return response->status;
  *out = reinterpret_cast<fuchsia_hardware_block_volume_VolumeManagerInfo&>(*response->info);
  return ZX_OK;
}

// Takes ownership of |dir|.
int open_partition_impl(DIR* dir, const char* out_path_base, const PartitionMatcher* matcher,
                        zx_duration_t timeout, char* out_path) {
  ZX_ASSERT(matcher != nullptr);
  auto cleanup = fit::defer([&]() { closedir(dir); });

  typedef struct {
    const PartitionMatcher* matcher;
    const char* out_path_base;
    char* out_path;
    fbl::unique_fd out_partition;
  } alloc_helper_info_t;

  alloc_helper_info_t info;
  info.matcher = matcher;
  info.out_path_base = out_path_base;
  info.out_path = out_path;
  info.out_partition.reset();

  auto cb = [](int dirfd, int event, const char* fn, void* cookie) {
    if (event != WATCH_EVENT_ADD_FILE) {
      return ZX_OK;
    } else if ((strcmp(fn, ".") == 0) || strcmp(fn, "..") == 0) {
      return ZX_OK;
    }
    auto info = static_cast<alloc_helper_info_t*>(cookie);
    fbl::unique_fd devfd(openat(dirfd, fn, O_RDWR));
    if (!devfd) {
      return ZX_OK;
    }

    fdio_cpp::UnownedFdioCaller connection(devfd.get());
    zx::unowned_channel channel(connection.borrow_channel());
    if (fs_management::PartitionMatches(channel, *(info->matcher))) {
      info->out_partition = std::move(devfd);
      if (info->out_path) {
        strcpy(info->out_path, info->out_path_base);
        strcat(info->out_path, fn);
      }
      return ZX_ERR_STOP;
    }
    return ZX_OK;
  };

  zx_time_t deadline = zx_deadline_after(timeout);
  if (fdio_watch_directory(dirfd(dir), cb, deadline, &info) != ZX_ERR_STOP) {
    return -1;
  }
  return info.out_partition.release();
}

__EXPORT
int open_partition(const PartitionMatcher* matcher, zx_duration_t timeout, char* out_path) {
  DIR* dir = opendir(kBlockDevPath);
  if (dir == nullptr) {
    return -1;
  }

  return open_partition_impl(dir, kBlockDevPath, matcher, timeout, out_path);
}

__EXPORT
int open_partition_with_devfs(int devfs_root_fd, const PartitionMatcher* matcher,
                              zx_duration_t timeout, char* out_path_relative) {
  fbl::unique_fd block_dev_fd(openat(devfs_root_fd, kBlockDevRelativePath, O_RDONLY));
  if (!block_dev_fd) {
    return -1;
  }
  DIR* dir = fdopendir(block_dev_fd.get());
  if (dir == nullptr) {
    return -1;
  }

  return open_partition_impl(dir, kBlockDevRelativePath, matcher, timeout, out_path_relative);
}

zx_status_t destroy_partition_impl(fbl::unique_fd&& fd) {
  fdio_cpp::FdioCaller partition_caller(std::move(fd));

  zx_status_t status;
  zx_status_t io_status =
      fuchsia_hardware_block_volume_VolumeDestroy(partition_caller.borrow_channel(), &status);
  if (io_status) {
    return io_status;
  }
  return status;
}

__EXPORT
zx_status_t destroy_partition(const uint8_t* uniqueGUID, const uint8_t* typeGUID) {
  char path[PATH_MAX];
  PartitionMatcher matcher{
      .type_guid = typeGUID,
      .instance_guid = uniqueGUID,
  };
  fbl::unique_fd fd(open_partition(&matcher, 0, path));
  if (!fd) {
    return ZX_ERR_IO;
  }
  return destroy_partition_impl(std::move(fd));
}

__EXPORT
zx_status_t destroy_partition_with_devfs(int devfs_root_fd, const uint8_t* uniqueGUID,
                                         const uint8_t* typeGUID) {
  char relative_path[PATH_MAX];
  PartitionMatcher matcher{
      .type_guid = typeGUID,
      .instance_guid = uniqueGUID,
  };
  fbl::unique_fd fd(open_partition_with_devfs(devfs_root_fd, &matcher, 0, relative_path));
  if (!fd) {
    return ZX_ERR_IO;
  }
  return destroy_partition_impl(std::move(fd));
}
