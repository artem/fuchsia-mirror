// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_TGL_REGISTERS_DPLL_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_TGL_REGISTERS_DPLL_H_

#include <zircon/assert.h>

#include <cstdint>

#include <hwreg/bitfields.h>

#include "src/graphics/display/drivers/intel-i915-tgl/hardware-common.h"

namespace tgl_registers {

// DPLL_CTRL1 (Display PLL Control 1?)
//
// Some of this register's reserved fields are not MBZ (must be zero). So, the
// register can only be updated safely via read-modify-write operations.
//
// This register is not documented on Tiger Lake or DG1.
//
// Kaby Lake: IHD-OS-KBL-Vol 2c-1.17 Part 1 pages 528-531
// Skylake: IHD-OS-SKL-Vol 2c-05.16 Part 1 pages 526-529
class DisplayPllControl1 : public hwreg::RegisterBase<DisplayPllControl1, uint32_t> {
 public:
  DEF_RSVDZ_FIELD(31, 28);

  // Documented values for the `pll_display_port_ddi_frequency_select` fields.
  enum class DisplayPortDdiFrequencySelect : int {
    k2700Mhz = 0b000,  // DP HBR2. Lane clock 5.4 GHz. VCO 8100, divider 6.
    k1350Mhz = 0b001,  // DP HBR1. Lane clock 2.7 GHz. VCO 8100, divider 3.
    k810Mhz = 0b010,   // DP RBR. Lane clock 1.62 GHz. VCO 8100, divider 10.
    k1620Mhz = 0b011,  // eDP rate 5. Lane clock 3.24 GHz. VCO 8100, divider 5.
    k1080Mhz = 0b100,  // eDP rate 2. Lane clock 2.16 GHz. VCO 8640, divider 8.
    k2160Mhz = 0b101,  // eDP rate 6. Lane clock 4.32 GHz. VCO 8640, divider 4.

    // TODO(fxbug.dev/110690): Figure out modeling for invalid values.
  };

  DEF_BIT(23, pll3_uses_hdmi_configuration_mode);
  DEF_BIT(22, pll3_spread_spectrum_clocking_enabled);
  DEF_ENUM_FIELD(DisplayPortDdiFrequencySelect, 21, 19, pll3_display_port_ddi_frequency_select);
  DEF_BIT(18, pll3_programming_enabled);

  DEF_BIT(17, pll2_uses_hdmi_configuration_mode);
  DEF_BIT(16, pll2_spread_spectrum_clocking_enabled);
  DEF_ENUM_FIELD(DisplayPortDdiFrequencySelect, 15, 13, pll2_display_port_ddi_frequency_select);
  DEF_BIT(12, pll2_programming_enabled);

  DEF_BIT(11, pll1_uses_hdmi_configuration_mode);
  DEF_BIT(10, pll1_spread_spectrum_clocking_enabled);
  DEF_ENUM_FIELD(DisplayPortDdiFrequencySelect, 9, 7, pll1_display_port_ddi_frequency_select);
  DEF_BIT(6, pll1_programming_enabled);

  DEF_ENUM_FIELD(DisplayPortDdiFrequencySelect, 3, 1, pll0_display_port_ddi_frequency_select);
  DEF_BIT(0, pll0_programming_enabled);

