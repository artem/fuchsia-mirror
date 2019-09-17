// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fvm.h"

#include <dirent.h>
#include <fcntl.h>
#include <fuchsia/device/llcpp/fidl.h>
#include <fuchsia/hardware/block/llcpp/fidl.h>
#include <fuchsia/hardware/block/partition/llcpp/fidl.h>
#include <fuchsia/hardware/block/volume/llcpp/fidl.h>
#include <lib/fzl/fdio.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/channel.h>
#include <lib/zx/fifo.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <fbl/algorithm.h>
#include <fbl/array.h>
#include <fbl/unique_fd.h>
#include <fbl/unique_ptr.h>
#include <fs-management/fvm.h>
#include <fs-management/mount.h>
#include <fvm/format.h>
#include <fvm/fvm-sparse.h>
#include <ramdevice-client/ramdisk.h>
#include <zxcrypt/fdio-volume.h>

#include "pave-logging.h"

namespace paver {
namespace {

namespace block = ::llcpp::fuchsia::hardware::block;
namespace partition = ::llcpp::fuchsia::hardware::block::partition;
namespace volume = ::llcpp::fuchsia::hardware::block::volume;

using ::llcpp::fuchsia::hardware::block::volume::VolumeInfo;

// The number of additional slices a partition will need to become
// zxcrypt'd.
//
// TODO(aarongreen): Replace this with a value supplied by ulib/zxcrypt.
constexpr size_t kZxcryptExtraSlices = 1;

// Looks up the topological path of a device.
// |buf| is the buffer the path will be written to.  |buf_len| is the total
// capcity of the buffer, including space for a null byte.
// Upon success, |buf| will contain the null-terminated topological path.
zx_status_t GetTopoPathFromFd(const fbl::unique_fd& fd, char* buf, size_t buf_len) {
  fzl::UnownedFdioCaller caller(fd.get());
  auto result = ::llcpp::fuchsia::device::Controller::Call::GetTopologicalPath(caller.channel());
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.status != ZX_OK) {
    return response.status;
  }
  strncpy(buf, response.path.data(), std::min(buf_len, response.path.size()));
  buf[response.path.size()] = '\0';
  return ZX_OK;
}

// Confirm that the file descriptor to the underlying partition exists within an
// FVM, not, for example, a GPT or MBR.
//
// |out| is true if |fd| is a VPartition, else false.
zx_status_t FvmIsVirtualPartition(const fbl::unique_fd& fd, bool* out) {
  char path[PATH_MAX];
  zx_status_t status = GetTopoPathFromFd(fd, path, sizeof(path));
  if (status != ZX_OK) {
    return ZX_ERR_IO;
  }

  *out = strstr(path, "fvm") != nullptr;
  return ZX_OK;
}

// Describes the state of a partition actively being written
// out to disk.
struct PartitionInfo {
  PartitionInfo() : pd(nullptr) {}

  fvm::partition_descriptor_t* pd;
  fbl::unique_fd new_part;
};

inline fvm::extent_descriptor_t* GetExtent(fvm::partition_descriptor_t* pd, size_t extent) {
  return reinterpret_cast<fvm::extent_descriptor_t*>(reinterpret_cast<uintptr_t>(pd) +
                                                     sizeof(fvm::partition_descriptor_t) +
                                                     extent * sizeof(fvm::extent_descriptor_t));
}

}  // namespace

// Registers a FIFO
zx_status_t RegisterFastBlockIo(const fbl::unique_fd& fd, const zx::vmo& vmo, vmoid_t* out_vmoid,
                                block_client::Client* out_client) {
  fzl::UnownedFdioCaller caller(fd.get());

  auto result = block::Block::Call::GetFifo(caller.channel());
  if (!result.ok()) {
    return result.status();
  }
  auto& response = result.value();
  if (response.status != ZX_OK) {
    return response.status;
  }

  zx::vmo dup;
  if (vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) != ZX_OK) {
    ERROR("Couldn't duplicate buffer vmo\n");
    return ZX_ERR_IO;
  }

  auto result2 = block::Block::Call::AttachVmo(caller.channel(), std::move(dup));
  if (result2.status() != ZX_OK) {
    return result2.status();
  }
  const auto& response2 = result2.value();
  if (response2.status != ZX_OK) {
    return response2.status;
  }

  *out_vmoid = response2.vmoid->id;
  return block_client::Client::Create(std::move(response.fifo), out_client);
}

