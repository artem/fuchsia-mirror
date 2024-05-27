// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/android.h"

#include <fidl/fuchsia.device.manager/cpp/common_types.h>
#include <lib/zx/result.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/system/public/zircon/errors.h>

#include <algorithm>
#include <iterator>
#include <string>

#include <fbl/algorithm.h>
#include <src/lib/fidl/cpp/include/lib/fidl/cpp/channel.h>
#include <src/lib/uuid/uuid.h>

#include "src/storage/lib/paver/boot_control_definition.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/libboot_control.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/system_shutdown_state.h"
#include "src/storage/lib/paver/utils.h"
#include "src/storage/lib/paver/validation.h"

namespace paver {

namespace {

using fuchsia_device_manager::SystemPowerState;
using uuid::Uuid;

}  // namespace

zx::result<std::unique_ptr<DevicePartitioner>> AndroidDevicePartitioner::Initialize(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
    fidl::ClientEnd<fuchsia_device::Controller> block_device, std::shared_ptr<Context> context) {
  if (arch != Arch::kX64 && arch != Arch::kArm64) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto status =
      GptDevicePartitioner::InitializeGpt(std::move(devfs_root), svc_root, std::move(block_device));
  if (status.is_error()) {
    return status.take_error();
  }

  auto partitioner =
      WrapUnique(new AndroidDevicePartitioner(arch, std::move(status->gpt), std::move(context)));
  if (status->initialize_partition_tables) {
    if (auto status = partitioner->InitPartitionTables(); status.is_error()) {
      return status.take_error();
    }
  }

  LOG("Successfully initialized Android Device Partitioner\n");
  return zx::ok(std::move(partitioner));
}

bool AndroidDevicePartitioner::SupportsPartition(const PartitionSpec& spec) const {
  constexpr PartitionSpec supported_specs[] = {
      PartitionSpec(paver::Partition::kBootloaderA, "boot_shim"),
      PartitionSpec(paver::Partition::kBootloaderB, "boot_shim"),
      PartitionSpec(paver::Partition::kZirconA),
      PartitionSpec(paver::Partition::kZirconB),
      PartitionSpec(paver::Partition::kVbMetaA),
      PartitionSpec(paver::Partition::kVbMetaB),
      PartitionSpec(paver::Partition::kAbrMeta),
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager)};
  return std::any_of(std::cbegin(supported_specs), std::cend(supported_specs),
                     [&](const PartitionSpec& supported) { return SpecMatches(spec, supported); });
}

