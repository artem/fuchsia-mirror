// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/x64.h"

#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/system/public/zircon/errors.h>

#include <algorithm>
#include <iterator>

#include <src/lib/fidl/cpp/include/lib/fidl/cpp/channel.h>

#include "fidl/fuchsia.device.manager/cpp/common_types.h"
#include "src/lib/uuid/uuid.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/system_shutdown_state.h"
#include "src/storage/lib/paver/utils.h"
#include "src/storage/lib/paver/validation.h"

namespace paver {

namespace {

using fuchsia_device_manager::SystemPowerState;
using uuid::Uuid;

constexpr size_t kKibibyte = 1024;
constexpr size_t kMebibyte = kKibibyte * 1024;
constexpr size_t kGibibyte = kMebibyte * 1024;

// All X64 boards currently use the legacy partition scheme.
constexpr PartitionScheme kPartitionScheme = PartitionScheme::kLegacy;

// TODO: Remove support after July 9th 2021.
constexpr char kOldEfiName[] = "efi-system";

}  // namespace

zx::result<std::unique_ptr<DevicePartitioner>> EfiDevicePartitioner::Initialize(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
    fidl::ClientEnd<fuchsia_device::Controller> block_device, std::shared_ptr<Context> context) {
  if (arch != Arch::kX64) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto status =
      GptDevicePartitioner::InitializeGpt(std::move(devfs_root), svc_root, std::move(block_device));
  if (status.is_error()) {
    return status.take_error();
  }

  auto partitioner =
      WrapUnique(new EfiDevicePartitioner(arch, std::move(status->gpt), std::move(context)));
  if (status->initialize_partition_tables) {
    if (auto status = partitioner->InitPartitionTables(); status.is_error()) {
      return status.take_error();
    }
  }

  LOG("Successfully initialized EFI Device Partitioner\n");
  return zx::ok(std::move(partitioner));
}

bool EfiDevicePartitioner::SupportsPartition(const PartitionSpec& spec) const {
  const PartitionSpec supported_specs[] = {
      PartitionSpec(paver::Partition::kBootloaderA),
      PartitionSpec(paver::Partition::kZirconA),
      PartitionSpec(paver::Partition::kZirconB),
      PartitionSpec(paver::Partition::kZirconR),
      PartitionSpec(paver::Partition::kVbMetaA),
      PartitionSpec(paver::Partition::kVbMetaB),
      PartitionSpec(paver::Partition::kVbMetaR),
      PartitionSpec(paver::Partition::kAbrMeta),
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager),
      PartitionSpec(paver::Partition::kFuchsiaVolumeManager, kOpaqueVolumeContentType),
  };
  return std::any_of(std::cbegin(supported_specs), std::cend(supported_specs),
                     [&](const PartitionSpec& supported) { return SpecMatches(spec, supported); });
}

