// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_PANEL_BOE_TV070WSM_FITIPOWER_JD9365_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_PANEL_BOE_TV070WSM_FITIPOWER_JD9365_H_

#include <lib/mipi-dsi/mipi-dsi.h>

#include <cstdint>

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

namespace amlogic_display {

// clang-format off

constexpr uint8_t lcd_init_sequence_BOE_TV070WSM_FITIPOWER_JD9365[] = {
    // Sleeps for 10 milliseconds.
    //
    // BOE TV070WSM-TG1 spec, Section 8.0 "Power Sequence", page 21 states that
    // the interval between VCC being ready and display reset must be greater
    // than 10ms.
    kDsiOpSleep, 10,

    // Resets the panel by setting the GPIO LCD RESET pin.
    // The GPIO pin #0 is bound to the display panel's reset (LCD RESET) pin.

    // Pulls up the GPIO LCD RESET pin. Waits for 30 milliseconds.
    kDsiOpGpio, 3, 0, 1, 30,

    // Pulls down the GPIO LCD RESET pin. Waits for 10 milliseconds.
    //
    // BOE TV070WSM-TG1 spec, Section 8.0 "Power Sequence", page 21 states that
    // the duration of the RESET pin being pulled down should be greater than
    // 10 microseconds.
    kDsiOpGpio, 3, 0, 0, 10,

    // Pulls up the GPIO LCD RESET pin to finish the reset. Waits for 30
    // milliseconds.
    //
    // BOE TV070WSM-TG1 spec, Section 8.0 "Power Sequence", page 21 states that
    // interval from display reset to entering MIPI low-power mode should be
    // greater than 5 milliseconds.
    kDsiOpGpio, 3, 0, 1, 30,

    // RDDIDIF (Read Display Identification Information) - MIPI DCS command 0x04
    //
    // Reads 3 8-bit registers containing the display identification
    // information.
    //
    // JD9365 datasheet Section 10.2.3, page 130
    //
    // The 1st parameter identifies the LCD module’s manufacturer.
    // The 2nd parameter defines the panel type and tracks the LCD module /
    // driver version.
    // The 3rd parameter identifies the LCD module and driver.
    kDsiOpReadReg, 2, 4, 3,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 0.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,

    // SET_PASSWORD - 0xe1-0xe3 on all pages
    //
    // JD9365 user guide Section 2.6.5, page 23
    //
    // The password (0x93, 0x65, 0xf8) enables standard DCS commands and all
    // in-house registers.
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 1.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,

    // Panel Voltage Setup

    // VCOM_SET (Set VCOM Voltage) - User page 1, 0x00-0x01
    //
    // Sets the panel common voltage (VCOM) when the gates are scanned
    // normally, i.e. from the top to the bottom.
    //
    // JD9365 user guide Section 2.7.1, page 28
    //
    // Sets the VCOM[8:0] register to 0x090.
    // The VCOM voltage is set to
    //     0.3 V - 10 mV * VCOM[8:0] = 0.3 V - 10mV * 144 = -1.14 V.
    //
    // SETPANEL sets the gate scanning order to top-to-bottom, so this voltage
    // setting is effective.
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x90,

    // VCOM_R_SET (Set VCOM Voltage for reverse scan) - User page 1, 0x03-0x04
    //
    // Sets the panel common voltage (VCOM) when the gates are scanned
    // reversely, i.e. from the bottom to the top.
    //
    // JD9365 user guide Section 2.7.2, page 29
    //
    // Sets the VCOM_R[8:0] register to 0x090.
    // The VCOM voltage is set to
    //     0.3 V - 10 mV * VCOM_R[8:0] = 0.3 V - 10mV * 144 = -1.14 V.
    //
    // SETPANEL sets the gate scanning order to top-to-bottom, so this voltage
    // setting is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x90,

    // GAMMA_SET (Set Gamma Reference Voltage) - User page 1, 0x17-0x1c
    //
    // Sets the source and reference voltages for the Gamma circuit. The
    // Gamma circuit supplies power for the Source driver.
    //
    // JD9365 user guide Section 2.7.4, page 32
    //
    // Sets the VGMP[8:0] register to 0x0b0.
    // The power source voltage for positive polarity (VGMP) is set to
    //      2.6 V + (VGMP[8:0] - 0x27) * 12.5 mV = 4.3125 V.
    // Sets the VGSP[8:0] register to 0x001.
    // The reference voltage for positive polarity (VGSP) is set to
    //      0.3 V + (VGSP[8:0] - 0x01) * 12.5 mV = 0.3 V.
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x01,
    // Sets the VGMN[8:0] register to 0x0b0.
    // The power source voltage for negative polarity (VGMN) is set to
    //      -2.6 V - (VGMN[8:0] - 0x27) * 12.5 mV = -4.3125 V.
    // Sets the VGSN[8:0] register to 0x001.
    // The reference voltage for negative polarity (VGSN) is set to
    //      -0.3 V - (VGSN[8:0] - 0x01) * 12.5 mV = -0.3 V.
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x01,

    // SETSTBA (Set Source Options) - User page 1, 0x35-0x36
    //
    // Sets the output ability of the operational amplifiers (OP or OP-AMP)
    // of the Gamma circuit and the Source channels.
    //
    // JD9365 user guide Section 2.7.8, page 37
    //
    // Sets the GAP[2:0] register to 0x2.
    // The output bias current of the Gamma circuit OP-AMP is set to
    //          (1 + GAP[2:0]) * IREF = 3 * IREF
    // where IREF stands for the fixed current.
    //
    // Sets the SAP[2:0] register to 0x8.
    // The output bias current of the Source channel OP-AMP is set to
    //          (1 + SAP[2:0]) * IREF = 9 * IREF
    // where IREF stands for the fixed current.
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x28,

    // SETPANEL (Set Panel Related Configurations) - User page 1, 0x37
    //
    // Configures the sources and gates of the panel.
    //
    // JD9365 user guide Section 2.7.9, page 38
    //
    // Sets the Z_line register to 1.
    // The Zig-zag source (selected by ENZ[1:0]) outputs to gates (i.e. lines)
    // of even numbers on the Zig-zag inversion mode.
    //
    // Sets the ENZ[1:0] register to 0b10.
    // The rightmost Zig-zag source (SZ[3]) is used for Zig-zag inversion mode.
    //
    // The two registers above are effective iff the Zig-zag inversion mode is
    // selected in SETRGBCYC. The current SETRGBCYC configuration selects the
    // Zig-zag inversion mode, so these registers are effective.
    //
    // Sets the SS_Panel register to 1.
    // The source voltage signals (S[1:2400]) are scanned horizontally from
    // the right (S[2400]) to the left (S[1]).
    //
    // Sets the GS_Panel register to 0.
    // The gate voltage signals (G[1:1280]) are scanned vertically from the
    // top (G[1]) to the bottom (G[1280]).
    //
    // Sets the REV_Panel register to 0.
    // The panel is normally black.
    //
    // Sets the BGR_Panel register to 1.
    // Source signals are mapped to subpixel components in the order of
    // (B, G, R).
    // For example, for non-zig-zag inversion modes, the first three source
    // signals are (S[1], S[2], S[3]), and they will be mapped to B, G, and R
    // components of a pixel respectively.
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x69,

    // SETRGBCYC (Set Display Waveform Cycles for RGB Mode) - User page 1, 0x38-0x3f
    //
    // Configures the display waveform cycles, i.e. the timings of the source
    // data (SD) signals emitted by the source driver.
    //
    // JD9365 user guide Section 2.7.10, pages 39-40
    //
    // Sets the RGB_JDT[2:0] register to 0b101.
    // The source driver uses the Zig-zag inversion method to drive the
    // source line of the TFT panel.
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x05,
    //
    // Sets the RGB_N_EQ1[7:0] register to 0x00.
    // The duration of the first equalization stage to pull the source driver
    // (SD) output signals to ground voltage (GND) is 0 timing clocks.
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    //
    // Sets the RGB_N_EQ2[7:0] register to 0x01.
    // The duration of the second equalization stage to pull the source driver
    // (SD) output signals to analog voltage input (VCI) is 1 timing clock, i.e.
    // 4 oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    //
    // Sets the RGB_N_EQ3[7:0] register to 0x01.
    // The duration of the third equalization stage to pull the source driver
    // (SD) output signals from grayscale voltage (e.g. +V255) back to
    // analog voltage input (VCI) is 144 timing clock, i.e. 144 * 4 = 576
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    //
    // Configures the output time of the source driver operational amplifier
    // (SD OP).
    //
    // Sets the RGB_CHGEN_ON[7:0] register to 0xff.
    // The charging enable (CHGEN) signal is enabled 255 timing clocks after
    // a horizontal sync signal.
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    //
    // Sets the RGB_CHGEN_OFF[7:0] register to 0xff.
    // The charging enable (CHGEN) signal of the first SD OP is disabled 255
    // timing clocks after a horizontal sync signal. This means the first
    // SD OP is never enabled.
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    //
    // Sets the RGB_CHGEN_OFF2[7:0] register to 0xff.
    // The charging enable (CHGEN) signal of the second SD OP is disabled 255
    // timing clocks after a horizontal sync signal. This means the second
    // SD OP is never enabled.
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,

    // SET TCON (Timing controller settings) - User page 1, 0x40-0x4c
    //
    // Configures the timing controller.
    //
    // JD9365 user guide Section 2.7.11, pages 41-45
    //
    // Configures the horizontal and vertical resolution to 600 x 1024.
    //
    // Sets the RSO[2:0] register to 0b010.
    // The horizontal resolution is 600 pixels. The display driver IC enables
    // source channels S[1:900] and S[1503:2400].
    //
    // Sets the LN[9:0] register to 0x200.
    // The vertical resolution is
    //     2 * LN[9:0] = 2 * 512 = 1024 lines.
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    //
    // Sets the SLT[7:0] register to 0x99.
    // The width of the scan line time is
    //     4 * SLT[7:0] = 4 * 153 = 612 oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    //
    // Sets the VFP[7:0] register to 0x06.
    // The vertical front porch is 6.
    // TODO(https://fxbug.dev/321897820): This doesn't match the display panel
    // timing parameters.
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x06,
    //
    // Sets the VBP[7:0] register to 0x09.
    // The vertical back porch plus the vertical sync width is 9.
    // TODO(https://fxbug.dev/321897820): This doesn't match the display panel
    // timing parameters.
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x09,
    //
    // Sets the HBP[7:0] register to 0x3c.
    // The horizontal back porch plus the horizontal sync width is 60.
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x3c,
    //
    // Sets the bits 15-8 of the TCON_OPT1 register to 0x04.
    // The detailed bit definitions are not available.
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x04,

    // DCDC_SEL (Power Mode and Charge Pump Settings) - User page 1, 0x55-0x5c
    //
    // Configures the DC / DC converter.
    //
    // JD9365 user guide Section 2.7.13, pages 47-49
    //
    // Sets the DCDCM[3:0] register to 0b0010.
    // Selects the power mode for the positive analog supply voltage (AVDD),
    // negative analog supply voltage (AVEE) and clamped negative supply
    // voltage (VCL).
    //
    // AVDD and AVEE are provided by the NT (Novatek) power IC and VCL is
    // provided by the internal charge pump (CP).
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x02,
    //
    // Sets the AUTO_RT register to false.
    // The auto pumping ratio function is disabled. The charge pump circuit
    // will not detect voltage of VCI to select suitable ratio for the charge
    // pump circuit.
    //
    // Sets the AVDD_RT[1:0] register to 0x1.
    // If the BOOSTM[1:0] input pins are 0b00, the charge pump ratio of the
    // positive analog supply voltage output (AVDD) is 2.0 x VCIP, where VCIP
    // is the DC/DC setup supply.
    //
    // The JD9365 user guide (page 47) states that AVDD uses the internal
    // charge pump when BOOSTM is 0b00; however, the JD9365D datasheet
    // (page 15) states that when BOOSTM is 0b00, AVEE is under the external
    // power mode, which conflicts with the user guide.
    //
    // The TV070WSM spec doesn't mention the pin configuration of the DDIC, so
    // it's unknown whether this configuration register is effective.
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    //
    // Sets the VGH_RT[2:0] register to 0x4.
    // The charge pump ratio of the positive gate driver voltage (VGH) is
    // 3 * AVDD - VCL.
    //
    // Sets the VGL_RT[2:0] register to 0x2.
    // The charge pump ratio the negative gate driver voltage (VGL) is
    // AVEE + VCL - AVDD.
    //
    // Sets the VCL_RT[1:0] register to 0x1.
    // The charge pump ratio of the clamped negative analog supply voltage (VCL)
    // to -VCIP.
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x89,
    //
    // Sets the AVDD[4:0] register to 0x0a.
    // Clamps the positive analog supply voltage output (AVDD) to
    // 6.5 V - 0x0a * 100mV = 5.5 V.
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    //
    // Sets the AVEE[4:0] register to 0x0a.
    // Clamps the negative analog supply voltage output (AVEE) to
    // -6.5 V + 0x0a * 100mV = -5.5 V.
    //
    // Sets the VCL[2:0] register to 0x00.
    // Clamps the clamped negative analog supply voltage output (VCL) to
    // -2.5 V + 0x00 * 200mV = -2.5 V.
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    //
    // Sets the VGH[6:0] register to 0x27.
    // Clamps the positive gate driver voltage (VGH) to
    // 7.0 V + 0x27 * 200mV = 14.8 V.
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x27,
    //
    // Sets the VGL[5:0] register to 0x15.
    // Clamps the negative gate driver voltage (VGL) to
    // -7.0 V - 0x15 * 200mV = -11.2 V.
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x15,

    // SET_GAMMA (Set Gamma Output Voltage) - User page 1, 0x5d-0x82
    //
    // Configures the gamma table to convert 8-bit RGB values to the amplitude
    // of grayscale voltages.
    //
    // JD9365 user guide Section 2.7.14, pages 50-51
    //
    // The following registers specify the amplitude of the reference outputs
    // of the positive polarity grayscale voltages (VOP) for 18 predefined RGB
    // input values, by adjusting the variable resistors.
    //
    // RPA18 / VPR18.
    // Sets the variable resistor for the reference grayscale voltage output on
    // positive polarities (VOP) for input of 255:
    // VOP255 = (360 - 128 + VPR18) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.989 * VGMP + 0.011 * VGSP
    // where VGMP is the power source voltage for positive polarities, and
    // VGSP is the reference voltage for positive polarities (ditto for the
    // rest of the definitions).
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x7c,
    //
    // RPA17 / VPR17.
    // Sets the variable resistor for the VOP for input of 251:
    // VOP251 = (360 - 128 + VPR17) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.925 * VGMP + 0.075 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x65,
    //
    // RPA16 / VPR16.
    // Sets the variable resistor for the VOP for input of 247:
    // VOP247 = (360 - 128 + VPR16) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.875 * VGMP + 0.125 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x53,
    //
    // RPA15 / VPR15.
    // Sets the variable resistor for the VOP for input of 243:
    // VOP243 = (360 - 128 + VPR15) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.839 * VGMP + 0.161 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x46,
    //
    // RPA14 / VPR14.
    // Sets the variable resistor for the VOP for input of 235:
    // VOP235 = (344 - 128 + VPR14) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.781 * VGMP + 0.219 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x41,
    //
    // RPA13 / VPR13.
    // Sets the variable resistor for the VOP for input of 227:
    // VOP227 = (344 - 128 + VPR13) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.742 * VGMP + 0.258 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x33,
    //
    // RPA12 / VPR12.
    // Sets the variable resistor for the VOP for input of 211:
    // VOP211 = (316 - 128 + VPR12) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.669 * VGMP + 0.331 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x35,
    //
    // RPA11 / VPR11.
    // Sets the variable resistor for the VOP for input of 191:
    // VOP191 = (316 - 128 + VPR11) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.606 * VGMP + 0.394 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x1e,
    //
    // RPA10 / VPR10.
    // Sets the variable resistor for the VOP for input of 159:
    // VOP159 = (264 - 128 + VPR10) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.528 * VGMP + 0.472 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x36,
    //
    // RPA9 / VPR9.
    // Sets the variable resistor for the VOP for input of 128:
    // VOP128 = (244 - 128 + VPR9) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.467 * VGMP + 0.533 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x34,
    //
    // RPA8 / VPR8.
    // Sets the variable resistor for the VOP for input of 96:
    // VOP96 = (224 - 128 + VPR8) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.411 * VGMP + 0.589 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x34,
    //
    // RPA7 / VPR7.
    // Sets the variable resistor for the VOP for input of 64:
    // VOP64 = (172 - 128 + VPR7) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.347 * VGMP + 0.653 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x51,
    //
    // RPA6 / VPR6.
    // Sets the variable resistor for the VOP for input of 44:
    // VOP44 = (172 - 128 + VPR6) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.294 * VGMP + 0.706 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x3e,
    //
    // RPA5 / VPR5.
    // Sets the variable resistor for the VOP for input of 28:
    // VOP28 = (144 - 128 + VPR5) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.233 * VGMP + 0.767 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x44,
    //
    // RPA4 / VPR4.
    // Sets the variable resistor for the VOP for input of 20:
    // VOP20 = (144 - 128 + VPR4) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.192 * VGMP + 0.809 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x35,
    //
    // RPA3 / VPR3.
    // Sets the variable resistor for the VOP for input of 12:
    // VOP12 = VPR3 / 360 * (VGMP - VGSP) + VGSP
    //       = 0.128 * VGMP + 0.872 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x2e,
    //
    // RPA2 / VPR2.
    // Sets the variable resistor for the VOP for input of 8:
    // VOP8 = VPR2 / 360 * (VGMP - VGSP) + VGSP
    //      = 0.086 * VGMP + 0.914 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x1f,
    //
    // RPA1 / VPR1.
    // Sets the variable resistor for the VOP for input of 4:
    // VOP4 = VPR1 / 360 * (VGMP - VGSP) + VGSP
    //      = 0.033 * VGMP + 0.967 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x0c,
    // RPA0 / VPR0.
    // Sets the variable resistor for the VOP for input of 0:
    // VOP0 = VPR0 / 360 * (VGMP - VGSP) + VGSP
    //      = 0 * VGMP + 1 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x00,

    // The following registers specify the amplitude of the reference outputs
    // of the negative polarity grayscale voltages (VON) for 18 predefined RGB
    // input values, by adjusting the variable resistors.
    //
    // For this panel, the gamma values defined for negative polarities are the
    // same as  the gamma values for positive polarities.
    //
    // RNA18 / VNR18.
    // Sets the variable resistor for the reference grayscale voltage output on
    // negative polarities (VON) for input of 255:
    // VON255 = (360 - 128 + VNR18) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.989 * VGMN + 0.011 * VGSN
    // where VGMN is the power source voltage for negative polarities, and
    // VGSN is the reference voltage for negative polarities (ditto for the
    // rest of the definitions).
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x7c,
    //
    // RNA17 / VNR17.
    // Sets the variable resistor for the VON for input of 251:
    // VON251 = (360 - 128 + VNR17) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.925 * VGMN + 0.075 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x65,
    //
    // RNA16 / VNR16.
    // Sets the variable resistor for the VON for input of 247:
    // VON247 = (360 - 128 + VNR16) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.875 * VGMN + 0.125 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x53,
    //
    // RNA15 / VNR15.
    // Sets the variable resistor for the VON for input of 243:
    // VON243 = (360 - 128 + VNR15) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.839 * VGMN + 0.161 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x46,
    //
    // RNA14 / VNR14.
    // Sets the variable resistor for the VON for input of 235:
    // VON235 = (344 - 128 + VNR14) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.781 * VGMN + 0.219 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x41,
    //
    // RNA13 / VNR13.
    // Sets the variable resistor for the VON for input of 227:
    // VON227 = (344 - 128 + VNR13) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.742 * VGMN + 0.258 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x33,
    //
    // RNA12 / VNR12.
    // Sets the variable resistor for the VON for input of 211:
    // VON211 = (316 - 128 + VNR12) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.669 * VGMN + 0.331 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x35,
    //
    // RNA11 / VNR11.
    // Sets the variable resistor for the VON for input of 191:
    // VON191 = (316 - 128 + VNR11) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.606 * VGMN + 0.394 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x1e,
    //
    // RNA10 / VNR10.
    // Sets the variable resistor for the VON for input of 159:
    // VON159 = (264 - 128 + VNR10) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.528 * VGMN + 0.472 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x36,
    //
    // RNA9 / VNR9.
    // Sets the variable resistor for the VON for input of 128:
    // VON128 = (244 - 128 + VNR9) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.467 * VGMN + 0.533 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x34,
    //
    // RNA8 / VNR8.
    // Sets the variable resistor for the VON for input of 96:
    // VON96 = (224 - 128 + VNR8) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.411 * VGMN + 0.589 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x34,
    // RNA7 / VNR7.
    // Sets the variable resistor for the VON for input of 64:
    // VON64 = (172 - 128 + VNR7) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.347 * VGMN + 0.653 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x51,
    //
    // RNA6 / VNR6.
    // Sets the variable resistor for the VON for input of 44:
    // VON44 = (172 - 128 + VNR6) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.294 * VGMN + 0.706 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x3e,
    //
    // RNA5 / VNR5.
    // Sets the variable resistor for the VON for input of 28:
    // VON28 = (144 - 128 + VNR5) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.233 * VGMN + 0.767 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x44,
    //
    // RNA4 / VNR4.
    // Sets the variable resistor for the VON for input of 20:
    // VON20 = (144 - 128 + VNR4) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.192 * VGMN + 0.809 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x35,
    //
    // RNA3 / VNR3.
    // Sets the variable resistor for the VON for input of 12:
    // VON12 = VNR3 / 360 * (VGMN - VGSN) + VGSN
    //       = 0.128 * VGMN + 0.872 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x2e,
    //
    // RNA2 / VNR2.
    // Sets the variable resistor for the VON for input of 8:
    // VON8 = VNR2 / 360 * (VGMN - VGSN) + VGSN
    //      = 0.086 * VGMN + 0.914 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x1f,
    //
    // RNA1 / VNR1.
    // Sets the variable resistor for the VON for input of 4:
    // VON4 = VNR1 / 360 * (VGMN - VGSN) + VGSN
    //      = 0.033 * VGMN + 0.967 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x0c,
    //
    // RNA0 / VNR0.
    // Sets the variable resistor for the VON for input of 0:
    // VON0 = VNR0 / 360 * (VGMN - VGSN) + VGSN
    //      = 0 * VGMN + 1 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 2.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,

    // SET_GIP_L (SET CGOUTx_L Signal Mapping, GS_Panel=0) - User page 2, 0x00-0x15
    //
    // Maps the timing controller output signals to gate-in-panel control output
    // pins on the left side of panel. Effective iff the gate scan output is
    // top-to-bottom.
    //
    // JD9365 user guide Section 2.8.1, page 53
    //
    // There are 22 gate-in-panel (GIP) control output (CGOUT) pins for the
    // left side of the panel, named CGOUT(1-22)_L. The following registers map
    // the output signals to CGOUT pins.
    //
    // Because SETPANEL sets the scan direction to top-to-bottom, the following
    // configurations are effective.
    //
    // CGOUT1_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV1 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x45,
    //
    // CGOUT2_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV1 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x45,
    //
    // CGOUT3_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV3 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x47,
    //
    // CGOUT4_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV3 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x47,
    //
    // CGOUT5_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV1 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x41,
    //
    // CGOUT6_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV1 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x41,
    //
    // CGOUT7_L, ..., CGOUT13_L: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    //
    // CGOUT14_L, CGOUT15_L, CGOUT16_L: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Floating (no output mapping).
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x1d,
    //
    // CGOUT17_L, ..., CGOUT22_L: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,

    // SET_GIP_R (SET CGOUTx_R Signal Mapping, GS_Panel=0) - User page 2, 0x16-0x2b
    //
    // Maps the timing controller output signals to gate-in-panel control output
    // pins on the right side of panel. Effective iff the gate scan output is
    // top-to-bottom.
    //
    // JD9365 user guide Section 2.8.2, page 54
    //
    // There are 22 Gate-in-panel (GIP) control output (CGOUT) pins for the
    // right side of the panel, named CGOUT(1-22)_R. The following registers map
    // the output signals to CGOUT pins.
    //
    // Because SETPANEL sets the scan direction to top-to-bottom, the following
    // configurations are effective.
    //
    // CGOUT1_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV0 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x44,
    //
    // CGOUT2_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV0 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x44,
    //
    // CGOUT3_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV2 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x46,
    //
    // CGOUT4_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV2 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x46,
    //
    // CGOUT5_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV0 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x40,
    //
    // CGOUT6_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV0 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x40,
    //
    // CGOUT7_R, ..., CGOUT13_R: Pulls to VGH on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    //
    // CGOUT14_R, CGOUT15_R, CGOUT16_R: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Floating (no output mapping).
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x1d,
    //
    // CGOUT17_R, ..., CGOUT22_R: Pulls to VGH on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x1f,

    // SETGIP1 (Set GIP Signal Timing 1) - User page 2, 0x58-0x6b
    //
    // Configures the gate-in-panel (GIP) signal timing.
    //
    // JD9365 user guide Section 2.8.5, pages 57, 59-63
    //
    // Sets GIP_GAS_OPT to true.
    // On abnormal power off, CGOUT signals are pulled to voltages specified in
    // SET_GIP_{L,R}.
    //
    // Sets INIT_PORCH[3:0] to 0.
    // The initialization signal porch size is 0 frames, i.e. no panel
    // initialization signal is emitted.
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    //
    // Sets INIT_W[3:0] to 0.
    // The initialization signal frame width is 0 frames.
    // Because no panel initialization signal is emitted, this is not effective.
    //
    // Sets INIT[10:0] to 0.
    // The initialization signal line width is 0 lines.
    // Because no panel initialization signal is emitted, this is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    //
    // Sets STV_NUM[1:0] to 0x1.
    // Enables 2 vertical start pulse signals: STV0 and STV1.
    //
    // Sets STV_S0[10:0] to 0x007.
    // Sets the initial phase of the first vertical start pulse signal (STV0).
    // The first vertical start pulse signal (STV0) is emitted 1 + 7 = 8 lines
    // after the beginning of a vertical sync pulse.
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x07,
    //
    // Sets STV_W[3:0] to 0x2.
    // The duration (width) of all the vertical start pulse signals are
    //   1 + STV_W[3:0] = 1 + 2 = 3 lines.
    //
    // Sets STV_S1[2:0] to 0.
    // The phase difference between vertical start pulse signals STV0 and STV1
    // is 0 + 1 = 1 line.
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x20,
    //
    // Sets STV_S2[4:0] to 0.
    // The phase difference between vertical start pulse signals STV0 and STV2
    // is 0 + 1 = 1 line.
    // The STV2 signal is disabled, thus this register configuration won't take
    // effect.
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    //
    // Sets STV_S3[4:0] to 0.
    // The phase difference between vertical start pulse signals STV0 and STV3
    // is 0 + 1 = 1 line.
    // The STV3 signal is disabled, thus this register configuration won't take
    // effect.
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x00,
    //
    // Sets ETV_S2[4:0] to 0.
    // The phase difference between vertical end pulse signals EVT0 and EVT2
    // is 0 + 1 = 1 line.
    // The ETV (vertical end pulse) signals are not used by the panel.
    // This register configuration won't take effect.
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x00,
    //
    // Sets ETV_S3[4:0] to 0.
    // The phase difference between vertical end pulse signals EVT0 and EVT3
    // is 0 + 1 = 1 line.
    // The ETV (vertical end pulse) signals are not used. This register
    // configuration won't take effect.
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x00,
    //
    // Sets SETV_ON[7:0] to 0x7a.
    // Fine-tunes the starting point of the STV / ETV signals.
    //
    // The duration between the end of a horizontal sync pulse and the
    // beginning of the vertical start (end) pulse right after it is 0x7a = 122
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    //
    // Sets SETV_OFF[7:0] to 0x7a.
    // Fine-tunes the end point of the STV / ETV signals.
    //
    // The duration between the end of the vertical start (end) pulse and the
    // end of the horizontal sync pulse right before it is 0x7a = 122
    // oscillator periods.
    //
    // For example, in the diagram below, the HSYNC signal is falling edge
    // triggered, and the STV signal is rising edge triggered.
    // The duration A stands for SETV_ON, and the duration B stands for SETV_OFF.
    //
    // HSYNC ---__-----------__------------__------------__
    // STV   ____________---------------------------___
    //            |<-A->|                    |<-B->|
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    //
    // Registers 0x65 and 0x66 configure the vertical end pulse (ETV) signals.
    // Since the ETV signals are not provided to the panel, the configuration
    // registers are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x00,
    //
    // Sets CKV0_NUM[3:0] to 3.
    // Enables 4 vertical clock (CKV) signals on the CKV signal group 0:
    // CKV0, CKV1, CKV2 and CKV3.
    //
    // Sets CKV0_W[2:0] to 2.
    // The duration (width) of a vertical clock (CKV) signal is
    // 1 + CKV0_W[2:0] = 1 + 2 = 3 lines.
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x32,
    //
    // Sets CKV0_S0[7:0] to 8.
    // Sets the initial phase of the first vertical clock signal (CKV0).
    // The first vertical clock signal (CKV0) is emitted 1 + 8 = 9 lines after
    // the beginning of a vertical sync pulse.
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x08,
    //
    // Sets CKV0_ON[7:0] to 0x7a.
    // Fine-tunes the starting point of the CKV signals.
    //
    // The duration between the end of a horizontal sync pulse and the
    // beginning of the vertical clock (CKV) pulse right after it is 0x7a = 122
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    //
    // Sets CKV0_OFF[7:0] to 0x7a.
    // Fine-tunes the end point of the CKV signals.
    //
    // The duration between the end of the vertical clock (CKV) pulse and the
    // end of the horizontal sync pulse right before it is 0x7a = 122
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    //
    // Sets CKV0_DUM[7:0] to 0.
    // If CKV0_CON is zero, a total amount of CKV0_DUM[7:0] placeholder
    // vertical clock (CKV) pulses are emitted after the active lines every
    // frame. In this case, if CKV0_CON is zero, no placeholder vertical clock
    // pulses will be emitted after active lines.
    //
    // On the current configuration, CKV0_CON is 1, thus this register
    // is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x00,

    // SETGIP2 (Set GIP Signal Timing 2) - User page 2, 0x6c-0x7e
    //
    // Configures the gate-in-panel (GIP) signal timing.
    //
    // JD9365 user guide Section 2.8.6, pages 58-63
    //
    // Sets EOLR, GEQ_LINE and GEQ_W[3:0] to 0.
    // The above registers are not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    //
    // Sets GEQ_GGND1[5:0] to 0x04.
    // The duration of the equalization period to turn on a gate GIP signal is
    // 4 oscillator clocks.
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    //
    // Sets GEQ_GGND2[5:0] to 0x04.
    // The duration of the equalization period to turn off a gate GIP signal is
    // 4 oscillator clocks.
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x04,
    //
    // Sets GIPDR[1:0] = 0b10, VGHO_SEL = 0, VGLO_SEL = 0, VGLO_SEL2 = 1,
    // CKV_GROUP = 0.
    // The registers above are not documented in the JD9365 user guide.
    // JD9364 user guide states that CKV_GROUP = 0 indicates that all CKV
    // signals belong to the CKV signal group 0. CKV1_* registers will not be
    // effective.
    //
    // Sets CKV0_CON = 1.
    // Overrides the CKV0_DUM configuration. CKV signals CKV0-CKV3 always output
    // on blanking areas.
    //
    // Sets CKV1_CON = 0.
    // Does not override the CKV1_DUM configuration. CKV1 signals are not
    // enabled, thus this register is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x89,
    //
    // Sets CKV1_NUM[3:0] = 0.
    // No signals from the CKV signal group 1 is enabled.
    //
    // Sets CKV1_W[3:0] = 0.
    // The width (duration) of CKV1 signals is 0 + 1 = 1 line. No CKV1 signal
    // is enabled, so this register is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    //
    // Registers 0x70-0x74 further configure the vertical clock signals of the
    // CKV signal group 1.
    // Since these CKV signals are not enabled, the configuration registers
    // are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    //
    // Sets FLM_EN to false.
    // Disables the generation of the first line marker signal (FLM).
    //
    // Other fields on registers 0x75-0x78 further configure the first line
    // marker signal (FLM). Since the FLM signal is not enabled, the
    // configuration registers are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    //
    // Sets VEN_EN to false.
    // Disables the generation of the VEN (Vertical enabled?) signal.
    //
    // Other fields on registers 0x77, 0x79-0x7e further configure the VEN
    // signal. Since the VEN signal is not enabled, the configuration
    // registers are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x5d,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 3.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,

    // The addresses 0xa9 and 0xac are not documented in the JD9365 datasheet
    // nor the JD9365 user guide.
    kMipiDsiDtGenShortWrite2, 2, 0xa9, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xac, 0x4d,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 4.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,

    // The addresses 0x00, 0x02 and 0x09 are not documented in the JD9365
    // datasheet nor the JD9365 user guide.
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0xb3,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x60,

    // SETSTBA2 (Set IF Source Switch Control Timing) - User page 4, 0x0c-0x0f
    //
    // JD9365 user guide Section 2.10.1, page 71
    //
    // Sets SDS[14:13] to 0b10.
    // The PEQ power source is PCAP.
    //
    // Sets SDS[11] to 0.
    // The NEQ power source is NCAP.
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 0.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,

    // WRDISBV (Write Display Brightness) - MIPI DCS command 0x51
    //
    // Adjusts the brightness value of the display.
    //
    // JD9365 datasheet Section 10.2.35, page 164
    //
    // Sets the display brightness value to 255.
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,

    // WRCTRLD (Write CTRL Display) - MIPI DCS command 0x53
    //
    // Configures the brightness / backlight control blocks.
    //
    // JD9365 datasheet Section 10.2.37, page 166
    //
    // Sets BCTRL to true.
    // Enables the brightness control block.
    //
    // Sets DD to true.
    // Enables display dimming.
    //
    // Sets BL to true.
    // Enables the backlight control circuit.
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,

    // Sleeps for 5 milliseconds.
    kDsiOpSleep, 5,

    // SLPOUT (Exit Sleep In Mode) - MIPI DCS command 0x11
    //
    // Turns off the sleep mode, enables the DC/DC converter, and starts
    // the internal oscillator and the panel scanning procedure.
    //
    // JD9365 datasheet Section 10.2.16, page 143
    kMipiDsiDtDcsShortWrite0, 1, 0x11,

    // Sleeps for 125 milliseconds.
    //
    // BOE TV070WSM-TG1 spec states that the interval between the MIPI-DSI
    // initialization code and the high-speed mode should be greater than 120
    // milliseconds.
    kDsiOpSleep, 125,

    // DISPON (Display On) - MIPI DCS command 0x29
    //
    // Enables output from the frame memory to the display panel.
    //
    // JD9365 datasheet Section 10.2.24, page 152
    kMipiDsiDtDcsShortWrite0, 1, 0x29,

    // TODO(https://fxbug.dev/321897820): We do not explicitly set the power
    // of the LEDs. The power sequence might be invalid for the LCDs.
    // BOE TV070WSM-TG1 spec states that the interval between the display
    // entering the high-speed mode and LED being turned on should be greater
    // than 35 milliseconds.

    // Sleeps for 20 milliseconds.
    kDsiOpSleep, 20,

    // End marker.
    kDsiOpSleep, 0xff,
};

// clang-format on

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_PANEL_BOE_TV070WSM_FITIPOWER_JD9365_H_