  // If true, the Display PLL is configured for HDMI operation.
  //
  // If this field is true, the PLL uses the configuration in the DPLL*_CFGCR*
  // registers. The PLL will generate AFE (Analog Front-End) clock frequencies
  // suitable for use with DDIs that serve HDMI connections. HDMI operation does
  // not support SSC (Spread Spectrum Clocking).
  //
  // If this field is false, the PLL is configured for DisplayPort operation,
  // which uses the frequency and SSC configuration in this register. The PLL's
  // AFE clock output frequencies will be suitable for use with DDIs that serve
  // DisplayPort connections.
  //
  // This helper always returns false on DPLL0. The underlying field does not
  // exist for Display PLL0, because PLL0 does not support HDMI operation.
  bool pll_uses_hdmi_configuration_mode(Dpll dpll) const {
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    if (dpll == Dpll::DPLL_0) {
      return false;  // DPLL 0 does not support HDMI operation.
    }

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6 + 5;
    return static_cast<bool>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), bit_index, bit_index).get());
  }

  // See `pll_uses_hdmi_configuration_mode()` for details.
  DisplayPllControl1& set_pll_uses_hdmi_configuration_mode(Dpll dpll, bool hdmi_mode) {
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    if (dpll == Dpll::DPLL_0) {
      ZX_DEBUG_ASSERT(!hdmi_mode);
      return *this;
    }

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6 + 5;
    hwreg::BitfieldRef<uint32_t>(reg_value_ptr(), bit_index, bit_index).set(hdmi_mode ? 1 : 0);
    return *this;
  }

  // If true, the Display PLL uses SSC (Spread Spectrum Clocking).
  //
  // This helper always return false for DPLL (Display PLL) 0. The underlying
  // field does not exist for DPLL0. DPLL0 does not support SSC, because it must
  // deliver a constant frequency to the core display clock.
  bool pll_spread_spectrum_clocking_enabled(Dpll dpll) const {
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    if (dpll == Dpll::DPLL_0) {
      return false;  // DPLL 0 does not support SSC (Spread Spectrum Clocking).
    }

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6 + 4;
    return static_cast<bool>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), bit_index, bit_index).get());
  }

  // See `pll_spread_spectrum_clocking_enabled()` for details.
  DisplayPllControl1& set_pll_spread_spectrum_clocking_enabled(Dpll dpll, bool ssc_enabled) {
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    if (dpll == Dpll::DPLL_0) {
      ZX_DEBUG_ASSERT(!ssc_enabled);
      return *this;
    }

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6 + 4;
    hwreg::BitfieldRef<uint32_t>(reg_value_ptr(), bit_index, bit_index).set(ssc_enabled ? 1 : 0);
    return *this;
  }

  // The Display PLL's DDI clock frequency, when operating in DisplayPort mode.
  //
  // This field sets the AFE (Analog Front-End) clock for the DPLL (Display
  // PLL), when the DPLL is operating in DisplayPort Mode. The AFE clock
  // dictates the frequency of the DDIs that use this DPLL As their clocking
  // source.
  //
  // When a DDI serves a DisplayPort connection, it pushes bits on both clock
  // edges (rising and falling). So, the AFE clock frequency (which becomes the
  // DDI's clock frequency) must be set to half the DisplayPort bit rate. For
  // example, a 2,700 MHz frequency would be used for the HBR2 link rate, which
  // is 5.4 Gbit/s.
  //
  // This field is ignored if the DPLL is not operating in DisplayPort mode.
  //
  // The frequency of DPLL0 indirectly impacts the CDCLK (core display clock)
  // frequency. The PLL's VCO (voltage-controlled oscillator) frequency will be
  // either 8,640 Mhz or 8,100 MHz, subject to the constraint that the
  // DisplayPort frequency must evenly divide the VCO frequency.
  //
  // This helper returns 0 if the field is set to an undocumented value.
  int16_t pll_display_port_ddi_frequency_mhz(Dpll dpll) const {
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6 + 1;
    const int raw_frequency_select = static_cast<int>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), bit_index + 2, bit_index).get());

    const auto frequency_select = static_cast<DisplayPortDdiFrequencySelect>(raw_frequency_select);
    switch (frequency_select) {
      case DisplayPortDdiFrequencySelect::k2700Mhz:
        return 2'700;
      case DisplayPortDdiFrequencySelect::k1350Mhz:
        return 1'350;
      case DisplayPortDdiFrequencySelect::k810Mhz:
        return 810;
      case DisplayPortDdiFrequencySelect::k1620Mhz:
        return 1'620;
      case DisplayPortDdiFrequencySelect::k1080Mhz:
        return 1'080;
      case DisplayPortDdiFrequencySelect::k2160Mhz:
        return 2'160;
    }
    return 0;  // The field is set to an undocumented value.
  }

  // See `pll_display_port_ddi_frequency_mhz()` for details.
  DisplayPllControl1& set_pll_display_port_ddi_frequency_mhz(Dpll dpll, int16_t ddi_frequency_mhz) {
    DisplayPortDdiFrequencySelect frequency_select;
    switch (ddi_frequency_mhz) {
      case 2'700:
        frequency_select = DisplayPortDdiFrequencySelect::k2700Mhz;
        break;
      case 1'350:
        frequency_select = DisplayPortDdiFrequencySelect::k1350Mhz;
        break;
      case 810:
        frequency_select = DisplayPortDdiFrequencySelect::k810Mhz;
        break;
      case 1'620:
        frequency_select = DisplayPortDdiFrequencySelect::k1620Mhz;
        break;
      case 1'080:
        frequency_select = DisplayPortDdiFrequencySelect::k1080Mhz;
        break;
      case 2'160:
        frequency_select = DisplayPortDdiFrequencySelect::k2160Mhz;
        break;
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid DDI clock frequency: %d Mhz", ddi_frequency_mhz);
        frequency_select = DisplayPortDdiFrequencySelect::k2700Mhz;
    }

    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6 + 1;
    hwreg::BitfieldRef<uint32_t>(reg_value_ptr(), bit_index + 2, bit_index)
        .set(static_cast<uint32_t>(frequency_select));
    return *this;
  }

  // If true, the Display PLL uses the configuration in this register.
  bool pll_programming_enabled(Dpll dpll) const {
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6;
    return static_cast<bool>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), bit_index, bit_index).get());
  }

  // See `pll_programming_enabled()` for details.
  DisplayPllControl1& set_pll_programming_enabled(Dpll dpll, bool programming_enabled) {
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    const int dpll_index = dpll - Dpll::DPLL_0;
    const int bit_index = dpll_index * 6;
    hwreg::BitfieldRef<uint32_t>(reg_value_ptr(), bit_index, bit_index)
        .set(programming_enabled ? 1 : 0);
    return *this;
  }

  static auto Get() { return hwreg::RegisterAddr<DisplayPllControl1>(0x6c058); }
};

