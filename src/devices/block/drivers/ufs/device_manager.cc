// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/ufs/device_manager.h"

#include <lib/fit/defer.h>
#include <lib/trace/event.h>
#include <lib/zx/clock.h>

#include "src/devices/block/drivers/ufs/registers.h"
#include "src/devices/block/drivers/ufs/transfer_request_processor.h"
#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "transfer_request_processor.h"

namespace ufs {
zx::result<std::unique_ptr<DeviceManager>> DeviceManager::Create(
    Ufs &controller, TransferRequestProcessor &transfer_request_processor) {
  fbl::AllocChecker ac;
  auto device_manager =
      fbl::make_unique_checked<DeviceManager>(&ac, controller, transfer_request_processor);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate device manager.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(device_manager));
}

zx::result<> DeviceManager::SendLinkStartUp() {
  DmeLinkStartUpUicCommand link_startup_command(controller_);
  if (zx::result<std::optional<uint32_t>> result = link_startup_command.SendCommand();
      result.is_error()) {
    zxlogf(ERROR, "Failed to startup UFS link: %s", result.status_string());
    return result.take_error();
  }
  return zx::ok();
}

zx::result<> DeviceManager::DeviceInit() {
  zx::time device_init_start_time = zx::clock::get_monotonic();
  SetFlagUpiu set_flag_upiu(Flags::fDeviceInit);
  auto query_response = req_processor_.SendQueryRequestUpiu(set_flag_upiu);
  if (query_response.is_error()) {
    return query_response.take_error();
  }

  zx::time device_init_time_out = device_init_start_time + zx::usec(kDeviceInitTimeoutUs);
  while (true) {
    ReadFlagUpiu read_flag_upiu(Flags::fDeviceInit);
    auto response = req_processor_.SendQueryRequestUpiu(read_flag_upiu);
    if (response.is_error()) {
      return response.take_error();
    }
    uint8_t flag = response->GetResponse<FlagResponseUpiu>().GetFlag();

    if (!flag)
      break;

    if (zx::clock::get_monotonic() > device_init_time_out) {
      zxlogf(ERROR, "Wait for fDeviceInit timed out");
      return zx::error(ZX_ERR_TIMED_OUT);
    }
    usleep(10000);
  }
  return zx::ok();
}

