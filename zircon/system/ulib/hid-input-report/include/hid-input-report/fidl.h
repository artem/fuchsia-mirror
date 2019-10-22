// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef HID_INPUT_REPORT_FIDL_H_
#define HID_INPUT_REPORT_FIDL_H_

#include <fuchsia/input/report/llcpp/fidl.h>

#include <hid-input-report/descriptors.h>

namespace hid_input_report {

struct FidlMouseDescriptor {
  FidlMouseDescriptor() {}
  ::llcpp::fuchsia::input::report::MouseDescriptor mouse_descriptor;
  ::llcpp::fuchsia::input::report::MouseDescriptor::Builder mouse_builder =
      ::llcpp::fuchsia::input::report::MouseDescriptor::Build();

  ::llcpp::fuchsia::input::report::Axis movement_x = {};
  ::llcpp::fuchsia::input::report::Axis movement_y = {};
  ::llcpp::fuchsia::input::report::Axis scroll_v = {};
  ::llcpp::fuchsia::input::report::Axis scroll_h = {};
  fidl::VectorView<uint8_t> buttons_view;
  uint8_t buttons[hid_input_report::kMouseMaxButtons] = {};
};

struct FidlSensorDescriptor {
  FidlSensorDescriptor() {}
  ::llcpp::fuchsia::input::report::SensorDescriptor sensor_descriptor;
  ::llcpp::fuchsia::input::report::SensorDescriptor::Builder sensor_builder =
      ::llcpp::fuchsia::input::report::SensorDescriptor::Build();
  std::array<::llcpp::fuchsia::input::report::SensorAxis,
             ::llcpp::fuchsia::input::report::SENSOR_MAX_VALUES>
      values;
  fidl::VectorView<::llcpp::fuchsia::input::report::SensorAxis> values_view;
};

struct FidlDescriptor {
  FidlDescriptor() {}
  ::llcpp::fuchsia::input::report::DeviceDescriptor::Builder descriptor_builder =
      ::llcpp::fuchsia::input::report::DeviceDescriptor::Build();

  FidlMouseDescriptor mouse_descriptor;
  FidlSensorDescriptor sensor_descriptor;
};

struct FidlMouseReport {
  FidlMouseReport() {}
  ::llcpp::fuchsia::input::report::MouseReport mouse_report;
  ::llcpp::fuchsia::input::report::MouseReport::Builder mouse_builder =
      ::llcpp::fuchsia::input::report::MouseReport::Build();
  fidl::VectorView<uint8_t> buttons_view;

  // Holds the actual data that the builders/views point to.
  MouseReport report_data;
};

struct FidlSensorReport {
  FidlSensorReport() {}
  ::llcpp::fuchsia::input::report::SensorReport sensor_report;
  ::llcpp::fuchsia::input::report::SensorReport::Builder sensor_builder =
      ::llcpp::fuchsia::input::report::SensorReport::Build();
  fidl::VectorView<int64_t> values_view;

  // Holds the actual data that the builders/views point to.
  SensorReport report_data;
};

struct FidlReport {
  FidlReport() {}
  ::llcpp::fuchsia::input::report::InputReport::Builder report_builder =
      ::llcpp::fuchsia::input::report::InputReport::Build();
  std::variant<FidlMouseReport, FidlSensorReport> report;
};

// Builds the |FidlDescriptor| object from the |ReportDescriptor|.
zx_status_t SetFidlDescriptor(const ReportDescriptor& hid_descriptor, FidlDescriptor* descriptor);
// Builds the |FidlReport| object from the |Report|.
zx_status_t SetFidlReport(const Report& hid_report, FidlReport* report);

}  // namespace hid_input_report

#endif  // HID_INPUT_REPORT_FIDL_H_