// DPLL_CTRL2 (Display PLL Control 2?)
//
// This register controls which DPLL (Display PLL) is used as a clock source by
// each DDI.
//
// Some of this register's reserved fields are not MBZ (must be zero). So, the
// register can only be updated safely via read-modify-write operations.
//
// The Tiger Lake equivalent of this register is DPCLKA_CFGCR0.
//
// Kaby Lake: IHD-OS-KBL-Vol 2c-1.17 Part 1 pages 532-534
// Skylake: IHD-OS-SKL-Vol 2c-05.16 Part 1 pages 530-532
class DisplayPllDdiMapKabyLake : public hwreg::RegisterBase<DisplayPllDdiMapKabyLake, uint32_t> {
 public:
  DEF_RSVDZ_FIELD(31, 24);

  DEF_BIT(19, ddi_e_clock_disabled);
  DEF_BIT(18, ddi_d_clock_disabled);
  DEF_BIT(17, ddi_c_clock_disabled);
  DEF_BIT(16, ddi_b_clock_disabled);
  DEF_BIT(15, ddi_a_clock_disabled);

  DEF_FIELD(14, 13, ddi_e_clock_display_pll_index);
  DEF_BIT(12, ddi_e_clock_programming_enabled);

  DEF_FIELD(11, 10, ddi_d_clock_display_pll_index);
  DEF_BIT(9, ddi_d_clock_programming_enabled);

  DEF_FIELD(8, 7, ddi_c_clock_display_pll_index);
  DEF_BIT(6, ddi_c_clock_programming_enabled);

  DEF_FIELD(5, 4, ddi_b_clock_display_pll_index);
  DEF_BIT(3, ddi_b_clock_programming_enabled);

  DEF_FIELD(2, 1, ddi_a_clock_display_pll_index);
  DEF_BIT(0, ddi_a_clock_programming_enabled);

  // If true, the DDI's clock is disabled. This is accomplished by gating.
  bool ddi_clock_disabled(Ddi ddi) const {
    ZX_ASSERT(ddi >= Ddi::DDI_A);
    ZX_ASSERT(ddi <= Ddi::DDI_E);

    const int ddi_index = ddi - Ddi::DDI_A;
    const int bit_index = 15 + ddi_index;
    return static_cast<bool>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), bit_index, bit_index).get());
  }

  // See `ddi_clock_disabled()` for details.
  DisplayPllDdiMapKabyLake& set_ddi_clock_disabled(Ddi ddi, bool clock_disabled) {
    ZX_ASSERT(ddi >= Ddi::DDI_A);
    ZX_ASSERT(ddi <= Ddi::DDI_E);

    const int ddi_index = ddi - Ddi::DDI_A;
    const int bit_index = 15 + ddi_index;
    hwreg::BitfieldRef<uint32_t>(reg_value_ptr(), bit_index, bit_index).set(clock_disabled ? 1 : 0);
    return *this;
  }

  // The DPLL (Display PLL) used as a clock source for a DDI.
  Dpll ddi_clock_display_pll(Ddi ddi) const {
    ZX_ASSERT(ddi >= Ddi::DDI_A);
    ZX_ASSERT(ddi <= Ddi::DDI_E);

    const int ddi_index = ddi - Ddi::DDI_A;
    const int bit_index = ddi_index * 3 + 1;
    const uint32_t dpll_index = static_cast<int>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), bit_index + 1, bit_index).get());
    // The cast result is DPLL0-3 because `dpll_index` comes from a 2-bit field.
    return static_cast<Dpll>(dpll_index);
  }

  // See `ddi_clock_display_pll()` for details.
  DisplayPllDdiMapKabyLake& set_ddi_clock_display_pll(Ddi ddi, Dpll dpll) {
    ZX_ASSERT(ddi >= Ddi::DDI_A);
    ZX_ASSERT(ddi <= Ddi::DDI_E);
    ZX_ASSERT(dpll >= Dpll::DPLL_0);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    const int ddi_index = ddi - Ddi::DDI_A;
    const int bit_index = ddi_index * 3 + 1;
    const int dpll_index = dpll - Dpll::DPLL_0;
    hwreg::BitfieldRef<uint32_t>(reg_value_ptr(), bit_index + 1, bit_index).set(dpll_index);
    return *this;
  }

  // If true, the DDI uses the clock configuration in this register.
  bool ddi_clock_programming_enabled(Ddi ddi) const {
    ZX_ASSERT(ddi >= Ddi::DDI_A);
    ZX_ASSERT(ddi <= Ddi::DDI_E);

    const int ddi_index = ddi - Ddi::DDI_A;
    const int bit_index = ddi_index * 3;
    return static_cast<bool>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), bit_index, bit_index).get());
  }

  // See `ddi_clock_programming_enabled()` for details.
  DisplayPllDdiMapKabyLake& set_ddi_clock_programming_enabled(Ddi ddi, bool programming_enabled) {
    ZX_ASSERT(ddi >= Ddi::DDI_A);
    ZX_ASSERT(ddi <= Ddi::DDI_E);

    const int ddi_index = ddi - Ddi::DDI_A;
    const int bit_index = ddi_index * 3;
    hwreg::BitfieldRef<uint32_t>(reg_value_ptr(), bit_index, bit_index)
        .set(programming_enabled ? 1 : 0);
    return *this;
  }

  static auto Get() { return hwreg::RegisterAddr<DisplayPllDdiMapKabyLake>(0x6c05c); }
};

