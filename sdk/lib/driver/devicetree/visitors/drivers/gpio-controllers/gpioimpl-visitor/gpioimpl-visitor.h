// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_GPIO_CONTROLLERS_GPIOIMPL_VISITOR_GPIOIMPL_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_GPIO_CONTROLLERS_GPIOIMPL_VISITOR_GPIOIMPL_VISITOR_H_

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/fidl.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/property-parser.h>

#include <cstdint>
#include <memory>
#include <vector>

namespace gpio_impl_dt {

class GpioImplVisitor : public fdf_devicetree::Visitor {
 public:
  static constexpr char kGpioReference[] = "gpios";
  static constexpr char kGpioCells[] = "#gpio-cells";
  static constexpr char kGpioNames[] = "gpio-names";

  static constexpr char kPinCtrl0[] = "pinctrl-0";
  static constexpr char kPins[] = "pins";
  static constexpr char kPinFunction[] = "function";
  static constexpr char kPinDriveStrengthUa[] = "drive-strength-microamp";
  static constexpr char kPinOutputLow[] = "output-low";
  static constexpr char kPinOutputHigh[] = "output-high";
  static constexpr char kPinInputEnable[] = "input-enable";
  static constexpr char kPinBiasPullDown[] = "bias-pull-down";
  static constexpr char kPinBiasPullUp[] = "bias-pull-up";
  static constexpr char kPinBiasDisable[] = "bias-disable";

  GpioImplVisitor();

  zx::result<> FinalizeNode(fdf_devicetree::Node& node) override;

  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

 private:
  struct GpioController {
    std::vector<uint8_t> gpio_pins_metadata;
    fuchsia_hardware_gpioimpl::InitMetadata init_steps;
  };

  // Return an existing or a new instance of GpioController.
  GpioController& GetController(uint32_t node_id);

  // Helper to parse nodes with a reference to gpio-controller in "gpios" property.
  zx::result<> ParseReferenceChild(fdf_devicetree::Node& child,
                                   fdf_devicetree::ReferenceNode& parent,
                                   fdf_devicetree::PropertyCells specifiers,
                                   std::optional<std::string_view> gpio_name);

  // Helper to parse gpio init hog to produce fuchsia_hardware_gpioimpl::InitStep.
  zx::result<> ParseGpioHogChild(fdf_devicetree::Node& child);

  // Helper to parse pin controller configuration as it relates to gpio.
  zx::result<> ParsePinCtrlCfg(fdf_devicetree::Node& child, fdf_devicetree::ReferenceNode& cfg_node,
                               fdf_devicetree::ParentNode& gpio_node);

  zx::result<fdf_devicetree::ParentNode> GetGpioNodeForPinConfig(
      fdf_devicetree::ReferenceNode& cfg_node);

  zx::result<> AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t pin, std::string gpio_name);

  zx::result<> AddInitNodeSpec(fdf_devicetree::Node& child);

  bool is_match(const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties);

  // Mapping of gpio controller node ID to its info.
  std::map<uint32_t, GpioController> gpio_controllers_;
  std::unique_ptr<fdf_devicetree::PropertyParser> gpio_parser_;
  std::unique_ptr<fdf_devicetree::PropertyParser> pinctrl_cfg_parser_;
  std::unique_ptr<fdf_devicetree::PropertyParser> pinctrl_state_parser_;
};

}  // namespace gpio_impl_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_GPIO_CONTROLLERS_GPIOIMPL_VISITOR_GPIOIMPL_VISITOR_H_
