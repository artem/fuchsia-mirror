// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_DEVICE_MANAGER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_DEVICE_MANAGER_H_

#include <lib/inspect/cpp/inspect.h>
#include <lib/scsi/controller-dfv1.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>

#include <map>

#include "src/devices/block/drivers/ufs/transfer_request_processor.h"
#include "src/devices/block/drivers/ufs/upiu/attributes.h"
#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/flags.h"

namespace ufs {

// UFS Specification Version 3.1, section 7.4.3 "Power Mode Control".
enum class UfsPowerMode : uint8_t {
  kIdle = 0x00,
  kPreActive = 0x10,
  kActive = 0x11,
  kPreSleep = 0x20,
  kSleep = 0x21,
  kPrePowerDown = 0x30,
  kPowerDown = 0x33,
};

constexpr uint8_t kLowestActiveIcclevel = 0x00;
constexpr uint8_t kHighestActiveIcclevel = 0x0f;

enum class LinkState : uint8_t {
  kOff = 0,     // Power down / Disable
  kActive = 1,  // Fast / Slow / Sleep state
  kHibernate = 2,
  kBroken = 3,
};

// UFS Specification Version 4.0, section 13.4.18 "WriteBooster".
enum class WriteBoosterBufferType : uint8_t {
  kLuDedicatedBuffer = 0x00,
  kSharedBuffer = 0x01,
};

enum class UserSpaceConfigurationOption : uint8_t {
  kUserSpaceReduction = 0x00,
  kPreserveUserSpace = 0x01,
};

using PowerModeMap = std::map<UfsPowerMode, std::pair<scsi::PowerCondition, LinkState>>;

// UFS Specification Version 3.1, section 5.1.2 "UFS Device Manager"
// The device manager has the following two responsibilities:
// - Handling device level operations.
// - Managing device level configurations.
// Device level operations include functions such as device power management, settings related to
// data transfer, background operations enabling, and other device specific operations.
// Device level configuration is managed by the device manager by maintaining and storing a set of
// descriptors. The device manager handles commands like query request which allow to modify or
// retrieve configuration information of the device.

// Query requests and link-layer control should be sent from the DeviceManager.
class Ufs;
class DeviceManager {
 public:
  static zx::result<std::unique_ptr<DeviceManager>> Create(
      Ufs &controller, TransferRequestProcessor &transfer_request_processor);
  explicit DeviceManager(Ufs &controller, TransferRequestProcessor &transfer_request_processor)
      : controller_(controller), req_processor_(transfer_request_processor) {}

  // Device initialization.
  zx::result<> SendLinkStartUp();
  zx::result<> DeviceInit();
  zx::result<uint32_t> GetBootLunEnabled();
  zx::result<> GetControllerDescriptor();
  zx::result<UnitDescriptor> ReadUnitDescriptor(uint8_t lun);

  // WriteBooster
  zx::result<> ConfigureWriteBooster(inspect::Node &wb_node);
  bool IsWriteBoosterEnabled() const { return is_write_booster_enabled_; }

  // Device power management.
  zx::result<> InitReferenceClock(inspect::Node &controller_node);
  zx::result<> InitUniproAttributes(inspect::Node &unipro_node);
  zx::result<> InitUicPowerMode(inspect::Node &unipro_node);
  zx::result<> InitUfsPowerMode(inspect::Node &controller_node, inspect::Node &attributes_node);

  zx::result<> Suspend();
  zx::result<> Resume();

  bool IsSuspended() const { return current_power_mode_ != UfsPowerMode::kActive; }

  GeometryDescriptor &GetGeometryDescriptor() { return geometry_descriptor_; }

  // This function is only used for the QEMU quirk case.
  void SetCurrentPowerMode(UfsPowerMode power_mode) {
    current_power_mode_ = power_mode;
    current_power_condition_ = power_mode_map_[power_mode].first;
    current_link_state_ = power_mode_map_[power_mode].second;
  }

  uint8_t GetMaxLunCount() const { return max_lun_count_; }

  // for test
  DeviceDescriptor &GetDeviceDescriptor() { return device_descriptor_; }
  PowerModeMap &GetPowerModeMap() { return power_mode_map_; }
  UfsPowerMode GetCurrentPowerMode() const { return current_power_mode_; }
  scsi::PowerCondition GetCurrentPowerCondition() const { return current_power_condition_; }
  LinkState GetCurrentLinkState() const { return current_link_state_; }

 private:
  friend class UfsTest;

  zx::result<uint32_t> ReadAttribute(Attributes attribute, uint8_t index = 0);
  zx::result<> WriteAttribute(Attributes attribute, uint32_t value, uint8_t index = 0);
  zx::result<uint32_t> DmeGet(uint16_t mbi_attribute);
  zx::result<uint32_t> DmePeerGet(uint16_t mbi_attribute);
  zx::result<> DmeSet(uint16_t mbi_attribute, uint32_t value);

  zx::result<> SetPowerCondition(scsi::PowerCondition power_condition);

  zx::result<bool> IsWriteBoosterBufferLifeTimeLeft();
  zx::result<> EnableWriteBooster(inspect::Node &wb_node);
  zx::result<> DisableWriteBooster();
  zx::result<bool> NeedWriteBoosterBufferFlush();

  Ufs &controller_;
  TransferRequestProcessor &req_processor_;

  DeviceDescriptor device_descriptor_;
  GeometryDescriptor geometry_descriptor_;

  uint8_t max_lun_count_;

  // WriteBooster
  bool is_write_booster_enabled_ = false;
  bool is_write_booster_flush_enabled_ = false;
  uint8_t write_booster_dedicated_lu_;
  WriteBoosterBufferType write_booster_buffer_type_;
  UserSpaceConfigurationOption user_space_configuration_option_;
  uint32_t write_booster_flush_threshold_ = 4;  // 40% of the available buffer size.

  // Power management
  UfsPowerMode current_power_mode_ = UfsPowerMode::kIdle;
  scsi::PowerCondition current_power_condition_ = scsi::PowerCondition::kIdle;
  LinkState current_link_state_ = LinkState::kOff;

  // There are 3 power modes for UFS devices: UFS power mode, SCSI power condition, and Unipro link
  // state. We need to relate and use them appropriately.
  PowerModeMap power_mode_map_ = {
      {UfsPowerMode::kActive, {scsi::PowerCondition::kActive, LinkState::kActive}},
      {UfsPowerMode::kSleep, {scsi::PowerCondition::kIdle, LinkState::kHibernate}},
  };
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_DEVICE_MANAGER_H_