// DPLL_CFGCR1 (Display PLL Configuration and Control Register 1?)
//
// When the DPLL (Display PLL) operates in HDMI mode, this register configures
// the frequency of the DCO (Digitally-Controlled Oscillator) in the DPLL. This
// influences the frequency that the DPLL outputs to connected DDIs.
//
// This register's reserved fields are all MBZ (must be zero). So, this register
// can be safely written without reading it first.
//
// The Tiger Lake equivalent of this register is DPLL_CFGCR0.
//
// Kaby Lake: IHD-OS-KBL-Vol 2c-1.17 Part 1 page 525
// Skylake: IHD-OS-SKL-Vol 2c-05.16 Part 1 pages 530-532
class DisplayPllDcoFrequencyKabyLake
    : public hwreg::RegisterBase<DisplayPllDcoFrequencyKabyLake, uint32_t> {
 public:
  // Kaby Lake and Skylake display engines support a single reference frequency.
  static constexpr int32_t kReferenceFrequencyKhz = 24'000;

  // The number of fractional bits in the DCO frequency multiplier.
  //
  // The DCO frequency multiplier is a fixed-point (as opposed to
  // floating-point) number. This constant represents the position of the base-2
  // equivalent of the decimal point.
  static constexpr int kMultiplierPrecisionBits = 15;

  // If true, the circuits for generating HDMI frequencies are enabled.
  //
  // This must be set when the DPLL operates in HDMI mode.
  DEF_BIT(31, frequency_programming_enabled);

  DEF_RSVDZ_FIELD(30, 24);

  // These fields have a non-trivial representation. They should be used via the
  // `dco_frequency_multiplier()` and `set_dco_frequency_multiplier()` helpers.
  DEF_FIELD(23, 9, dco_frequency_multiplier_fraction);
  DEF_FIELD(8, 0, dco_frequency_multiplier_integer);

  // The frequency multiplier for the DCO (Digitally Controlled Oscillator).
  //
  // The return value has `kMultiplierPrecisionBits` fractional bits.
  //
  // The multiplier is relative to the display engine reference frequency. On
  // Kaby Lake, this reference frequency is always `kReferenceFrequencyHz`.
  int32_t dco_frequency_multiplier() const {
    return static_cast<int32_t>(
        (static_cast<int32_t>(dco_frequency_multiplier_integer()) << kMultiplierPrecisionBits) |
        static_cast<int32_t>(dco_frequency_multiplier_fraction()));
  }

  // See `dco_frequency_multiplier()` for details.
  DisplayPllDcoFrequencyKabyLake& set_dco_frequency_multiplier(int32_t multiplier) {
    ZX_ASSERT(multiplier > 0);
    ZX_ASSERT(multiplier < (1 << 24));

    return set_dco_frequency_multiplier_fraction(multiplier & ((1 << kMultiplierPrecisionBits) - 1))
        .set_dco_frequency_multiplier_integer(multiplier >> kMultiplierPrecisionBits);
  }

  // The currently configured DCO (Digitally Controlled Oscillator) frequency.
  //
  // This is a convenience method on top of the `dco_frequency_multiplier`
  // fields.
  int32_t dco_frequency_khz() const {
    // The formulas in the PRM use truncating division when converting from a
    // frequency to a DCO multiplier. Rounding up below aims to re-constitue an
    // original frequency that is round-tripped through the conversion.
    return static_cast<int32_t>(((int64_t{dco_frequency_multiplier()} * kReferenceFrequencyKhz) +
                                 (1 << kMultiplierPrecisionBits) - 1) >>
                                kMultiplierPrecisionBits);
  }

  // The currently configured DCO (Digitally Controlled Oscillator) frequency.
  //
  // This is a convenience method on top of the `dco_frequency_multiplier`
  // fields.
  DisplayPllDcoFrequencyKabyLake& set_dco_frequency_khz(int frequency_khz) {
    // The formulas in the PRM use truncating division.
    return set_dco_frequency_multiplier(static_cast<int32_t>(
        (int64_t{frequency_khz} << kMultiplierPrecisionBits) / kReferenceFrequencyKhz));
  }

  static auto GetForDpll(Dpll dpll) {
    ZX_ASSERT(dpll >= Dpll::DPLL_1);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    const int dpll_index = dpll - Dpll::DPLL_0;
    return hwreg::RegisterAddr<DisplayPllDcoFrequencyKabyLake>(0x6c040 + (dpll_index - 1) * 8);
  }
};

