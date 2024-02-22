// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_PANEL_KD_KD070D82_FITIPOWER_JD9364_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_PANEL_KD_KD070D82_FITIPOWER_JD9364_H_

#include <lib/mipi-dsi/mipi-dsi.h>

#include <cstdint>

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

namespace amlogic_display {

// clang-format off

constexpr uint8_t lcd_init_sequence_KD_KD070D82_FITIPOWER_JD9364[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x9e,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0xaa,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x70,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x7e,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0xf3,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x5a,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x3d,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x3d,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x4f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x52,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x4e,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x2d, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x0b,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x0d,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0xb4,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0xbc,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xaf, 0x20,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 125,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    kDsiOpSleep, 0xff,
};

// clang-format on

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_PANEL_KD_KD070D82_FITIPOWER_JD9364_H_
