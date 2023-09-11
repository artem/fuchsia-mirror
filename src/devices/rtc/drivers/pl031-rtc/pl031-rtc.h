// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_RTC_DRIVERS_PL031_RTC_PL031_RTC_H_
#define SRC_DEVICES_RTC_DRIVERS_PL031_RTC_PL031_RTC_H_

#include <fidl/fuchsia.hardware.rtc/cpp/wire.h>
#include <lib/ddk/device.h>
#include <lib/mmio/mmio.h>

#include <ddktl/device.h>

namespace rtc {

namespace FidlRtc = fuchsia_hardware_rtc;
class Pl031;
using RtcDeviceType = ddk::Device<Pl031, ddk::Messageable<FidlRtc::Device>::Mixin>;

struct Pl031Regs {
  uint32_t dr;
  uint32_t mr;
  uint32_t lr;
  uint32_t cr;
  uint32_t msc;
  uint32_t ris;
  uint32_t mis;
  uint32_t icr;
};

class Pl031 : public RtcDeviceType {
 public:
  static zx_status_t Bind(void*, zx_device_t* dev);

  Pl031(zx_device_t* parent, fdf::MmioBuffer mmio);
  ~Pl031() = default;

  // fidl::WireServer<FidlRtc::Device>:
  void Get(GetCompleter::Sync& completer) override;
  void Set(SetRequestView request, SetCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<FidlRtc::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}  // No-op

  // DDK bindings.
  void DdkRelease();

 private:
  zx_status_t SetRtc(FidlRtc::wire::Time rtc);

  fdf::MmioBuffer mmio_;
  MMIO_PTR Pl031Regs* regs_;
};

}  // namespace rtc

#endif  // SRC_DEVICES_RTC_DRIVERS_PL031_RTC_PL031_RTC_H_