zx::result<std::unique_ptr<PartitionClient>> AndroidDevicePartitioner::AddPartition(
    const PartitionSpec& spec) const {
  ERROR("Adding partitions is not supported for Android devices\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<std::unique_ptr<PartitionClient>> AndroidDevicePartitioner::FindPartition(
    const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // Boot shim is passed as kBootloaderX. Meaning we have following mappings:
  // 	boot-shim -> boot
  // 	zircon    -> vendor_boot
  // 	vbmeta    -> vbmeta
  // 	abrmeta   -> misc
  // 	FVM       -> super
  std::string_view part_name;
  switch (spec.partition) {
    case Partition::kBootloaderA:
      part_name = "boot_a";
      break;
    case Partition::kBootloaderB:
      part_name = "boot_b";
      break;
    case Partition::kZirconA:
      part_name = "vendor_boot_a";
      break;
    case Partition::kZirconB:
      part_name = "vendor_boot_b";
      break;
    case Partition::kVbMetaA:
      part_name = "vbmeta_a";
      break;
    case Partition::kVbMetaB:
      part_name = "vbmeta_b";
      break;
    case Partition::kAbrMeta:
      part_name = "misc";
      break;
    case Partition::kFuchsiaVolumeManager:
      part_name = "super";
      break;
    default:
      ERROR("Android partitioner cannot find unknown partition type\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  const auto filter = [&](const gpt_partition_t& part) { return FilterByName(part, part_name); };
  auto status = gpt_->FindPartition(filter);
  if (status.is_error()) {
    return status.take_error();
  }
  return zx::ok(std::move(status->partition));
}

zx::result<> AndroidDevicePartitioner::FinalizePartition(const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::make_result(gpt_->GetGpt()->Sync());
}

zx::result<> AndroidDevicePartitioner::WipeFvm() const { return gpt_->WipeFvm(); }

zx::result<> AndroidDevicePartitioner::InitPartitionTables() const {
  ERROR("Initialising partition tables is not supported for Android devices\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> AndroidDevicePartitioner::WipePartitionTables() const {
  ERROR("Wiping partition tables is not supported for Android devices\n");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> AndroidDevicePartitioner::ValidatePayload(const PartitionSpec& spec,
                                                       cpp20::span<const uint8_t> data) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (IsZirconPartitionSpec(spec)) {
    if (!IsValidAndroidKernel(arch_, data)) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  return zx::ok();
}

zx::result<> AndroidDevicePartitioner::OnStop() const {
  const auto state = GetShutdownSystemState(gpt_->svc_root());
  switch (state) {
    case SystemPowerState::kRebootBootloader:
      ERROR("Setting one shot reboot to bootloader flag is not implemented.\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    case SystemPowerState::kRebootRecovery:
      ERROR("Setting one shot reboot to recovery flag is not implemented.\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    case SystemPowerState::kFullyOn:
    case SystemPowerState::kReboot:
    case SystemPowerState::kPoweroff:
    case SystemPowerState::kMexec:
    case SystemPowerState::kSuspendRam:
    case SystemPowerState::kRebootKernelInitiated:
      // nothing to do for these cases
      break;
  }

  return zx::ok();
}

zx::result<std::unique_ptr<DevicePartitioner>> AndroidPartitionerFactory::New(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
    std::shared_ptr<Context> context, fidl::ClientEnd<fuchsia_device::Controller> block_device) {
  return AndroidDevicePartitioner::Initialize(std::move(devfs_root), svc_root, arch,
                                              std::move(block_device), std::move(context));
}

class AndroidAbrClient : public abr::Client, android::IoOps {
 public:
  // IoOps implementation for BootControl
  int read(void* data, uint64_t offset, size_t len) const override {
    return vmo_.read(data, offset, len);
  }
  int write(const void* data, uint64_t offset, size_t len) const override {
    return vmo_.write(data, offset, len);
  }

  static zx::result<std::unique_ptr<abr::Client>> Create(
      std::unique_ptr<paver::PartitionClient> partition) {
    auto status = partition->GetBlockSize();
    if (status.is_error()) {
      ERROR("Unabled to get block size\n");
      return status.take_error();
    }
    // Make sure block contains BootloaderMessageAB structure.
    size_t block_size = std::max(status.value(), sizeof(android::BootloaderMessageAB));

    zx::vmo vmo;
    if (auto status = zx::make_result(zx::vmo::create(block_size, 0, &vmo)); status.is_error()) {
      ERROR("Failed to create vmo\n");
      return status.take_error();
    }

    if (auto status = partition->Read(vmo, block_size); status.is_error()) {
      ERROR("Failed to read from partition\n");
      return status.take_error();
    }

    return zx::ok(new AndroidAbrClient(std::move(partition), std::move(vmo), block_size));
  }

 private:
  AndroidAbrClient(std::unique_ptr<paver::PartitionClient> partition, zx::vmo vmo,
                   size_t block_size)
      : Client(/*custom = */ true),
        partition_(std::move(partition)),
        vmo_(std::move(vmo)),
        data_size_(block_size),
        boot_control_(*this) {}

  zx::result<> Read(uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<> Write(const uint8_t* buffer, size_t size) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  android::SlotMetadata ToAndroid(const AbrSlotData& src) {
    android::SlotMetadata slot_metadata;
    // Max values for src matches bit length for `slot_metadata`
    slot_metadata.priority = src.priority;
    slot_metadata.tries_remaining = src.tries_remaining;
    slot_metadata.successful_boot = src.successful_boot;
    slot_metadata.verity_corrupted = 0;
    return slot_metadata;
  }

  AbrSlotData ToFuchsia(const android::SlotMetadata& slot_metadata) {
    AbrSlotData abr_slot_data;
    // Max values for src matches bit length for `slot_metadata`
    abr_slot_data.priority = slot_metadata.priority;
    abr_slot_data.tries_remaining = slot_metadata.tries_remaining;
    abr_slot_data.successful_boot = slot_metadata.successful_boot;
    return abr_slot_data;
  }

  zx::result<> ReadCustom(AbrSlotData* a, AbrSlotData* b, uint8_t* one_shot_recovery) override {
    android::BootloaderControl bootloader_control;
    boot_control_.Load(&bootloader_control);

    if (bootloader_control.nb_slot != 2) {
      ERROR("Only 2-slot systems are supported");
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }

    *a = ToFuchsia(bootloader_control.slot_info[0]);
    *b = ToFuchsia(bootloader_control.slot_info[1]);

    // oneshot recovery is not supported via Android ABR
    *one_shot_recovery = 0;

    return zx::ok();
  }

  zx::result<> WriteCustom(const AbrSlotData* a, const AbrSlotData* b,
                           uint8_t one_shot_recovery) override {
    android::BootloaderControl bootloader_control;
    if (!boot_control_.Load(&bootloader_control)) {
      ERROR("Failed to read Bootloader Control data\n");
      return zx::error(ZX_ERR_IO);
    }

    bootloader_control.nb_slot = 2;
    bootloader_control.slot_info[0] = ToAndroid(*a);
    bootloader_control.slot_info[1] = ToAndroid(*b);
    // Oneshot recovery is not supported via Andoird ABR
    (void)one_shot_recovery;

    if (!boot_control_.UpdateAndSave(&bootloader_control)) {
      ERROR("Failed to write Bootloader Control data\n");
      return zx::error(ZX_ERR_IO);
    }

    return zx::ok();
  }

  zx::result<> Flush() const override {
    if (auto status = partition_->Write(vmo_, data_size_); status.is_error()) {
      ERROR("Failed to read from partition\n");
      return status.take_error();
    }
    return partition_->Flush();
  }

  std::unique_ptr<paver::PartitionClient> partition_;
  zx::vmo vmo_;
  size_t data_size_;
  android::BootControl boot_control_;
};

zx::result<std::unique_ptr<abr::Client>> AndroidAbrClientFactory::New(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    std::shared_ptr<paver::Context> context) {
  auto partitioner = AndroidDevicePartitioner::Initialize(
      std::move(devfs_root), std::move(svc_root), GetCurrentArch(), {}, std::move(context));

  if (partitioner.is_error()) {
    return partitioner.take_error();
  }

  // Assume ABR Metadata partition indicate it is android partition layout.
  auto partition = partitioner->FindPartition(paver::PartitionSpec(paver::Partition::kAbrMeta));
  if (partition.is_error()) {
    ERROR("Failed to find abr partition\n");
    return partition.take_error();
  }

  return AndroidAbrClient::Create(std::move(partition.value()));
}

}  // namespace paver