// DPLL_CFGCR2 (Display PLL Configuration and Control Register 2?)
//
// When the DPLL (Display PLL) operates in HDMI mode, this register configures
// the frequency dividers between the DCO (Digitally-Controlled Oscillator) in
// the DPLL and the DPLL's AFE (Analog Front-End) clock output, which goes to
// connected DDIs. The frequency output by the DPLL to DDIs, also called AFE
// clock frequency, is the DCO frequency configured in DPLL_CFGCR1 divided by
// the product of all the dividers (P * Q * K, also documented as P0 * P1 * P2)
// in this register.
//
// Unfortunately, Intel's documentation refers to the DCO frequency dividers
// both as (P0, P1, P2) and as (P, Q, K). Fortunately, both variations use short
// names, so we can use both variations in our names below. This facilitates
// checking our code against documents that use either naming variation.
//
// This register's reserved fields are all MBZ (must be zero). So, this register
// can be safely written without reading it first.
//
// The Tiger Lake equivalent of this register is DPLL_CFGCR1.
//
// Kaby Lake: IHD-OS-KBL-Vol 2c-1.17 Part 1 page 526-527
// Skylake: IHD-OS-SKL-Vol 2c-05.16 Part 1 pages 524-525
class DisplayPllDcoDividersKabyLake
    : public hwreg::RegisterBase<DisplayPllDcoDividersKabyLake, uint32_t> {
 public:
  // Possible values for the `k_p2_divider_select` field.
  enum class KP2DividerSelect {
    k5 = 0b00,
    k2 = 0b01,  // The preferred value
    k3 = 0b10,
    k1 = 0b11,
  };

  // Documented values for the `p_p0_divider_select` field.
  enum class PP0DividerSelect {
    k1 = 0b000,
    k2 = 0b001,
    k3 = 0b010,
    k7 = 0b100,
  };

  // Possible values for the `center_frequency_select` field.
  enum class CenterFrequencySelect {
    k9600Mhz = 0b00,
    k9000Mhz = 0b01,
    k8400Mhz = 0b11,
  };

  DEF_RSVDZ_FIELD(31, 16);

  // This field has a non-trivial representation and should be accessed via the
  // `q_p1_divider() and `set_q_p1_divider()` helpers.
  DEF_FIELD(15, 8, q_p1_divider_select);

  // This field has a non-trivial representation and should be accessed via the
  // `q_p1_divider() and `set_q_p1_divider()` helpers.
  DEF_BIT(7, q_p1_divider_select_enabled);

  // This field has a non-trivial representation and should be accessed via the
  // `k_p2_divider() and `set_k_p2_divider()` helpers.
  DEF_ENUM_FIELD(KP2DividerSelect, 6, 5, k_p2_divider_select);

  // This field has a non-trivial representation and should be accessed via the
  // `k_p2_divider() and `set_k_p2_divider()` helpers.
  DEF_ENUM_FIELD(PP0DividerSelect, 4, 2, p_p0_divider_select);

  // This field has a non-trivial representation and should be accessed via the
  // `center_frequency_mhz()` and `set_center_frequency_mhz()` helpers.
  DEF_ENUM_FIELD(CenterFrequencySelect, 1, 0, center_frequency_select);

  // The K (P2) divider.
  //
  // The preferred value is 2. If the K divider is not 2, this constrains both
  // the Q (P1) divider and the P (P0) divider.
  uint8_t k_p2_divider() const {
    switch (k_p2_divider_select()) {
      case KP2DividerSelect::k5:
        return 5;
      case KP2DividerSelect::k2:
        return 2;
      case KP2DividerSelect::k3:
        return 3;
      case KP2DividerSelect::k1:
        return 1;
    }
    // This will never happen. `k_p2_divider_select()` is a 2-bit field.
    ZX_DEBUG_ASSERT(false);
    return 0;
  }

  // The value of the Q (P1) divider.
  //
  // This field must not be zero. Any other value (1-255) is acceptable.
  //
  // The Q divider must be 1 (disabled) if the K divider is not 2. This
  // requirement is also stated as ensuring a 50% duty cycle for this divider.
  uint8_t q_p1_divider() const {
    return (q_p1_divider_select_enabled()) ? q_p1_divider_select() : 1;
  }

  // See `q_p1_divider()` for details.
  DisplayPllDcoDividersKabyLake& set_q_p1_divider(uint8_t q_p1_divider) {
    ZX_ASSERT(q_p1_divider > 0);
    return set_q_p1_divider_select_enabled(q_p1_divider != 1).set_q_p1_divider_select(q_p1_divider);
  }

  // See `k_p2_divider()` for details.
  DisplayPllDcoDividersKabyLake& set_k_p2_divider(uint8_t k_p2_divider) {
    KP2DividerSelect k_p2_divider_select;
    switch (k_p2_divider) {
      case 5:
        k_p2_divider_select = KP2DividerSelect::k5;
        break;
      case 2:
        k_p2_divider_select = KP2DividerSelect::k2;
        break;
      case 3:
        k_p2_divider_select = KP2DividerSelect::k3;
        break;
      case 1:
        k_p2_divider_select = KP2DividerSelect::k1;
        break;
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid K (P2) divider: %d", k_p2_divider);
        k_p2_divider_select = KP2DividerSelect::k2;
    };
    return set_k_p2_divider_select(k_p2_divider_select);
  }

  // The P (P0) divider.
  //
  // The P (P0) divider can only be 1 if the Q (P1) divider is also 1.
  //
  // This helper returns 0 if the field is set to an undocumented value.
  uint8_t p_p0_divider() const {
    switch (p_p0_divider_select()) {
      case PP0DividerSelect::k1:
        return 1;
      case PP0DividerSelect::k2:
        return 2;
      case PP0DividerSelect::k3:
        return 3;
      case PP0DividerSelect::k7:
        return 7;
    }
    return 0;  // The field is set to an undocumented value.
  }

  // See `p_p0_divider()` for details.
  DisplayPllDcoDividersKabyLake& set_p_p0_divider(uint8_t p_p0_divider) {
    PP0DividerSelect p_p0_divider_select;
    switch (p_p0_divider) {
      case 1:
        p_p0_divider_select = PP0DividerSelect::k1;
        break;
      case 2:
        p_p0_divider_select = PP0DividerSelect::k2;
        break;
      case 3:
        p_p0_divider_select = PP0DividerSelect::k3;
        break;
      case 7:
        p_p0_divider_select = PP0DividerSelect::k7;
        break;
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid P (P0) divider: %d", p_p0_divider);
        p_p0_divider_select = PP0DividerSelect::k2;
    };
    return set_p_p0_divider_select(p_p0_divider_select);
  }

  // The center frquency for the DPLL's DCO, in Mhz.
  //
  // The DCO frequency configured in the DisplayPllDcoFrequencyKabyLake register must be
  // within [-6%, +1%] of the selected center frequency.
  //
  // This helper returns 0 if the field is set to an undocumented value.
  int16_t center_frequency_mhz() const {
    switch (center_frequency_select()) {
      case CenterFrequencySelect::k8400Mhz:
        return 8'400;
      case CenterFrequencySelect::k9000Mhz:
        return 9'000;
      case CenterFrequencySelect::k9600Mhz:
        return 9'600;
    }
    return 0;  // The field is set to an undocumented value.
  }

  // See `center_frequency_mhz()` for details.
  DisplayPllDcoDividersKabyLake& set_center_frequency_mhz(int16_t center_frequency_mhz) {
    CenterFrequencySelect center_frequency_select;
    switch (center_frequency_mhz) {
      case 8'400:
        center_frequency_select = CenterFrequencySelect::k8400Mhz;
        break;
      case 9'000:
        center_frequency_select = CenterFrequencySelect::k9000Mhz;
        break;
      case 9'600:
        center_frequency_select = CenterFrequencySelect::k9600Mhz;
        break;
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid DCO center frequency: %d Mhz", center_frequency_mhz);
        center_frequency_select = CenterFrequencySelect::k9000Mhz;
    }
    return set_center_frequency_select(center_frequency_select);
  }

  static auto GetForDpll(Dpll dpll) {
    ZX_ASSERT(dpll >= Dpll::DPLL_1);
    ZX_ASSERT(dpll <= Dpll::DPLL_3);

    const int dpll_index = dpll - Dpll::DPLL_0;
    return hwreg::RegisterAddr<DisplayPllDcoDividersKabyLake>(0x6c044 + (dpll_index - 1) * 8);
  }
};