zx::result<> DeviceManager::GetControllerDescriptor() {
  ReadDescriptorUpiu read_device_desc_upiu(DescriptorType::kDevice);
  auto response = req_processor_.SendQueryRequestUpiu(read_device_desc_upiu);
  if (response.is_error()) {
    return response.take_error();
  }
  device_descriptor_ =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<DeviceDescriptor>();

  // The field definitions for VersionReg and wSpecVersion are the same.
  // wSpecVersion use big-endian byte ordering.
  auto version = VersionReg::Get().FromValue(betoh16(device_descriptor_.wSpecVersion));
  zxlogf(INFO, "UFS device version %u.%u%u", version.major_version_number(),
         version.minor_version_number(), version.version_suffix());

  zxlogf(INFO, "%u enabled LUNs found", device_descriptor_.bNumberLU);

  ReadDescriptorUpiu read_geometry_desc_upiu(DescriptorType::kGeometry);
  response = req_processor_.SendQueryRequestUpiu(read_geometry_desc_upiu);
  if (response.is_error()) {
    return response.take_error();
  }
  geometry_descriptor_ =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<GeometryDescriptor>();

  // TODO(https://fxbug.dev/42075643): We need to functionalize the code to get max_lun_count_.
  if (geometry_descriptor_.bMaxNumberLU == 0) {
    max_lun_count_ = 8;
  } else if (geometry_descriptor_.bMaxNumberLU == 1) {
    max_lun_count_ = 32;
  } else {
    zxlogf(ERROR, "Invalid Geometry Descriptor bMaxNumberLU value=%d",
           geometry_descriptor_.bMaxNumberLU);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // The kDeviceDensityUnit is defined in the spec as 512.
  // qTotalRawDeviceCapacity use big-endian byte ordering.
  constexpr uint32_t kDeviceDensityUnit = 512;
  zxlogf(INFO, "UFS device total size is %lu bytes",
         betoh64(geometry_descriptor_.qTotalRawDeviceCapacity) * kDeviceDensityUnit);

  return zx::ok();
}

zx::result<uint32_t> DeviceManager::ReadAttribute(Attributes attribute, uint8_t index) {
  ReadAttributeUpiu read_attribute_upiu(attribute, index);
  auto query_response = req_processor_.SendQueryRequestUpiu(read_attribute_upiu);
  if (query_response.is_error()) {
    return query_response.take_error();
  }
  return zx::ok(query_response->GetResponse<AttributeResponseUpiu>().GetAttribute());
}

zx::result<> DeviceManager::WriteAttribute(Attributes attribute, uint32_t value, uint8_t index) {
  WriteAttributeUpiu write_attribute_upiu(attribute, value, index);
  auto query_response = req_processor_.SendQueryRequestUpiu(write_attribute_upiu);
  if (query_response.is_error()) {
    return query_response.take_error();
  }
  return zx::ok();
}

zx::result<uint32_t> DeviceManager::DmeGet(uint16_t mbi_attribute) {
  DmeGetUicCommand dme_get_command(controller_, mbi_attribute, 0);
  auto value = dme_get_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  if (!value.value().has_value()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  return zx::ok(value.value().value());
}

zx::result<uint32_t> DeviceManager::DmePeerGet(uint16_t mbi_attribute) {
  DmePeerGetUicCommand dme_peer_get_command(controller_, mbi_attribute, 0);
  auto value = dme_peer_get_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  if (!value.value().has_value()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  return zx::ok(value.value().value());
}

zx::result<> DeviceManager::DmeSet(uint16_t mbi_attribute, uint32_t value) {
  DmeSetUicCommand dme_set_command(controller_, mbi_attribute, 0, value);
  if (auto result = dme_set_command.SendCommand(); result.is_error()) {
    return result.take_error();
  }
  return zx::ok();
}

zx::result<uint32_t> DeviceManager::GetBootLunEnabled() {
  // Read bBootLunEn to confirm device interface is ok.
  zx::result<uint32_t> boot_lun_enabled = ReadAttribute(Attributes::bBootLunEn);
  if (boot_lun_enabled.is_error()) {
    return boot_lun_enabled.take_error();
  }
  zxlogf(DEBUG, "bBootLunEn 0x%0x", boot_lun_enabled.value());
  return zx::ok(boot_lun_enabled.value());
}

zx::result<UnitDescriptor> DeviceManager::ReadUnitDescriptor(uint8_t lun) {
  ReadDescriptorUpiu read_unit_desc_upiu(DescriptorType::kUnit, lun);
  auto response = req_processor_.SendQueryRequestUpiu(read_unit_desc_upiu);
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok(response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<UnitDescriptor>());
}

zx::result<> DeviceManager::ConfigureWriteBooster(inspect::Node &wb_node) {
  // Copy to access the unaligned value.
  const ExtendedUfsFeaturesSupport kExtendedUfsFeaturesSupport{
      betoh32(device_descriptor_.dExtendedUfsFeaturesSupport)};

  if (!kExtendedUfsFeaturesSupport.writebooster_support()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (zx::result<> result = EnableWriteBooster(wb_node); result.is_error()) {
    zxlogf(ERROR, "Failed to enable WriteBooster: %s", result.status_string());
    return result.take_error();
  }

  auto disable_write_booster = fit::defer([&] {
    if (is_write_booster_enabled_) {
      if (zx::result<> result = DisableWriteBooster(); result.is_error()) {
        zxlogf(ERROR, "Failed to disable WriteBooster: %s", result.status_string());
      } else {
        zxlogf(WARNING, "WriteBooster is disabled");
      }
    }
  });

  // Get WriteBooster buffer parameters.
  write_booster_buffer_type_ =
      static_cast<WriteBoosterBufferType>(device_descriptor_.bWriteBoosterBufferType);
  wb_node.RecordUint("write_booster_buffer_type", static_cast<uint8_t>(write_booster_buffer_type_));

  user_space_configuration_option_ = static_cast<UserSpaceConfigurationOption>(
      device_descriptor_.bWriteBoosterBufferPreserveUserSpaceEn);
  wb_node.RecordUint("user_space_configuration_option",
                     static_cast<uint8_t>(user_space_configuration_option_));

  // Find the size of the write buffer.
  uint32_t alloc_units = 0;
  if (write_booster_buffer_type_ == WriteBoosterBufferType::kSharedBuffer) {
    alloc_units = betoh32(device_descriptor_.dNumSharedWriteBoosterBufferAllocUnits);
  } else if (write_booster_buffer_type_ == WriteBoosterBufferType::kLuDedicatedBuffer) {
    for (uint8_t lun = 0; lun < max_lun_count_; ++lun) {
      ReadDescriptorUpiu read_unit_desc_upiu(DescriptorType::kUnit, lun);
      auto response = req_processor_.SendQueryRequestUpiu(read_unit_desc_upiu);
      if (response.is_error()) {
        continue;
      }
      auto unit_desc =
          response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<UnitDescriptor>();
      alloc_units = betoh32(unit_desc.dLUNumWriteBoosterBufferAllocUnits);
      if (alloc_units > 0) {
        // Found a dedicated buffer from LU.
        write_booster_dedicated_lu_ = lun;
        wb_node.RecordUint("write_booster_dedicated_lu", write_booster_dedicated_lu_);
        break;
      }
    }
  } else {
    zxlogf(WARNING, "Not supported WriteBooster buffer type: 0x%x",
           static_cast<uint8_t>(write_booster_buffer_type_));
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (alloc_units == 0) {
    // Unable to enable WriteBooster due to lack of resources.
    zxlogf(WARNING, "The WriteBooster buffer size is zero.");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  uint32_t write_booster_buffer_size_in_bytes =
      alloc_units * static_cast<uint32_t>(geometry_descriptor_.bAllocationUnitSize) *
      (betoh32(geometry_descriptor_.dSegmentSize)) * kSectorSize;
  wb_node.RecordUint("write_booster_buffer_size_in_bytes", write_booster_buffer_size_in_bytes);

  zx::result<bool> result = IsWriteBoosterBufferLifeTimeLeft();
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to IsWriteBoosterBufferLifeTimeLeft(): %s", result.status_string());
    return result.take_error();
  }
  if (!result.value()) {
    // Unable to enable WriteBooster due to lack of resources.
    zxlogf(WARNING, "Exceeded its maximum estimated WriteBooster Buffer life time");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // TODO(https://fxbug.dev/42075643): Need to handle WRITEBOOSTER_FLUSH_NEEDED exception case.

  disable_write_booster.cancel();
  zxlogf(INFO, "WriteBooster is enabled");
  return zx::ok();
}

zx::result<bool> DeviceManager::IsWriteBoosterBufferLifeTimeLeft() {
  uint8_t buffer_lun = write_booster_buffer_type_ == WriteBoosterBufferType::kLuDedicatedBuffer
                           ? write_booster_dedicated_lu_
                           : 0;
  zx::result<uint32_t> life_time = ReadAttribute(Attributes::bWBBufferLifeTimeEst, buffer_lun);
  if (life_time.is_error()) {
    return life_time.take_error();
  }
  if (life_time.value()) {
    if (life_time == kExceededWriteBoosterBufferLifeTime) {
      return zx::ok(false);
    }
  }

  return zx::ok(true);
}

zx::result<> DeviceManager::EnableWriteBooster(inspect::Node &wb_node) {
  // Enable WriteBooster.
  SetFlagUpiu set_wb_upiu(Flags::fWriteBoosterEn);
  if (auto response = req_processor_.SendQueryRequestUpiu(set_wb_upiu); response.is_error()) {
    return response.take_error();
  }
  is_write_booster_enabled_ = true;
  wb_node.RecordBool("is_write_booster_enabled", is_write_booster_enabled_);

  // Enable WriteBooster buffer flush during hibernate.
  SetFlagUpiu set_wb_flush_hibern8_upiu(Flags::fWBBufferFlushDuringHibernate);
  if (auto response = req_processor_.SendQueryRequestUpiu(set_wb_flush_hibern8_upiu);
      response.is_error()) {
    return response.take_error();
  }
  wb_node.RecordBool("writebooster_buffer_flush_during_hibernate", true);

  // Enable WriteBooster buffer flush.
  // TODO(https://fxbug.dev/42075643): For Samsung Exynos, ignore this flush behaviour due to the
  // quirk of not supporting manual flush.
  SetFlagUpiu set_wb_flush_upiu(Flags::fWBBufferFlushEn);
  if (auto response = req_processor_.SendQueryRequestUpiu(set_wb_flush_upiu); response.is_error()) {
    return response.take_error();
  }
  is_write_booster_flush_enabled_ = true;
  wb_node.RecordBool("writebooster_buffer_flush_enabled", is_write_booster_flush_enabled_);

  return zx::ok();
}

zx::result<> DeviceManager::DisableWriteBooster() {
  if (is_write_booster_flush_enabled_) {
    // Disable WriteBooster buffer flush.
    ClearFlagUpiu clear_wb_flush_upiu(Flags::fWBBufferFlushEn);
    if (auto response = req_processor_.SendQueryRequestUpiu(clear_wb_flush_upiu);
        response.is_error()) {
      return response.take_error();
    }
    is_write_booster_flush_enabled_ = false;
    // TODO(https://fxbug.dev/42075643): The inspect value of |writebooster_buffer_flush_enabled|
    // should be set to false.
  }

  // Disable WriteBooster buffer flush during hibernate.
  ClearFlagUpiu clear_wb_flush_hibern8_upiu(Flags::fWBBufferFlushDuringHibernate);
  if (auto response = req_processor_.SendQueryRequestUpiu(clear_wb_flush_hibern8_upiu);
      response.is_error()) {
    return response.take_error();
  }
  // TODO(https://fxbug.dev/42075643): The inspect value of
  // |writebooster_buffer_flush_during_hibernate| should be set to false.

  // Disable WriteBooster.
  ClearFlagUpiu clear_wb_upiu(Flags::fWriteBoosterEn);
  if (auto response = req_processor_.SendQueryRequestUpiu(clear_wb_upiu); response.is_error()) {
    return response.take_error();
  }
  is_write_booster_enabled_ = false;
  // TODO(https://fxbug.dev/42075643): The inspect value of |is_write_booster_enabled| should be set
  // to false.

  return zx::ok();
}

zx::result<bool> DeviceManager::NeedWriteBoosterBufferFlush() {
  if (!is_write_booster_enabled_) {
    return zx::ok(false);
  }

  auto result = IsWriteBoosterBufferLifeTimeLeft();
  if (result.is_error()) {
    return result.take_error();
  }
  if (!result.value()) {
    if (auto result = DisableWriteBooster(); result.is_error()) {
      zxlogf(ERROR, "Failed to disable WriteBooster: %s", result.status_string());
      return result.take_error();
    }
    return zx::ok(false);
  }

  uint8_t buffer_lun = write_booster_buffer_type_ == WriteBoosterBufferType::kLuDedicatedBuffer
                           ? write_booster_dedicated_lu_
                           : 0;
  auto available_buffer_size = ReadAttribute(Attributes::bAvailableWBBufferSize, buffer_lun);
  if (available_buffer_size.is_error()) {
    return available_buffer_size.take_error();
  }

  switch (user_space_configuration_option_) {
    case UserSpaceConfigurationOption::kUserSpaceReduction: {
      // In kUserSpaceReduction mode, flush should be performed when 10% or less of the buffer is
      // left.
      constexpr uint32_t k10PercentBufferRemains = 0x01;
      return zx::ok(available_buffer_size.value() <= k10PercentBufferRemains);
    }
    case UserSpaceConfigurationOption::kPreserveUserSpace: {
      // In kPreserveUserSpace mode, flush should be performed when the current buffer is greater
      // than 0 and the available buffer below write_booster_flush_threshold_ is left.
      auto current_buffer_size = ReadAttribute(Attributes::dCurrentWBBufferSize, buffer_lun);
      if (current_buffer_size.is_error()) {
        return current_buffer_size.take_error();
      }
      if (!current_buffer_size.value()) {
        return zx::ok(false);
      }
      return zx::ok(available_buffer_size.value() < write_booster_flush_threshold_);
    }
    default:
      return zx::error(ZX_ERR_INVALID_ARGS);
  }
}

zx::result<> DeviceManager::InitReferenceClock(inspect::Node &controller_node) {
  // Intel UFSHCI reference clock = 19.2MHz
  constexpr AttributeReferenceClock reference_clock = AttributeReferenceClock::k19_2MHz;
  if (auto result = WriteAttribute(Attributes::bRefClkFreq, reference_clock); result.is_error()) {
    return result.take_error();
  }

  std::string reference_clock_string;
  switch (reference_clock) {
    case AttributeReferenceClock::k19_2MHz:
      reference_clock_string = "19.2 MHz";
      break;
    case AttributeReferenceClock::k26MHz:
      reference_clock_string = "26 MHz";
      break;
    case AttributeReferenceClock::k38_4MHz:
      reference_clock_string = "38.4 MHz";
      break;
    case AttributeReferenceClock::kObsolete:
      reference_clock_string = "52 MHz (Obsolete))";
      break;
    default:
      return zx::error(ZX_ERR_INVALID_ARGS);
  }
  controller_node.RecordString("reference_clock", reference_clock_string);

  return zx::ok();
}

zx::result<> DeviceManager::InitUniproAttributes(inspect::Node &unipro_node) {
  // UniPro Version
  // 7~15 = Above 2.0, 6 = 2.0, 5 = 1.8, 4 = 1.61, 3 = 1.6, 2 = 1.41, 1 = 1.40, 0 = Reserved
  zx::result<uint32_t> remote_version = DmeGet(PA_RemoteVerInfo);
  if (remote_version.is_error()) {
    return remote_version.take_error();
  }
  zx::result<uint32_t> local_version = DmeGet(PA_LocalVerInfo);
  if (local_version.is_error()) {
    return local_version.take_error();
  }

  // UniPro automatically sets timing information such as PA_TActivate through the
  // PACP_CAP_EXT1_ind command during Link Startup operation.
  zx::result<uint32_t> host_t_activate = DmeGet(PA_TActivate);
  if (host_t_activate.is_error()) {
    return host_t_activate.take_error();
  }
  // Intel Lake-field UFSHCI has a quirk. We need to add 200us to the PEER's PA_TActivate.
  DmePeerSetUicCommand dme_peer_set_t_activate(controller_, PA_TActivate, 0,
                                               host_t_activate.value() + 2);
  if (auto result = dme_peer_set_t_activate.SendCommand(); result.is_error()) {
    return result.take_error();
  }
  zx::result<uint32_t> device_t_activate = DmePeerGet(PA_TActivate);
  if (device_t_activate.is_error()) {
    return device_t_activate.take_error();
  }
  // PA_Granularity = 100us (1=1us, 2=4us, 3=8us, 4=16us, 5=32us, 6=100us)
  zx::result<uint32_t> host_granularity = DmeGet(PA_Granularity);
  if (host_granularity.is_error()) {
    return host_granularity.take_error();
  }
  zx::result<uint32_t> device_granularity = DmePeerGet(PA_Granularity);
  if (device_granularity.is_error()) {
    return device_granularity.take_error();
  }

  unipro_node.RecordUint("remote_version", remote_version.value());
  unipro_node.RecordUint("local_version", local_version.value());
  unipro_node.RecordUint("host_t_activate", host_t_activate.value());
  unipro_node.RecordUint("device_t_activate", device_t_activate.value());
  unipro_node.RecordUint("host_granularity", host_granularity.value());
  unipro_node.RecordUint("device_granularity", device_granularity.value());

  return zx::ok();
}

zx::result<> DeviceManager::InitUicPowerMode(inspect::Node &unipro_node) {
  if (zx::result<> result = controller_.Notify(NotifyEvent::kPrePowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  // Update lanes with available TX/RX lanes.
  zx::result<uint32_t> tx_lanes = DmeGet(PA_AvailTxDataLanes);
  if (tx_lanes.is_error()) {
    return tx_lanes.take_error();
  }
  zx::result<uint32_t> rx_lanes = DmeGet(PA_AvailRxDataLanes);
  if (rx_lanes.is_error()) {
    return rx_lanes.take_error();
  }
  // Get max HS-GEAR.
  zx::result<uint32_t> max_rx_hs_gear = DmeGet(PA_MaxRxHSGear);
  if (max_rx_hs_gear.is_error()) {
    return max_rx_hs_gear.take_error();
  }

  // Set data lanes.
  if (zx::result<> result = DmeSet(PA_ActiveTxDataLanes, tx_lanes.value()); result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(PA_ActiveRxDataLanes, rx_lanes.value()); result.is_error()) {
    return result.take_error();
  }

  // Set HS-GEAR to max gear.
  if (zx::result<> result = DmeSet(PA_TxGear, max_rx_hs_gear.value()); result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(PA_RxGear, max_rx_hs_gear.value()); result.is_error()) {
    return result.take_error();
  }

  // Set termination.
  // HS-MODE = ON / LS-MODE = OFF
  if (zx::result<> result = DmeSet(PA_TxTermination, true); result.is_error()) {
    return result.take_error();
  }

  // HS-MODE = ON / LS-MODE = OFF
  if (zx::result<> result = DmeSet(PA_RxTermination, true); result.is_error()) {
    return result.take_error();
  }

  // Set HSSerise (A = 1, B = 2)
  constexpr uint32_t kHsSeries = 2;
  if (zx::result<> result = DmeSet(PA_HSSeries, kHsSeries); result.is_error()) {
    return result.take_error();
  }

  // Set Timeout values.
  if (zx::result<> result = DmeSet(PA_PWRModeUserData0, DL_FC0ProtectionTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(PA_PWRModeUserData1, DL_TC0ReplayTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(PA_PWRModeUserData2, DL_AFC0ReqTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(PA_PWRModeUserData3, DL_FC0ProtectionTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(PA_PWRModeUserData4, DL_TC0ReplayTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(PA_PWRModeUserData5, DL_AFC0ReqTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result =
          DmeSet(DME_LocalFC0ProtectionTimeOutVal, DL_FC0ProtectionTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(DME_LocalTC0ReplayTimeOutVal, DL_TC0ReplayTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = DmeSet(DME_LocalAFC0ReqTimeOutVal, DL_AFC0ReqTimeOutVal_Default);
      result.is_error()) {
    return result.take_error();
  }

  // Set TX/RX PWRMode.
  // TX[3:0], RX[7:4]
  // Fast_Mode=1, Slow_Mode=2, FastAuto_Mode=4, SlowAuto_Mode=5
  constexpr uint32_t kFastMode = 1;
  constexpr uint32_t kRxBitShift = 4;
  constexpr uint32_t kPwrMode = kFastMode << kRxBitShift | kFastMode;
  if (zx::result<> result = DmeSet(PA_PWRMode, kPwrMode); result.is_error()) {
    return result.take_error();
  }

  // Wait for power mode changed.
  auto wait_for_completion = [&]() -> bool {
    return InterruptStatusReg::Get().ReadFrom(&controller_.GetMmio()).uic_power_mode_status();
  };
  fbl::String timeout_message = "Timeout waiting for Power Mode Change";
  if (zx_status_t status =
          controller_.WaitWithTimeout(wait_for_completion, kDeviceInitTimeoutUs, timeout_message);
      status != ZX_OK) {
    return zx::error(status);
  }
  // Clear 'Power Mode completion status'
  InterruptStatusReg::Get().FromValue(0).set_uic_power_mode_status(true).WriteTo(
      &controller_.GetMmio());

  HostControllerStatusReg::PowerModeStatus power_mode_status =
      HostControllerStatusReg::Get()
          .ReadFrom(&controller_.GetMmio())
          .uic_power_mode_change_request_status();
  if (power_mode_status != HostControllerStatusReg::PowerModeStatus::kPowerLocal) {
    zxlogf(ERROR, "Failed to change power mode: power_mode_status = 0x%x", power_mode_status);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPostPowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  // Intel Lake-field UFSHCI has a quirk. We need to wait 1250us and clear dme error.
  usleep(1250);
  // Test with dme_peer_get to make sure there are no errors.
  zx::result<uint32_t> device_granularity = DmePeerGet(PA_Granularity);
  if (device_granularity.is_error()) {
    return device_granularity.take_error();
  }

  unipro_node.RecordUint("PA_ActiveTxDataLanes", tx_lanes.value());
  unipro_node.RecordUint("PA_ActiveRxDataLanes", rx_lanes.value());
  unipro_node.RecordUint("PA_MaxRxHSGear", max_rx_hs_gear.value());
  unipro_node.RecordUint("PA_TxGear", max_rx_hs_gear.value());
  unipro_node.RecordUint("PA_RxGear", max_rx_hs_gear.value());
  unipro_node.RecordBool("tx_termination", true);
  unipro_node.RecordBool("rx_termination", true);
  unipro_node.RecordUint("PA_HSSeries", kHsSeries);
  unipro_node.RecordUint("power_mode", kPwrMode);

  return zx::ok();
}

zx::result<> DeviceManager::SetPowerCondition(scsi::PowerCondition target_power_condition) {
  if (current_power_condition_ == target_power_condition) {
    return zx::ok();
  }

  auto scsi_lun = Ufs::TranslateUfsLunToScsiLun(static_cast<uint8_t>(WellKnownLuns::kUfsDevice));
  if (scsi_lun.is_error()) {
    return scsi_lun.take_error();
  }

  // Send START STOP UNIT to change power mode
  zx_status_t status = controller_.StartStopUnit(kPlaceholderTarget, scsi_lun.value(),
                                                 /*immed*/ false, target_power_condition);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to send START STOP UNIT SCSI command: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  current_power_condition_ = target_power_condition;
  return zx::ok();
}

zx::result<> DeviceManager::Suspend() {
  const UfsPowerMode target_power_mode = UfsPowerMode::kSleep;
  const scsi::PowerCondition target_power_condition = power_mode_map_[target_power_mode].first;
  const LinkState target_link_state = power_mode_map_[target_power_mode].second;

  if (current_power_mode_ == target_power_mode) {
    return zx::ok();
  }

  if (current_power_mode_ != UfsPowerMode::kActive || current_link_state_ != LinkState::kActive) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  // TODO(https://fxbug.dev/42075643): We need to wait for the in flight I/O.
  // TODO(https://fxbug.dev/42075643): Disable background operations.

  // We should check if WriteBooster Flush is needed. If so, we should postpone changing the power
  // mode.
  zx::result<bool> result = NeedWriteBoosterBufferFlush();
  if (result.is_error()) {
    return result.take_error();
  }
  if (result.value()) {
    // TODO(https://fxbug.dev/42075643): We need to keep the power mode active until the
    // Writebooster flush is complete.
    zxlogf(WARNING, "WriteBooster buffer flush is needed");
    return zx::ok();
  }

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPrePowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = SetPowerCondition(target_power_condition); result.is_error()) {
    return result.take_error();
  }

  DmeHibernateEnterCommand dme_hibernate_enter_command(controller_);
  if (auto result = dme_hibernate_enter_command.SendCommand(); result.is_error()) {
    // TODO(https://fxbug.dev/42075643): Link has a problem and needs to perform error recovery.
    return result.take_error();
  }
  current_link_state_ = target_link_state;

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPostPowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  current_power_mode_ = target_power_mode;
  return zx::ok();
}

zx::result<> DeviceManager::Resume() {
  const UfsPowerMode target_power_mode = UfsPowerMode::kActive;
  const scsi::PowerCondition target_power_condition = power_mode_map_[target_power_mode].first;
  const LinkState target_link_state = power_mode_map_[target_power_mode].second;

  if (current_power_mode_ == target_power_mode) {
    return zx::ok();
  }

  if (current_power_mode_ != UfsPowerMode::kSleep || current_link_state_ != LinkState::kHibernate) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPrePowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  DmeHibernateExitCommand dme_hibernate_exit_command(controller_);
  if (auto result = dme_hibernate_exit_command.SendCommand(); result.is_error()) {
    // TODO(https://fxbug.dev/42075643): Link has a problem and needs to perform error recovery.
    return result.take_error();
  }
  current_link_state_ = target_link_state;

  if (zx::result<> result = SetPowerCondition(target_power_condition); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPostPowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  current_power_mode_ = target_power_mode;
  return zx::ok();
}

zx::result<> DeviceManager::InitUfsPowerMode(inspect::Node &controller_node,
                                             inspect::Node &attributes_node) {
  // Read current power mode (bCurrentPowerMode, bActiveIccLevel)
  zx::result<uint32_t> power_mode = ReadAttribute(Attributes::bCurrentPowerMode);
  if (power_mode.is_error()) {
    return power_mode.take_error();
  }
  current_power_mode_ = static_cast<UfsPowerMode>(power_mode.value());
  if (current_power_mode_ != UfsPowerMode::kActive) {
    zxlogf(ERROR, "Initial power mode is not active: 0x%x",
           static_cast<uint8_t>(current_power_mode_));
    return zx::error(ZX_ERR_BAD_STATE);
  }
  zxlogf(DEBUG, "bCurrentPowerMode 0x%0x", power_mode.value());

  current_power_condition_ = power_mode_map_[current_power_mode_].first;
  current_link_state_ = power_mode_map_[current_power_mode_].second;

  // TODO(https://fxbug.dev/42075643): Calculate and set the maximum ICC level. Currently, this
  // value is temporarily set to 0x0F, which is the highest active ICC level.
  if (auto result = WriteAttribute(Attributes::bActiveIccLevel, kHighestActiveIcclevel);
      result.is_error()) {
    return result.take_error();
  }

  // TODO(https://fxbug.dev/42075643): Enable auto hibernate

  attributes_node.RecordUint("bCurrentPowerMode", static_cast<uint8_t>(current_power_mode_));
  attributes_node.RecordUint("bActiveICCLevel", kHighestActiveIcclevel);
  controller_node.RecordUint("PowerCondition", static_cast<uint8_t>(current_power_condition_));
  controller_node.RecordUint("LinkState", static_cast<uint8_t>(current_link_state_));

  return zx::ok();
}

}  // namespace ufs
