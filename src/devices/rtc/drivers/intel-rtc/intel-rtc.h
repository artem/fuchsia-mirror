// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_RTC_DRIVERS_INTEL_RTC_INTEL_RTC_H_
#define SRC_DEVICES_RTC_DRIVERS_INTEL_RTC_INTEL_RTC_H_

#include <fidl/fuchsia.hardware.rtc/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <librtc_llcpp.h>

#include <ddktl/device.h>

namespace intel_rtc {
constexpr size_t kRtcBankSize = 128;

namespace FidlRtc = rtc::FidlRtc;

enum Registers {
  kRegSeconds,
  kRegSecondsAlarm,
  kRegMinutes,
  kRegMinutesAlarm,
  kRegHours,
  kRegHoursAlarm,
  kRegDayOfWeek,
  kRegDayOfMonth,
  kRegMonth,
  kRegYear,
  kRegA,
  kRegB,
  kRegC,
  kRegD,
};

constexpr uint8_t kHourPmBit = (1 << 7);
enum RegisterA {
  kRegAUpdateInProgressBit = 1 << 7,
};

enum RegisterB {
  kRegBHourFormatBit = 1 << 1,
  kRegBDataFormatBit = 1 << 2,
  kRegBUpdateCycleInhibitBit = 1 << 7,
};

class RtcDevice;
using DeviceType = ddk::Device<RtcDevice, ddk::Messageable<FidlRtc::Device>::Mixin>;

class RtcDevice : public DeviceType {
 public:
  RtcDevice(zx_device_t* parent, uint16_t port_base) : DeviceType(parent), port_base_(port_base) {}

  void DdkRelease() { delete this; }

  // fuchsia.hardware.rtc implementation.
  void Get(GetCompleter::Sync& completer) override;
  void Set(SetRequestView request, SetCompleter::Sync& completer) override;

  FidlRtc::wire::Time ReadTime() __TA_EXCLUDES(time_lock_);
  void WriteTime(FidlRtc::wire::Time time) __TA_EXCLUDES(time_lock_);

 private:
  // Read a register without doing any transformation of the value.
  uint8_t ReadRegRaw(Registers reg) __TA_REQUIRES(time_lock_);
  // Write a register without doing any transformation of the value.
  void WriteRegRaw(Registers reg, uint8_t val) __TA_REQUIRES(time_lock_);

  // Read a register, converting from BCD to binary if necessary.
  uint8_t ReadReg(Registers reg) __TA_REQUIRES(time_lock_);
  // Write a register, converting from binary to BCD if necessary.
  void WriteReg(Registers reg, uint8_t val) __TA_REQUIRES(time_lock_);

  // Returns the hour in 24-hour representation.
  uint8_t ReadHour() __TA_REQUIRES(time_lock_);
  // hour should be in 24-hour representation.
  void WriteHour(uint8_t hour) __TA_REQUIRES(time_lock_);

  // Updates |is_24_hour_| and |is_bcd_|.
  void CheckRtcMode() __TA_REQUIRES(time_lock_);

  uint16_t port_base_;

  bool is_24_hour_;
  bool is_bcd_;

  std::mutex time_lock_;
};

}  // namespace intel_rtc

#endif  // SRC_DEVICES_RTC_DRIVERS_INTEL_RTC_INTEL_RTC_H_
