// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/edid/timings.h"

#include <lib/stdcompat/span.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace edid {

namespace {

TEST(EdidTimingParams, Progressive) {
  constexpr display::DisplayTiming kExpected = {
      .horizontal_active_px = 0x0f'0f,
      .horizontal_front_porch_px = 0x0a'0a,
      .horizontal_sync_width_px = 0x01'01,
      .horizontal_back_porch_px = 0x02'02,
      .vertical_active_lines = 0x0b'0b,
      .vertical_front_porch_lines = 0x03'03,
      .vertical_sync_width_lines = 0x04'04,
      .vertical_back_porch_lines = 0x05'05,
      .pixel_clock_frequency_khz = 0x1f'1f'1f,
      .fields_per_frame = display::FieldsPerFrame::kProgressive,
      .hsync_polarity = display::SyncPolarity::kPositive,
      .vsync_polarity = display::SyncPolarity::kPositive,
      .vblank_alternates = false,
      .pixel_repetition = 0,
  };
  constexpr timing_params_t kEdidTimingParams = {
      .pixel_freq_khz = 0x1f'1f'1f,
      .horizontal_addressable = 0x0f'0f,
      .horizontal_front_porch = 0x0a'0a,
      .horizontal_sync_pulse = 0x01'01,
      .horizontal_blanking = 0x0d'0d,
      .vertical_addressable = 0x0b'0b,
      .vertical_front_porch = 0x03'03,
      .vertical_sync_pulse = 0x04'04,
      .vertical_blanking = 0x0c'0c,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      // Calculated from the values above:
      //  Htotal = Hactive + Hblank = 0x1c'1c,
      //  Vtotal = Vactive + Vblank = 0x17'17,
      //  Field refresh rate = Pixel Freq (Hz) / (Htotal * Vtotal) = 47.9501 Hz
      .vertical_refresh_e2 = 4795,
  };

  const display::DisplayTiming display_timing = ToDisplayTiming(kEdidTimingParams);
  EXPECT_EQ(kExpected, display_timing);

  const int vertical_refresh_rate_centihertz =
      (display_timing.vertical_field_refresh_rate_millihertz() + 5) / 10;
  EXPECT_EQ(vertical_refresh_rate_centihertz,
            static_cast<int>(kEdidTimingParams.vertical_refresh_e2));
}

TEST(EdidTimingParams, Interlaced) {
  constexpr display::DisplayTiming kExpected = {
      .horizontal_active_px = 0x0f'0f,
      .horizontal_front_porch_px = 0x0a'0a,
      .horizontal_sync_width_px = 0x01'01,
      .horizontal_back_porch_px = 0x02'02,
      .vertical_active_lines = 0x0b'0b,
      .vertical_front_porch_lines = 0x03'03,
      .vertical_sync_width_lines = 0x04'04,
      .vertical_back_porch_lines = 0x05'05,
      .pixel_clock_frequency_khz = 0x1f'1f'1f,
      .fields_per_frame = display::FieldsPerFrame::kInterlaced,
      .hsync_polarity = display::SyncPolarity::kPositive,
      .vsync_polarity = display::SyncPolarity::kPositive,
      .vblank_alternates = true,
      .pixel_repetition = 0,
  };
  constexpr timing_params_t kEdidTimingParams = {
      .pixel_freq_khz = 0x1f'1f'1f,
      .horizontal_addressable = 0x0f'0f,
      .horizontal_front_porch = 0x0a'0a,
      .horizontal_sync_pulse = 0x01'01,
      .horizontal_blanking = 0x0d'0d,
      .vertical_addressable = 0x0b'0b,
      .vertical_front_porch = 0x03'03,
      .vertical_sync_pulse = 0x04'04,
      .vertical_blanking = 0x0c'0c,
      .flags = timing_params_t::kInterlaced | timing_params_t::kPositiveHsync |
               timing_params_t::kPositiveVsync | timing_params_t::kAlternatingVblank,
      // Calculated from the values above:
      //  Htotal = Hactive + Hblank = 0x1c'1c,
      //  Vtotal = Vactive + Vblank * 2 + 1 = 0x23'24
      //  Field refresh rate = Pixel Freq (Hz) / (Htotal * Vtotal) = 63.0131 Hz
      .vertical_refresh_e2 = 63'01,
  };

  const display::DisplayTiming display_timing = ToDisplayTiming(kEdidTimingParams);
  EXPECT_EQ(kExpected, display_timing);

  const int vertical_refresh_rate_centihertz =
      (display_timing.vertical_field_refresh_rate_millihertz() + 5) / 10;
  EXPECT_EQ(vertical_refresh_rate_centihertz,
            static_cast<int>(kEdidTimingParams.vertical_refresh_e2));
}

TEST(EdidTimingParams, PolarityMapping) {
  // Verify Hsync / Vsync polarities are correctly mapped.
  {
    constexpr display::DisplayTiming kExpected = {
        .horizontal_active_px = 0x0f'0f,
        .horizontal_front_porch_px = 0x0a'0a,
        .horizontal_sync_width_px = 0x01'01,
        .horizontal_back_porch_px = 0x02'02,
        .vertical_active_lines = 0x0b'0b,
        .vertical_front_porch_lines = 0x03'03,
        .vertical_sync_width_lines = 0x04'04,
        .vertical_back_porch_lines = 0x05'05,
        .pixel_clock_frequency_khz = 0x1f'1f'1f,
        .fields_per_frame = display::FieldsPerFrame::kInterlaced,
        .hsync_polarity = display::SyncPolarity::kPositive,
        .vsync_polarity = display::SyncPolarity::kNegative,
        .vblank_alternates = true,
        .pixel_repetition = 0,
    };
    constexpr timing_params_t kEdidTimingParams = {
        .pixel_freq_khz = 0x1f'1f'1f,
        .horizontal_addressable = 0x0f'0f,
        .horizontal_front_porch = 0x0a'0a,
        .horizontal_sync_pulse = 0x01'01,
        .horizontal_blanking = 0x0d'0d,
        .vertical_addressable = 0x0b'0b,
        .vertical_front_porch = 0x03'03,
        .vertical_sync_pulse = 0x04'04,
        .vertical_blanking = 0x0c'0c,
        .flags = timing_params_t::kInterlaced | timing_params_t::kPositiveHsync |
                 timing_params_t::kAlternatingVblank,
        // Calculated from the values above:
        //  Htotal = Hactive + Hblank = 0x1c'1c,
        //  Vtotal = Vactive + Vblank * 2 + 1 = 0x23'24
        //  Field refresh rate = Pixel Freq (Hz) / (Htotal * Vtotal) = 63.0131 Hz
        .vertical_refresh_e2 = 63'01,
    };

    const display::DisplayTiming display_timing = ToDisplayTiming(kEdidTimingParams);
    EXPECT_EQ(kExpected, display_timing);

    const int vertical_refresh_rate_centihertz =
        (display_timing.vertical_field_refresh_rate_millihertz() + 5) / 10;
    EXPECT_EQ(vertical_refresh_rate_centihertz,
              static_cast<int>(kEdidTimingParams.vertical_refresh_e2));
  }
  {
    constexpr display::DisplayTiming kExpected = {
        .horizontal_active_px = 0x0f'0f,
        .horizontal_front_porch_px = 0x0a'0a,
        .horizontal_sync_width_px = 0x01'01,
        .horizontal_back_porch_px = 0x02'02,
        .vertical_active_lines = 0x0b'0b,
        .vertical_front_porch_lines = 0x03'03,
        .vertical_sync_width_lines = 0x04'04,
        .vertical_back_porch_lines = 0x05'05,
        .pixel_clock_frequency_khz = 0x1f'1f'1f,
        .fields_per_frame = display::FieldsPerFrame::kInterlaced,
        .hsync_polarity = display::SyncPolarity::kNegative,
        .vsync_polarity = display::SyncPolarity::kPositive,
        .vblank_alternates = true,
        .pixel_repetition = 0,
    };
    constexpr edid::timing_params_t kEdidTimingParams = {
        .pixel_freq_khz = 0x1f'1f'1f,
        .horizontal_addressable = 0x0f'0f,
        .horizontal_front_porch = 0x0a'0a,
        .horizontal_sync_pulse = 0x01'01,
        .horizontal_blanking = 0x0d'0d,
        .vertical_addressable = 0x0b'0b,
        .vertical_front_porch = 0x03'03,
        .vertical_sync_pulse = 0x04'04,
        .vertical_blanking = 0x0c'0c,
        .flags = timing_params_t::kInterlaced | timing_params_t::kPositiveVsync |
                 timing_params_t::kAlternatingVblank,
        // Calculated from the values above:
        //  Htotal = Hactive + Hblank = 0x1c'1c,
        //  Vtotal = Vactive + Vblank * 2 + 1 = 0x23'24
        //  Field refresh rate = Pixel Freq (Hz) / (Htotal * Vtotal) = 63.0131 Hz
        .vertical_refresh_e2 = 63'01,
    };

    const display::DisplayTiming display_timing = ToDisplayTiming(kEdidTimingParams);
    EXPECT_EQ(kExpected, display_timing);

    const int vertical_refresh_rate_centihertz =
        (display_timing.vertical_field_refresh_rate_millihertz() + 5) / 10;
    EXPECT_EQ(vertical_refresh_rate_centihertz,
              static_cast<int>(kEdidTimingParams.vertical_refresh_e2));
  }
}

TEST(EdidTimingParams, CeaTimingsToDisplayTiming) {
  cpp20::span<const edid::timing_params_t> kCeaTimings(edid::internal::cea_timings,
                                                       edid::internal::cea_timings_count);
  for (const edid::timing_params_t& cea_timing : kCeaTimings) {
    display::DisplayTiming timing = ToDisplayTiming(cea_timing);
    EXPECT_EQ(timing.horizontal_active_px, static_cast<int32_t>(cea_timing.horizontal_addressable));
    EXPECT_EQ(timing.horizontal_front_porch_px,
              static_cast<int32_t>(cea_timing.horizontal_front_porch));
    EXPECT_EQ(timing.horizontal_sync_width_px,
              static_cast<int32_t>(cea_timing.horizontal_sync_pulse));
    EXPECT_EQ(timing.horizontal_blank_px(), static_cast<int32_t>(cea_timing.horizontal_blanking));
    EXPECT_EQ(timing.vertical_active_lines, static_cast<int32_t>(cea_timing.vertical_addressable));
    EXPECT_EQ(timing.vertical_front_porch_lines,
              static_cast<int32_t>(cea_timing.vertical_front_porch));
    EXPECT_EQ(timing.vertical_sync_width_lines,
              static_cast<int32_t>(cea_timing.vertical_sync_pulse));
    EXPECT_EQ(timing.vertical_blank_lines(), static_cast<int32_t>(cea_timing.vertical_blanking));
    EXPECT_EQ(timing.pixel_clock_frequency_khz, static_cast<int32_t>(cea_timing.pixel_freq_khz));

    const int vertical_refresh_rate_centihertz =
        (timing.vertical_field_refresh_rate_millihertz() + 5) / 10;
    EXPECT_EQ(vertical_refresh_rate_centihertz, static_cast<int>(cea_timing.vertical_refresh_e2));
  }
}

TEST(EdidTimingParams, DmtTimingsToDisplayTiming) {
  cpp20::span<const edid::timing_params_t> kDmtTimings(edid::internal::dmt_timings,
                                                       edid::internal::dmt_timings_count);
  for (const edid::timing_params_t& dmt_timing : kDmtTimings) {
    display::DisplayTiming timing = ToDisplayTiming(dmt_timing);
    EXPECT_EQ(timing.horizontal_active_px, static_cast<int32_t>(dmt_timing.horizontal_addressable));
    EXPECT_EQ(timing.horizontal_front_porch_px,
              static_cast<int32_t>(dmt_timing.horizontal_front_porch));
    EXPECT_EQ(timing.horizontal_sync_width_px,
              static_cast<int32_t>(dmt_timing.horizontal_sync_pulse));
    EXPECT_EQ(timing.horizontal_blank_px(), static_cast<int32_t>(dmt_timing.horizontal_blanking));
    EXPECT_EQ(timing.vertical_active_lines, static_cast<int32_t>(dmt_timing.vertical_addressable));
    EXPECT_EQ(timing.vertical_front_porch_lines,
              static_cast<int32_t>(dmt_timing.vertical_front_porch));
    EXPECT_EQ(timing.vertical_sync_width_lines,
              static_cast<int32_t>(dmt_timing.vertical_sync_pulse));
    EXPECT_EQ(timing.vertical_blank_lines(), static_cast<int32_t>(dmt_timing.vertical_blanking));
    EXPECT_EQ(timing.pixel_clock_frequency_khz, static_cast<int32_t>(dmt_timing.pixel_freq_khz));

    const int vertical_refresh_rate_centihertz =
        (timing.vertical_field_refresh_rate_millihertz() + 5) / 10;
    EXPECT_EQ(vertical_refresh_rate_centihertz, static_cast<int>(dmt_timing.vertical_refresh_e2));
  }
}

}  // namespace

}  // namespace edid
