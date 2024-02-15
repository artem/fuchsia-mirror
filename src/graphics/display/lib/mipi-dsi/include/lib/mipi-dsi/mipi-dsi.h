// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_MIPI_DSI_INCLUDE_LIB_MIPI_DSI_MIPI_DSI_H_
#define SRC_GRAPHICS_DISPLAY_LIB_MIPI_DSI_INCLUDE_LIB_MIPI_DSI_MIPI_DSI_H_

#include <cstddef>
#include <cstdint>

// Some of the constants here are from the MIPI Alliance Specification for
// Display Serial Interface (DSI), which can be obtained from
// https://www.mipi.org/specifications/dsi
//
// mipi_dsi1 is Version 1.3.2, adopted on 23 September 2021.

// Assigned Virtual Channel ID
// TODO(payamm): Will need to generate and maintain VCID for multi-display
// solutions
constexpr uint8_t kMipiDsiVirtualChanId = 0;

// Data types for DSI Processor-to-Peripheral packets.
//
// mipi_dsi1 8.7.1 "Processor-sourced Data Type Summary" Table 16 "Data Types
// for Processor-Sourced Packets"

constexpr uint8_t kMipiDsiDtVsyncStart = 0x01;
constexpr uint8_t kMipiDsiDtVsyncEnd = 0x11;
constexpr uint8_t kMipiDsiDtHsyncStart = 0x21;
constexpr uint8_t kMipiDsiDtHsyncEnd = 0x31;
constexpr uint8_t kMipiDsiDtEotp = 0x08;
constexpr uint8_t kMipiDsiDtColorModeOff = 0x02;
constexpr uint8_t kMipiDsiDtColorModeOn = 0x12;
constexpr uint8_t kMipiDsiDtPeriCmdOff = 0x22;
constexpr uint8_t kMipiDsiDtPeriCmdOn = 0x32;
constexpr uint8_t kMipiDsiDtGenShortWrite0 = 0x03;
constexpr uint8_t kMipiDsiDtGenShortWrite1 = 0x13;
constexpr uint8_t kMipiDsiDtGenShortWrite2 = 0x23;
constexpr uint8_t kMipiDsiDtGenShortRead0 = 0x04;
constexpr uint8_t kMipiDsiDtGenShortRead1 = 0x14;
constexpr uint8_t kMipiDsiDtGenShortRead2 = 0x24;
constexpr uint8_t kMipiDsiDtDcsShortWrite0 = 0x05;
constexpr uint8_t kMipiDsiDtDcsShortWrite1 = 0x15;
constexpr uint8_t kMipiDsiDtDcsRead0 = 0x06;
constexpr uint8_t kMipiDsiDtSetMaxRetPkt = 0x37;
constexpr uint8_t kMipiDsiDtNullPkt = 0x09;
constexpr uint8_t kMipiDsiDtBlakingPkt = 0x19;
constexpr uint8_t kMipiDsiDtGenLongWrite = 0x29;
constexpr uint8_t kMipiDsiDtDcsLongWrite = 0x39;
constexpr uint8_t kMipiDsiDtYcbcr42220bit = 0x0C;
constexpr uint8_t kMipiDsiDtYcbcr42224bit = 0x1C;
constexpr uint8_t kMipiDsiDtYcbcr42216bit = 0x2C;
constexpr uint8_t kMipiDsiDtRgb101010 = 0x0D;
constexpr uint8_t kMipiDsiDtRgb121212 = 0x1D;
constexpr uint8_t kMipiDsiDtYcbcr42012bit = 0x3D;
constexpr uint8_t kMipiDsiDtRgb565 = 0x0E;
constexpr uint8_t kMipiDsiDtRgb666 = 0x1E;
constexpr uint8_t kMipiDsiDtRgb666L = 0x2E;
constexpr uint8_t kMipiDsiDtRgb888 = 0x3E;
constexpr uint8_t kMipiDsiDtUnknown = 0xFF;

// Data types for DSI Peripheral-to-Processor (response) packets.
//
// mipi_dsi1 8.10 "Peripheral-to-Processor Transactions – Detailed Format
// Description" Table 22 "Data Types for Peripheral-Sourced Packets"

constexpr uint8_t kMipiDsiRspGenShort1 = 0x11;
constexpr uint8_t kMipiDsiRspGenShort2 = 0x12;
constexpr uint8_t kMipiDsiRspGenLong = 0x1A;
constexpr uint8_t kMipiDsiRspDcsLong = 0x1C;
constexpr uint8_t kMipiDsiRspDcsShort1 = 0x21;
constexpr uint8_t kMipiDsiRspDcsShort2 = 0x22;

// MipiDsiCmd flag bit def
enum {
  MIPI_DSI_CMD_FLAGS_ACK = (1 << 0),
  MIPI_DSI_CMD_FLAGS_SET_MAX = (1 << 1),
};

#endif  // SRC_GRAPHICS_DISPLAY_LIB_MIPI_DSI_INCLUDE_LIB_MIPI_DSI_MIPI_DSI_H_
