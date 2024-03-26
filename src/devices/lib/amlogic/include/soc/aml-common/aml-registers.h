// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_REGISTERS_H_
#define SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_REGISTERS_H_

namespace aml_registers {

// REGISTER_USB_PHY_V2_RESET constants
constexpr uint32_t USB_RESET1_REGISTER_UNKNOWN_1_MASK = 0x4;
constexpr uint32_t USB_RESET1_REGISTER_UNKNOWN_2_MASK = 0x1'0000;
constexpr uint32_t USB_RESET1_LEVEL_MASK = 0x3'0000;
constexpr uint32_t A5_USB_RESET0_MASK = 0x10;
constexpr uint32_t A5_USB_RESET0_LEVEL_MASK = 0x100;
constexpr uint32_t A1_USB_RESET1_MASK = 0x10;
constexpr uint32_t A1_USB_RESET1_LEVEL_MASK = 0x40;
constexpr uint32_t USB_RESET1_LEVEL_UNKNOWN_MASK = 0xF << 26;

// REGISTER_NNA_RESET_LEVEL2 constants
constexpr uint32_t NNA_RESET2_LEVEL_MASK = 0x1000;
constexpr uint32_t A5_NNA_RESET1_LEVEL_MASK = 0x8000;

// REGISTER_MALI_RESET constants
constexpr uint32_t MALI_RESET0_MASK = 0x100000;
constexpr uint32_t MALI_RESET2_MASK = 0x4000;

// REGISTER_ISP_RESET constants
constexpr uint32_t ISP_RESET4_MASK = 0x2;

constexpr uint32_t SPICC0_RESET_MASK = 1 << 1;
constexpr uint32_t SPICC1_RESET_MASK = 1 << 6;

}  // namespace aml_registers

#endif  // SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_REGISTERS_H_