namespace {

zx_status_t FlushClient(block_client::Client* client) {
  block_fifo_request_t request;
  request.group = 0;
  request.vmoid = block::VMOID_INVALID;
  request.opcode = BLOCKIO_FLUSH;
  request.length = 0;
  request.vmo_offset = 0;
  request.dev_offset = 0;

  return client->Transaction(&request, 1);
}

// Stream an FVM partition to disk.
zx_status_t StreamFvmPartition(fvm::SparseReader* reader, PartitionInfo* part,
                               const fzl::VmoMapper& mapper, const block_client::Client& client,
                               size_t block_size, block_fifo_request_t* request) {
  size_t slice_size = reader->Image()->slice_size;
  const size_t vmo_cap = mapper.size();
  for (size_t e = 0; e < part->pd->extent_count; e++) {
    LOG("Writing extent %zu... \n", e);
    fvm::extent_descriptor_t* ext = GetExtent(part->pd, e);
    size_t offset = ext->slice_start * slice_size;
    size_t bytes_left = ext->extent_length;

    // Write real data
    while (bytes_left > 0) {
      size_t vmo_sz = 0;
      size_t actual;
      zx_status_t status = reader->ReadData(&reinterpret_cast<uint8_t*>(mapper.start())[vmo_sz],
                                            fbl::min(bytes_left, vmo_cap - vmo_sz), &actual);
      vmo_sz += actual;
      bytes_left -= actual;

      if (vmo_sz == 0) {
        ERROR("Read nothing from src_fd; %zu bytes left\n", bytes_left);
        return ZX_ERR_IO;
      } else if (vmo_sz % block_size != 0) {
        ERROR("Cannot write non-block size multiple: %zu\n", vmo_sz);
        return ZX_ERR_IO;
      } else if (status != ZX_OK) {
        ERROR("Error reading partition data\n");
        return status;
      }

      uint64_t length = vmo_sz / block_size;
      if (length > UINT32_MAX) {
        ERROR("Error writing partition: Too large\n");
        return ZX_ERR_OUT_OF_RANGE;
      }
      request->length = static_cast<uint32_t>(length);
      request->vmo_offset = 0;
      request->dev_offset = offset / block_size;

      ssize_t r;
      if ((r = client.Transaction(request, 1)) != ZX_OK) {
        ERROR("Error writing partition data\n");
        return static_cast<zx_status_t>(r);
      }

      offset += vmo_sz;
    }

    // Write trailing zeroes (which are implied, but were omitted from
    // transfer).
    bytes_left = (ext->slice_count * slice_size) - ext->extent_length;
    if (bytes_left > 0) {
      LOG("%zu bytes written, %zu zeroes left\n", ext->extent_length, bytes_left);
      memset(mapper.start(), 0, vmo_cap);
    }
    while (bytes_left > 0) {
      uint64_t length = fbl::min(bytes_left, vmo_cap) / block_size;
      if (length > UINT32_MAX) {
        ERROR("Error writing trailing zeroes: Too large\n");
        return ZX_ERR_OUT_OF_RANGE;
      }
      request->length = static_cast<uint32_t>(length);
      request->vmo_offset = 0;
      request->dev_offset = offset / block_size;

      zx_status_t status;
      if ((status = client.Transaction(request, 1)) != ZX_OK) {
        ERROR("Error writing trailing zeroes\n");
        return status;
      }

      offset += request->length * block_size;
      bytes_left -= request->length * block_size;
    }
  }
  return ZX_OK;
}

// Attempt to bind an FVM driver to a partition fd.
fbl::unique_fd TryBindToFvmDriver(const fbl::unique_fd& devfs_root,
                                  const fbl::unique_fd& partition_fd, zx::duration timeout) {
  char path[PATH_MAX] = {};
  zx_status_t status = GetTopoPathFromFd(partition_fd, path, sizeof(path));
  if (status != ZX_OK) {
    ERROR("Failed to get topological path\n");
    return fbl::unique_fd();
  }

  // We assume the FVM will either have completed binding, or is not bound at all. This is ensured
  // by the paver always waiting for the FVM to bind after invoking ControllerBind.
  char fvm_path[PATH_MAX];
  snprintf(fvm_path, sizeof(fvm_path), "%s/fvm", &path[5]);

  fbl::unique_fd fvm(openat(devfs_root.get(), fvm_path, O_RDWR));
  if (fvm) {
    return fvm;
  }

  fzl::UnownedFdioCaller caller(partition_fd.get());
  constexpr char kFvmDriverLib[] = "/boot/driver/fvm.so";
  auto result = ::llcpp::fuchsia::device::Controller::Call::Bind(
      caller.channel(), fidl::StringView(kFvmDriverLib));
  status = result.ok() ? result.value().status : result.status();
  if (status != ZX_OK) {
    ERROR("Could not bind fvm driver\n");
    return fbl::unique_fd();
  }

  if (wait_for_device_at(devfs_root.get(), fvm_path, timeout.get()) != ZX_OK) {
    ERROR("Error waiting for fvm driver to bind\n");
    return fbl::unique_fd();
  }
  return fbl::unique_fd(openat(devfs_root.get(), fvm_path, O_RDWR));
}

}  // namespace