// DPLL_ENABLE (DPLL Enable), LCPLL_CTL / WRPLL_CTL (LCPLL/WRPLL Control).
//
// This class describes all the PLL enablement registers, as they have similar
// layouts.
//
// On Tiger Lake, this covers all the DPLL_ENABLE (* PLL Enable) registers.
// * DPLL0_ENABLE, DPLL1_ENABLE, DPLL4_ENABLE - for DPLL0/1/4
// * TBTPLL_ENABLE - for DPLL2
// * MGPLL1_ENABLE ... MGPLL6_ENABLE - for MG and Dekel PLLs 1-6
//
// On Kaby Lake and Skylake, this covers the following registers:
// * LCPLL1_CTL / LCPLL2_CTL - LCPLL1/2 Control - for DPLL0/1
// * WRPLL1_CTL / WRPLL2_CTL - WRPLL1/2 Control - for DPLL2/3
//
// PLL enablement registers must not be changed while their corresponding PLLs
// are in use.
//
// On Kaby Lake and Skylake, all DPLLs can be used to drive DDIs. DPLL0 also
// drives the core display clocks (CDCLK, CD2XCLK). LCPLL (DPLL0, DPLL1)
// probably stands for "LC-tank PLL" and WRPLL (DPLL2, DPLL3) probably means
// "Wide-Range PLL".
//
// On Tiger Lake, TC (USB Type-C connector) DDI has its own PLL, called an MG
// PLL. DPLLs (Display PLLs) 0, 1, and 4 can be connected to all DDIs. DPLL2 is
// dedicated to generating the frequencies needed for TBT (Thunderbolt)
// operation, and is shared by all DDIs that operate in Thunderbolt mode.
//
// DPLL_ENABLE documentation:
// Tiger Lake: IHD-OS-TGL-Vol 2c-1.22-Rev 2.0 Part 1 pages 656-657
//
// LCPLL1_CTL and LCPLL2_CTL documentation:
// Kaby Lake: IHD-OS-KBL-Vol 2c-1.17 Part 1 pages 1121, 1122
// Skylake: IHD-OS-SKL-Vol 2c-05.16 Part 1 pages 1110, 1111
//
// WRPLL1_CTL and WRPLL2_CTL documentation:
// Kaby Lake: IHD-OS-KBL-Vol 2c-1.17 Part 2 pages 1349-1350
// Skylake: IHD-OS-SKL-Vol 2c-05.16 Part 2 pages 1321-1322
class PllEnable : public hwreg::RegisterBase<PllEnable, uint32_t> {
 public:
  // If true, the PLL will be enabled. If false, the PLL will be disabled.
  //
  // The PLL's frequency must be set before it is enabled.
  DEF_BIT(31, pll_enabled);