zx::result<std::unique_ptr<PartitionClient>> EfiDevicePartitioner::AddPartition(
    const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // NOTE: If you update the minimum sizes of partitions, please update the
  // EfiDevicePartitionerTests.InitPartitionTables test.
  size_t minimum_size_bytes = 0;
  switch (spec.partition) {
    case Partition::kBootloaderA:
      minimum_size_bytes = 16 * kMebibyte;
      break;
    case Partition::kZirconA:
      minimum_size_bytes = 128 * kMebibyte;
      break;
    case Partition::kZirconB:
      minimum_size_bytes = 128 * kMebibyte;
      break;
    case Partition::kZirconR:
      minimum_size_bytes = 192 * kMebibyte;
      break;
    case Partition::kVbMetaA:
      minimum_size_bytes = 64 * kKibibyte;
      break;
    case Partition::kVbMetaB:
      minimum_size_bytes = 64 * kKibibyte;
      break;
    case Partition::kVbMetaR:
      minimum_size_bytes = 64 * kKibibyte;
      break;
    case Partition::kAbrMeta:
      minimum_size_bytes = 4 * kKibibyte;
      break;
    case Partition::kFuchsiaVolumeManager:
      minimum_size_bytes = 56 * kGibibyte;
      break;
    default:
      ERROR("EFI partitioner cannot add unknown partition type\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  const char* name = PartitionName(spec.partition, kPartitionScheme);
  auto type = GptPartitionType(spec.partition);
  if (type.is_error()) {
    return type.take_error();
  }
  return gpt_->AddPartition(name, type.value(), minimum_size_bytes, /*optional_reserve_bytes*/ 0);
}

zx::result<std::unique_ptr<PartitionClient>> EfiDevicePartitioner::FindPartition(
    const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  switch (spec.partition) {
    case Partition::kBootloaderA: {
      const auto filter = [](const gpt_partition_t& part) {
        return FilterByTypeAndName(part, GUID_EFI_VALUE, GUID_EFI_NAME) ||
               // TODO: Remove support after July 9th 2021.
               FilterByTypeAndName(part, GUID_EFI_VALUE, kOldEfiName);
      };
      auto status = gpt_->FindPartition(filter);
      if (status.is_error()) {
        return status.take_error();
      }
      return zx::ok(std::move(status->partition));
    }
    case Partition::kZirconA:
    case Partition::kZirconB:
    case Partition::kZirconR:
    case Partition::kVbMetaA:
    case Partition::kVbMetaB:
    case Partition::kVbMetaR:
    case Partition::kAbrMeta: {
      const auto filter = [&spec](const gpt_partition_t& part) {
        auto status = GptPartitionType(spec.partition);
        return status.is_ok() && FilterByType(part, status.value());
      };
      auto status = gpt_->FindPartition(filter);
      if (status.is_error()) {
        return status.take_error();
      }
      return zx::ok(std::move(status->partition));
    }
    case Partition::kFuchsiaVolumeManager: {
      auto status = gpt_->FindPartition(IsFvmPartition);
      if (status.is_error()) {
        return status.take_error();
      }
      return zx::ok(std::move(status->partition));
    }
    default:
      ERROR("EFI partitioner cannot find unknown partition type\n");
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
}

zx::result<> EfiDevicePartitioner::FinalizePartition(const PartitionSpec& spec) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::make_result(gpt_->GetGpt()->Sync());
}

zx::result<> EfiDevicePartitioner::WipeFvm() const { return gpt_->WipeFvm(); }

zx::result<> EfiDevicePartitioner::InitPartitionTables() const {
  const std::array<Partition, 9> partitions_to_add{
      Partition::kBootloaderA, Partition::kZirconA, Partition::kZirconB,
      Partition::kZirconR,     Partition::kVbMetaA, Partition::kVbMetaB,
      Partition::kVbMetaR,     Partition::kAbrMeta, Partition::kFuchsiaVolumeManager,
  };

  // Wipe partitions.
  // EfiDevicePartitioner operates on partition types.
  auto status = gpt_->WipePartitions([&partitions_to_add](const gpt_partition_t& part) {
    for (auto& partition : partitions_to_add) {
      // Get the partition type GUID, and compare it.
      auto status = GptPartitionType(partition);
      if (status.is_error() || status.value() != Uuid(part.type)) {
        continue;
      }
      // If we are wiping any non-bootloader partition, we are done.
      if (partition != Partition::kBootloaderA) {
        return true;
      }
      // If we are wiping the bootloader partition, only do so if it is the
      // Fuchsia-installed bootloader partition. This is to allow dual-booting.
      char cstring_name[GPT_NAME_LEN] = {};
      utf16_to_cstring(cstring_name, part.name, GPT_NAME_LEN);
      if (strncasecmp(cstring_name, GUID_EFI_NAME, GPT_NAME_LEN) == 0) {
        return true;
      }
      // Support the old name.
      // TODO: Remove support after July 9th 2021.
      if (strncasecmp(cstring_name, kOldEfiName, GPT_NAME_LEN) == 0) {
        return true;
      }
    }
    return false;
  });
  if (status.is_error()) {
    ERROR("Failed to wipe partitions: %s\n", status.status_string());
    return status.take_error();
  }

  // Add partitions with default content_type.
  for (auto type : partitions_to_add) {
    auto status = AddPartition(PartitionSpec(type));
    if (status.status_value() == ZX_ERR_ALREADY_BOUND) {
      ERROR("Warning: Skipping existing partition \"%s\"\n", PartitionName(type, kPartitionScheme));
    } else if (status.is_error()) {
      ERROR("Failed to create partition \"%s\": %s\n", PartitionName(type, kPartitionScheme),
            status.status_string());
      return status.take_error();
    }
  }

  LOG("Successfully initialized GPT\n");
  return zx::ok();
}  // namespace paver

zx::result<> EfiDevicePartitioner::WipePartitionTables() const {
  return gpt_->WipePartitionTables();
}

zx::result<> EfiDevicePartitioner::ValidatePayload(const PartitionSpec& spec,
                                                   cpp20::span<const uint8_t> data) const {
  if (!SupportsPartition(spec)) {
    ERROR("Unsupported partition %s\n", spec.ToString().c_str());
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (IsZirconPartitionSpec(spec)) {
    if (!IsValidKernelZbi(arch_, data)) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  return zx::ok();
}

// Helper function to access `abr_client`.
// Used to set one-shot flags to reboot to recovery/bootloader.
zx::result<> EfiDevicePartitioner::CallAbr(
    std::function<zx::result<>(abr::Client&)> call_abr) const {
  auto partition = FindPartition(paver::PartitionSpec(paver::Partition::kAbrMeta));
  if (partition.is_error()) {
    ERROR("Failed to find A/B/R metadata partition: %s\n", partition.status_string());
    return partition.take_error();
  }

  auto abr_partition_client = abr::AbrPartitionClient::Create(std::move(partition.value()));
  if (abr_partition_client.is_error()) {
    ERROR("Failed to create A/B/R metadata partition client: %s\n",
          abr_partition_client.status_string());
    return abr_partition_client.take_error();
  }
  auto& abr_client = abr_partition_client.value();

  return call_abr(*abr_client);
}

zx::result<> EfiDevicePartitioner::OnStop() const {
  const auto state = GetShutdownSystemState(gpt_->svc_root());
  switch (state) {
    case SystemPowerState::kRebootBootloader:
      LOG("Setting one shot reboot to bootloader flag.\n");
      return CallAbr([](abr::Client& abr_client) { return abr_client.SetOneShotBootloader(); });
    case SystemPowerState::kRebootRecovery:
      LOG("Setting one shot reboot to recovery flag\n");
      return CallAbr([](abr::Client& abr_client) { return abr_client.SetOneShotRecovery(); });
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

zx::result<std::unique_ptr<DevicePartitioner>> X64PartitionerFactory::New(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root, Arch arch,
    std::shared_ptr<Context> context, fidl::ClientEnd<fuchsia_device::Controller> block_device) {
  return EfiDevicePartitioner::Initialize(std::move(devfs_root), svc_root, arch,
                                          std::move(block_device), std::move(context));
}

zx::result<std::unique_ptr<abr::Client>> X64AbrClientFactory::New(
    fbl::unique_fd devfs_root, fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root,
    std::shared_ptr<paver::Context> context) {
  auto partitioner = EfiDevicePartitioner::Initialize(std::move(devfs_root), std::move(svc_root),
                                                      GetCurrentArch(), {}, std::move(context));

  if (partitioner.is_error()) {
    return partitioner.take_error();
  }

  // ABR metadata has no need of a content type since it's always local rather
  // than provided in an update package, so just use the default content type.
  auto partition = partitioner->FindPartition(paver::PartitionSpec(paver::Partition::kAbrMeta));
  if (partition.is_error()) {
    ERROR("Failed to find abr partition\n");
    return partition.take_error();
  }

  return abr::AbrPartitionClient::Create(std::move(partition.value()));
}

}  // namespace paver