// Formats the FVM within the provided partition if it is not already formatted.
//
// On success, returns a file descriptor to an FVM.
// On failure, returns -1
fbl::unique_fd FvmPartitionFormat(const fbl::unique_fd& devfs_root, fbl::unique_fd partition_fd,
                                  size_t slice_size, BindOption option) {
  // Although the format (based on the magic in the FVM superblock)
  // indicates this is (or at least was) an FVM image, it may be invalid.
  //
  // Attempt to bind the FVM driver to this partition, but fall-back to
  // reinitializing the FVM image so the rest of the paving
  // process can continue successfully.
  fbl::unique_fd fvm_fd;
  if (option == BindOption::TryBind) {
    disk_format_t df = detect_disk_format(partition_fd.get());
    if (df == DISK_FORMAT_FVM) {
      fvm_fd = TryBindToFvmDriver(devfs_root, partition_fd, zx::sec(3));
      if (fvm_fd) {
        LOG("Found already formatted FVM.\n");
        VolumeInfo info;
        zx_status_t status = fvm_query(
            fvm_fd.get(), reinterpret_cast<fuchsia_hardware_block_volume_VolumeInfo*>(&info));
        if (status == ZX_OK) {
          if (info.slice_size == slice_size) {
            return fvm_fd;
          } else {
            ERROR("Mismatched slice size. Reinitializing FVM.\n");
          }
        } else {
          ERROR("Could not query FVM for info. Reinitializing FVM.\n");
        }
      } else {
        ERROR("Saw DISK_FORMAT_FVM, but could not bind driver. Reinitializing FVM.\n");
      }
    }
  }

  LOG("Initializing partition as FVM\n");
  zx_status_t status = fvm_init(partition_fd.get(), slice_size);
  if (status != ZX_OK) {
    ERROR("Failed to initialize fvm: %s\n", zx_status_get_string(status));
    return fbl::unique_fd();
  }

  {
    fzl::UnownedFdioCaller partition_connection(partition_fd.get());
    auto result = block::Block::Call::RebindDevice(partition_connection.channel());
    status = result.ok() ? result.value().status : result.status();
    if (status != ZX_OK) {
      ERROR("Could not rebind partition: %s\n", zx_status_get_string(status));
      return fbl::unique_fd();
    }
  }

  return TryBindToFvmDriver(devfs_root, partition_fd, zx::sec(3));
}

