// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_THERMAL_DRIVERS_AML_THERMISTOR_THERMISTOR_H_
#define SRC_DEVICES_THERMAL_DRIVERS_AML_THERMISTOR_THERMISTOR_H_

#include <lib/ddk/device.h>
#include <lib/thermal/ntc.h>

#include <fbl/ref_ptr.h>

#include "thermistor-channel.h"

namespace thermal {

class AmlThermistor;
using DeviceType = ddk::Device<AmlThermistor, ddk::Initializable>;

class AmlThermistor : public DeviceType {
 public:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(AmlThermistor);
  AmlThermistor(zx_device_t* device) : DeviceType(device) {}

  static zx_status_t Create(void* ctx, zx_device_t* device);

  void DdkInit(ddk::InitTxn txn);

  void DdkRelease() { delete this; }

 private:
  zx_status_t AddThermChannel(NtcChannel ch, NtcInfo info);
};
}  // namespace thermal

#endif  // SRC_DEVICES_THERMAL_DRIVERS_AML_THERMISTOR_THERMISTOR_H_