  // If true, the PLL is locked. If false, the PLL is not locked.
  //
  // On Tiger Lake, this field is supported on all PLL enablement registers.
  //
  // On Kaby Lake and Skylake, this field is only supported on LCPLL1, which
  // drives DPLL0. The underlying bit is reserved on all other registers. On
  // LCPLL1, this field seems redundant with the DPLL0 locked field in the
  // DPLL_STATUS register. However, PRM explicitly asks us to check this field,
  // in "Sequences to Initialize Display" sub-sections "Initialize Sequence" and
  // "Un-initialize Sequence".
  // Kaby Lake: IHD-OS-KBL-Vol 12-1.17 pages 112-113
  // Skylake: IHD-OS-SKL-Vol 12-05.16 pages 110-111
  DEF_BIT(30, pll_locked_tiger_lake_and_lcpll1);

  // If true, the PLL will eventually be powered on.
  //
  // This field is only documented for Tiger Lake.
  //
  // On Kaby Lake and Skylake, the underlying bit is reserved, and PLLs can be
  // assumed to be powered on at all times.
  DEF_BIT(27, power_on_request_tiger_lake);

  // If true, the PLL is currently powered on.
  //
  // A PLL must be powered on before it is enabled.
  //
  // This field is only documented for Tiger Lake. The underlying bit is
  // reserved on Kaby Lake and Skylake.
  DEF_BIT(26, powered_on_tiger_lake);