namespace {

// Formats a block device as a zxcrypt volume.
//
// On success, returns a file descriptor to an FVM.
// On failure, returns -1
zx_status_t ZxcryptCreate(PartitionInfo* part) {
  zx_status_t status;

  char path[PATH_MAX];
  status = GetTopoPathFromFd(part->new_part, path, sizeof(path));
  if (status != ZX_OK) {
    ERROR("Failed to get topological path\n");
    return status;
  }
  // TODO(security): ZX-1130. We need to bind with channel in order to pass a key here.
  // TODO(security): ZX-1864. The created volume must marked as needing key rotation.

  fbl::unique_fd devfs_root(open("/dev", O_RDONLY));
  fbl::unique_ptr<zxcrypt::FdioVolume> volume;
  if ((status = zxcrypt::FdioVolume::CreateWithDeviceKey(
           std::move(part->new_part), std::move(devfs_root), &volume)) != ZX_OK) {
    ERROR("Could not create zxcrypt volume\n");
    return status;
  }
  zx::channel zxcrypt_manager_chan;
  if ((status = volume->OpenManager(zx::sec(3), zxcrypt_manager_chan.reset_and_get_address())) !=
      ZX_OK) {
    ERROR("Could not open zxcrypt volume manager\n");
    return status;
  }

  zxcrypt::FdioVolumeManager zxcrypt_manager(std::move(zxcrypt_manager_chan));
  uint8_t slot = 0;
  if ((status = zxcrypt_manager.UnsealWithDeviceKey(slot)) != ZX_OK) {
    ERROR("Could not unseal zxcrypt volume\n");
    return status;
  }

  if ((status = volume->Open(zx::sec(3), &part->new_part)) != ZX_OK) {
    ERROR("Could not open zxcrypt volume\n");
    return status;
  }

  fvm::extent_descriptor_t* ext = GetExtent(part->pd, 0);
  size_t reserved = volume->reserved_slices();

  // |Create| guarantees at least |reserved| + 1 slices are allocated.  If the first extent had a
  // single slice, we're done.
  size_t allocated = fbl::max(reserved + 1, ext->slice_count);
  size_t needed = reserved + ext->slice_count;
  if (allocated >= needed) {
    return ZX_OK;
  }

  // Otherwise, extend by the number of slices we stole for metadata
  uint64_t offset = allocated - reserved;
  uint64_t length = needed - allocated;
  {
    fzl::UnownedFdioCaller partition_connection(part->new_part.get());
    auto result = volume::Volume::Call::Extend(partition_connection.channel(), offset, length);
    status = result.ok() ? result.value().status : result.status();
    if (status != ZX_OK) {
      ERROR("Failed to extend zxcrypt volume: %s\n", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

// Returns |ZX_OK| if |partition_fd| is a child of |fvm_fd|.
zx_status_t FvmPartitionIsChild(const fbl::unique_fd& fvm_fd, const fbl::unique_fd& partition_fd) {
  char fvm_path[PATH_MAX];
  char part_path[PATH_MAX];
  zx_status_t status;
  if ((status = GetTopoPathFromFd(fvm_fd, fvm_path, sizeof(fvm_path))) != ZX_OK) {
    ERROR("Couldn't get topological path of FVM\n");
    return status;
  } else if ((status = GetTopoPathFromFd(partition_fd, part_path, sizeof(part_path))) != ZX_OK) {
    ERROR("Couldn't get topological path of partition\n");
    return status;
  }
  if (strncmp(fvm_path, part_path, strlen(fvm_path))) {
    ERROR("Partition does not exist within FVM\n");
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

void RecommendWipe(const char* problem) {
  Warn(problem, "Please run 'install-disk-image wipe' to wipe your partitions");
}

// Deletes all partitions within the FVM with a type GUID matching |type_guid|
// until there are none left.
zx_status_t WipeAllFvmPartitionsWithGUID(const fbl::unique_fd& fvm_fd, const uint8_t type_guid[]) {
  fbl::unique_fd old_part;
  while ((old_part.reset(open_partition(nullptr, type_guid, ZX_MSEC(500), nullptr))), old_part) {
    bool is_vpartition;
    if (FvmIsVirtualPartition(old_part, &is_vpartition) != ZX_OK) {
      ERROR("Couldn't confirm old vpartition type\n");
      return ZX_ERR_IO;
    }
    if (FvmPartitionIsChild(fvm_fd, old_part) != ZX_OK) {
      RecommendWipe("Streaming a partition type which also exists outside the target FVM");
      return ZX_ERR_BAD_STATE;
    }
    if (!is_vpartition) {
      RecommendWipe("Streaming a partition type which also exists in a GPT");
      return ZX_ERR_BAD_STATE;
    }

    // We're paving a partition that already exists within the FVM: let's
    // destroy it before we pave anew.

    fzl::UnownedFdioCaller partition_connection(old_part.get());
    auto result = volume::Volume::Call::Destroy(partition_connection.channel());
    zx_status_t status = result.ok() ? result.value().status : result.status();
    if (status != ZX_OK) {
      ERROR("Couldn't destroy partition: %s\n", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

// Calculate the amount of space necessary for the incoming partitions,
// validating the header along the way. Additionally, deletes any old partitions
// which match the type GUID of the provided partition.
//
// Parses the information from the |reader| into |parts|.
zx_status_t PreProcessPartitions(const fbl::unique_fd& fvm_fd,
                                 const fbl::unique_ptr<fvm::SparseReader>& reader,
                                 const fbl::Array<PartitionInfo>& parts,
                                 size_t* out_requested_slices) {
  fvm::partition_descriptor_t* part = reader->Partitions();
  fvm::sparse_image_t* hdr = reader->Image();

  // Validate the header and determine the necessary slice requirements for
  // all partitions and all offsets.
  size_t requested_slices = 0;
  for (size_t p = 0; p < hdr->partition_count; p++) {
    parts[p].pd = part;
    if (parts[p].pd->magic != fvm::kPartitionDescriptorMagic) {
      ERROR("Bad partition magic\n");
      return ZX_ERR_IO;
    }

    zx_status_t status = WipeAllFvmPartitionsWithGUID(fvm_fd, parts[p].pd->type);
    if (status != ZX_OK) {
      ERROR("Failure wiping old partitions matching this GUID\n");
      return status;
    }

    fvm::extent_descriptor_t* ext = GetExtent(parts[p].pd, 0);
    if (ext->magic != fvm::kExtentDescriptorMagic) {
      ERROR("Bad extent magic\n");
      return ZX_ERR_IO;
    }
    if (ext->slice_start != 0) {
      ERROR("First slice must start at zero\n");
      return ZX_ERR_IO;
    }
    if (ext->slice_count == 0) {
      ERROR("Extents must have > 0 slices\n");
      return ZX_ERR_IO;
    }
    if (ext->extent_length > ext->slice_count * hdr->slice_size) {
      ERROR("Extent length must fit within allocated slice count\n");
      return ZX_ERR_IO;
    }

    // Filter drivers may require additional space.
    if ((parts[p].pd->flags & fvm::kSparseFlagZxcrypt) != 0) {
      requested_slices += kZxcryptExtraSlices;
    }

    for (size_t e = 1; e < parts[p].pd->extent_count; e++) {
      ext = GetExtent(parts[p].pd, e);
      if (ext->magic != fvm::kExtentDescriptorMagic) {
        ERROR("Bad extent magic\n");
        return ZX_ERR_IO;
      } else if (ext->slice_count == 0) {
        ERROR("Extents must have > 0 slices\n");
        return ZX_ERR_IO;
      } else if (ext->extent_length > ext->slice_count * hdr->slice_size) {
        ERROR("Extent must fit within allocated slice count\n");
        return ZX_ERR_IO;
      }

      requested_slices += ext->slice_count;
    }
    part = reinterpret_cast<fvm::partition_descriptor*>(reinterpret_cast<uintptr_t>(ext) +
                                                        sizeof(fvm::extent_descriptor_t));
  }

  *out_requested_slices = requested_slices;
  return ZX_OK;
}

// Allocates the space requested by the partitions by creating new
// partitions and filling them with extents. This guarantees that
// streaming the data to the device will not run into "no space" issues
// later.
zx_status_t AllocatePartitions(const fbl::unique_fd& fvm_fd,
                               const fbl::Array<PartitionInfo>& parts) {
  for (size_t p = 0; p < parts.size(); p++) {
    fvm::extent_descriptor_t* ext = GetExtent(parts[p].pd, 0);
    alloc_req_t alloc;
    // Allocate this partition as inactive so it gets deleted on the next
    // reboot if this stream fails.
    alloc.flags = volume::AllocatePartitionFlagInactive;
    alloc.slice_count = ext->slice_count;
    memcpy(&alloc.type, parts[p].pd->type, sizeof(alloc.type));
    zx_cprng_draw(alloc.guid, GPT_GUID_LEN);
    memcpy(&alloc.name, parts[p].pd->name, sizeof(alloc.name));
    LOG("Allocating partition %s consisting of %zu slices\n", alloc.name, alloc.slice_count);
    parts[p].new_part.reset(fvm_allocate_partition(fvm_fd.get(), &alloc));
    if (!parts[p].new_part) {
      ERROR("Couldn't allocate partition\n");
      return ZX_ERR_NO_SPACE;
    }

    // Add filter drivers.
    if ((parts[p].pd->flags & fvm::kSparseFlagZxcrypt) != 0) {
      LOG("Creating zxcrypt volume\n");
      zx_status_t status = ZxcryptCreate(&parts[p]);
      if (status != ZX_OK) {
        return status;
      }
    }

    // The 0th index extent is allocated alongside the partition, so we
    // begin indexing from the 1st extent here.
    for (size_t e = 1; e < parts[p].pd->extent_count; e++) {
      ext = GetExtent(parts[p].pd, e);
      uint64_t offset = ext->slice_start;
      uint64_t length = ext->slice_count;

      fzl::UnownedFdioCaller partition_connection(parts[p].new_part.get());
      auto result = volume::Volume::Call::Extend(partition_connection.channel(), offset, length);
      auto status = result.ok() ? result.value().status : result.status();
      if (status != ZX_OK) {
        ERROR("Failed to extend partition: %s\n", zx_status_get_string(status));
        return status;
      }
    }
  }

  return ZX_OK;
}

}  // namespace

zx_status_t FvmStreamPartitions(fbl::unique_fd partition_fd,
                                std::unique_ptr<fvm::ReaderInterface> payload) {
  fbl::unique_ptr<fvm::SparseReader> reader;
  zx_status_t status;
  if ((status = fvm::SparseReader::Create(std::move(payload), &reader)) != ZX_OK) {
    return status;
  }

  LOG("Header Validated - OK\n");

  fbl::unique_fd devfs_root(open("/dev", O_RDWR));
  if (!devfs_root) {
    ERROR("Couldn't open devfs root\n");
    return ZX_ERR_IO;
  }

  fvm::sparse_image_t* hdr = reader->Image();
  // Acquire an fd to the FVM, either by finding one that already
  // exists, or formatting a new one.
  fbl::unique_fd fvm_fd(FvmPartitionFormat(devfs_root, partition_fd.duplicate(),
                                           hdr->slice_size, BindOption::TryBind));
  if (!fvm_fd) {
    ERROR("Couldn't find FVM partition\n");
    return ZX_ERR_IO;
  }

  fbl::Array<PartitionInfo> parts(new PartitionInfo[hdr->partition_count], hdr->partition_count);

  // Parse the incoming image and calculate its size.
  //
  // Additionally, delete the old versions of any new partitions.
  size_t requested_slices = 0;
  if ((status = PreProcessPartitions(fvm_fd, reader, parts, &requested_slices)) != ZX_OK) {
    ERROR("Failed to validate partitions: %s\n", zx_status_get_string(status));
    return status;
  }

  // Contend with issues from an image that may be too large for this device.
  VolumeInfo info;
  status =
      fvm_query(fvm_fd.get(), reinterpret_cast<fuchsia_hardware_block_volume_VolumeInfo*>(&info));
  if (status != ZX_OK) {
    ERROR("Failed to acquire FVM info: %s\n", zx_status_get_string(status));
    return status;
  }
  size_t free_slices = info.pslice_total_count - info.pslice_allocated_count;
  if (info.pslice_total_count < requested_slices) {
    char buf[256];
    snprintf(buf, sizeof(buf), "Image size (%zu) > Storage size (%zu)",
             requested_slices * hdr->slice_size, info.pslice_total_count * hdr->slice_size);
    Warn(buf, "Image is too large to be paved to device");
    return ZX_ERR_NO_SPACE;
  }
  if (free_slices < requested_slices) {
    Warn("Not enough space to non-destructively pave",
         "Automatically reinitializing FVM; Expect data loss");
    fvm_fd = FvmPartitionFormat(devfs_root, partition_fd.duplicate(), hdr->slice_size,
                                BindOption::Reformat);
    if (!fvm_fd) {
      ERROR("Couldn't reformat FVM partition.\n");
      return ZX_ERR_IO;
    }
    LOG("FVM Reformatted successfully.\n");
  }

  LOG("Partitions pre-validated successfully: Enough space exists to pave.\n");

  // Actually allocate the storage for the incoming image.
  if ((status = AllocatePartitions(fvm_fd, parts)) != ZX_OK) {
    ERROR("Failed to allocate partitions: %s\n", zx_status_get_string(status));
    return status;
  }

  LOG("Partition space pre-allocated successfully.\n");

  constexpr size_t vmo_size = 1 << 20;

  fzl::VmoMapper mapping;
  zx::vmo vmo;
  if ((status = mapping.CreateAndMap(vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
                                     &vmo)) != ZX_OK) {
    ERROR("Failed to create stream VMO\n");
    return ZX_ERR_NO_MEMORY;
  }

  fzl::FdioCaller volume_manager(std::move(fvm_fd));

  // Now that all partitions are preallocated, begin streaming data to them.
  for (size_t p = 0; p < parts.size(); p++) {
    vmoid_t vmoid;
    block_client::Client client;
    zx_status_t status = RegisterFastBlockIo(parts[p].new_part, vmo, &vmoid, &client);
    if (status != ZX_OK) {
      ERROR("Failed to register fast block IO\n");
      return status;
    }

    fzl::UnownedFdioCaller partition_connection(parts[p].new_part.get());
    auto result = block::Block::Call::GetInfo(partition_connection.channel());
    if (!result.ok()) {
      ERROR("Couldn't get partition block info: %s\n", zx_status_get_string(result.status()));
      return result.status();
    }
    const auto& response = result.value();
    if (response.status != ZX_OK) {
      ERROR("Couldn't get partition block info: %s\n", zx_status_get_string(response.status));
      return response.status;
    }

    size_t block_size = response.info->block_size;

    block_fifo_request_t request;
    request.group = 0;
    request.vmoid = vmoid;
    request.opcode = BLOCKIO_WRITE;

    LOG("Streaming partition %zu\n", p);
    status = StreamFvmPartition(reader.get(), &parts[p], mapping, client, block_size, &request);
    LOG("Done streaming partition %zu\n", p);
    if (status != ZX_OK) {
      ERROR("Failed to stream partition status=%d\n", status);
      return status;
    }
    if ((status = FlushClient(&client)) != ZX_OK) {
      ERROR("Failed to flush client\n");
      return status;
    }
    LOG("Done flushing partition %zu\n", p);
  }

  for (size_t p = 0; p < parts.size(); p++) {
    fzl::UnownedFdioCaller partition_connection(parts[p].new_part.get());
    // Upgrade the old partition (currently active) to the new partition (currently
    // inactive) so the new partition persists.
    auto result = partition::Partition::Call::GetInstanceGuid(partition_connection.channel());
    if (!result.ok() || result.value().status != ZX_OK) {
      ERROR("Failed to get unique GUID of new partition\n");
      return ZX_ERR_BAD_STATE;
    }
    auto* guid = result.value().guid;

    auto result2 = volume::VolumeManager::Call::Activate(volume_manager.channel(), *guid, *guid);
    if (result2.status() != ZX_OK || result2.value().status != ZX_OK) {
      ERROR("Failed to upgrade partition\n");
      return ZX_ERR_IO;
    }
  }

  return ZX_OK;
}

}  // namespace paver
