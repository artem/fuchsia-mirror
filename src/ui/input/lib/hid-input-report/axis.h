// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_LIB_HID_INPUT_REPORT_AXIS_H_
#define SRC_UI_INPUT_LIB_HID_INPUT_REPORT_AXIS_H_

#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/hid-parser/parser.h>
#include <lib/hid-parser/units.h>
#include <lib/hid-parser/usages.h>

namespace hid_input_report {

fuchsia_input_report::wire::Unit HidUnitToLlcppUnit(hid::unit::UnitType unit);

zx_status_t HidSensorUsageToLlcppSensorType(hid::usage::Sensor usage,
                                            fuchsia_input_report::wire::SensorType* type);

zx_status_t HidLedUsageToLlcppLedType(hid::usage::LEDs usage,
                                      fuchsia_input_report::wire::LedType* type);

fuchsia_input_report::wire::Axis LlcppAxisFromAttribute(const hid::Attributes& attrs);

}  // namespace hid_input_report

#endif  // SRC_UI_INPUT_LIB_HID_INPUT_REPORT_AXIS_H_