  static auto GetForSkylakeDpll(Dpll dpll) {
    ZX_ASSERT_MSG(dpll >= Dpll::DPLL_0, "Unsupported DPLL %d", dpll);
    ZX_ASSERT_MSG(dpll <= Dpll::DPLL_3, "Unsupported DPLL %d", dpll);
    const int dpll_index = dpll - Dpll::DPLL_0;

    static constexpr uint32_t kAddresses[] = {0x46010, 0x46014, 0x46040, 0x46060};
    return hwreg::RegisterAddr<PllEnable>(kAddresses[dpll_index]);
  }

  // Tiger Lake: On IHD-OS-TGL-Vol 2c-1.22-Rev 2.0, Page 656, it mentions
  // that the MG register instances are used for Type-C in general, so they
  // can control Dekel PLLs as well (for example, MGPLL1_ENABLE controls
  // Dekel PLL Type-C Port 1).
  static auto GetForTigerLakeDpll(Dpll dpll) {
    if (dpll >= Dpll::DPLL_TC_1 && dpll <= Dpll::DPLL_TC_6) {
      // MGPLL1_ENABLE - MGPLL6_ENABLE
      const int mgpll_index = dpll - Dpll::DPLL_TC_1;
      return hwreg::RegisterAddr<PllEnable>(0x46030 + 4 * mgpll_index);
    }

    ZX_ASSERT_MSG(dpll >= Dpll::DPLL_0, "Unsupported DPLL %d", dpll);

    // TODO(fxbug.dev/110351): Allow DPLL 4, once we support it.
    ZX_ASSERT_MSG(dpll <= Dpll::DPLL_2, "Unsupported DPLL %d", dpll);

    const int dpll_index = dpll - Dpll::DPLL_0;
    static constexpr uint32_t kPllEnableAddresses[] = {0x46010, 0x46014, 0x46020, 0, 0x46018};
    return hwreg::RegisterAddr<PllEnable>(kPllEnableAddresses[dpll_index]);
  }
};

// DPLL_STATUS
//
// This register is not documented on Tiger Lake or DG1. On those display
// engines, the DPLL_ENABLE register for each DPLL has a status field.
//
// Kaby Lake: IHD-OS-KBL-Vol 2c-1.17 Part 1 pages 535-537
// Skylake: IHD-OS-SKL-Vol 2c-05.16 Part 1 pages 533-535
class DisplayPllStatus : public hwreg::RegisterBase<DisplayPllStatus, uint32_t> {
 public:
  DEF_BIT(28, pll3_sem_done);
  DEF_BIT(24, pll3_locked);
  DEF_BIT(20, pll2_sem_done);
  DEF_BIT(16, pll2_locked);
  DEF_BIT(12, pll1_sem_done);
  DEF_BIT(8, pll1_locked);
  DEF_BIT(4, pll0_sem_done);
  DEF_BIT(0, pll0_locked);

  // The meaning of "SEM Done" is not documented.
  //
  // Including access to these fields for logging purposes.
  bool pll_sem_done(Dpll display_pll) const {
    ZX_ASSERT_MSG(display_pll >= Dpll::DPLL_0, "Unsupported Display PLL %d", display_pll);
    ZX_ASSERT_MSG(display_pll <= Dpll::DPLL_3, "Unsupported Display PLL %d", display_pll);

    const int display_pll_index = display_pll - Dpll::DPLL_0;
    const int locked_bit_index = display_pll_index * 8 + 4;

    // The cast is lossless because the BitfieldRef references a 1-bit field.
    return static_cast<bool>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), locked_bit_index, locked_bit_index)
            .get());
  }

  // True if the DPLL (Display PLL) is locked onto its target frequency.
  //
  // Soon after a PLL is enabled, it will lock onto its target frequency. Soon
  // after a PLL is disabled, it will no longer be locked -- the frequency lock
  // will be lost.
  bool pll_locked(Dpll display_pll) const {
    ZX_ASSERT_MSG(display_pll >= Dpll::DPLL_0, "Unsupported Display PLL %d", display_pll);
    ZX_ASSERT_MSG(display_pll <= Dpll::DPLL_3, "Unsupported Display PLL %d", display_pll);

    const int display_pll_index = display_pll - Dpll::DPLL_0;
    const int locked_bit_index = display_pll_index * 8;

    // The cast is lossless because the BitfieldRef references a 1-bit field.
    return static_cast<bool>(
        hwreg::BitfieldRef<const uint32_t>(reg_value_ptr(), locked_bit_index, locked_bit_index)
            .get());
  }

  static auto Get() { return hwreg::RegisterAddr<DisplayPllStatus>(0x6c060); }
};

}  // namespace tgl_registers

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_TGL_REGISTERS_DPLL_H_
