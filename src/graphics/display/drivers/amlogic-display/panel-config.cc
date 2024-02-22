// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include "src/graphics/display/drivers/amlogic-display/initcodes-inl.h"
#include "src/graphics/display/drivers/amlogic-display/panel/boe-tv070wsm-fitipower-jd9364-astro.h"
#include "src/graphics/display/drivers/amlogic-display/panel/boe-tv070wsm-fitipower-jd9364-nelson.h"
#include "src/graphics/display/drivers/amlogic-display/panel/boe-tv070wsm-fitipower-jd9365.h"
#include "src/graphics/display/drivers/amlogic-display/panel/boe-tv101wxm-fitipower-jd9364.h"
#include "src/graphics/display/drivers/amlogic-display/panel/boe-tv101wxm-fitipower-jd9365.h"
#include "src/graphics/display/drivers/amlogic-display/panel/innolux-p070acb-fitipower-jd9364.h"
#include "src/graphics/display/drivers/amlogic-display/panel/innolux-p101dez-fitipower-jd9364.h"
#include "src/graphics/display/drivers/amlogic-display/panel/kd-kd070d82-fitipower-jd9364.h"
#include "src/graphics/display/drivers/amlogic-display/panel/kd-kd070d82-fitipower-jd9365.h"
#include "src/graphics/display/drivers/amlogic-display/panel/microtech-mtf050fhdi03-novatek-nt35596.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace amlogic_display {

namespace {

// BOE TV070WSM-TG1
//
// BOE TV070WSM-TG1 spec Section 6.1 "Signal Timing Spec", page 18 covers
// the timings below that aren't fixed by the MIPI DSI specification.
//
// The following timings also satisfy the minimum / maximum requirements
// specified in:
// - JD9364 datasheet, Section 11.3.3 "Timings for DSI Video Mode",
//   pages 211-214;
// - JD9365D datasheet, Section 11.3.3 "Timings for DSI Video Mode",
//   pages 197-200.
constexpr display::DisplayTiming kBoeTv070wsmPanelTimings = {
    .horizontal_active_px = 600,
    // The panel datasheet specifies a typical value of 20.
    .horizontal_front_porch_px = 40,
    .horizontal_sync_width_px = 24,
    .horizontal_back_porch_px = 36,
    .vertical_active_lines = 1024,
    // The panel datasheet specifies a typical value of 6.
    .vertical_front_porch_lines = 19,
    .vertical_sync_width_lines = 2,
    .vertical_back_porch_lines = 8,
    // The panel datasheet specifies a typical value of 42.4 MHz, with no
    // upper or lower bound.
    .pixel_clock_frequency_khz = 44'226,  // refresh rate: 60 Hz
    .fields_per_frame = display::FieldsPerFrame::kProgressive,
    .hsync_polarity = display::SyncPolarity::kNegative,
    .vsync_polarity = display::SyncPolarity::kNegative,
    .vblank_alternates = false,
    .pixel_repetition = 0,
};

// BOE TV101WXM-AG0
//
// BOE TV101WXM-AG0 spec Section 6.1 "Signal Timing Spec", page 18 covers
// the timings below that aren't fixed by the MIPI DSI specification.
//
// The following timings also satisfy the minimum / maximum requirements
// specified in:
// - JD9364 datasheet, Section 11.3.3 "Timings for DSI Video Mode",
//   pages 211-214;
// - JD9365 datasheet, Section 11.3.3 "Timings for DSI Video Mode",
//   pages 197-200.
constexpr display::DisplayTiming kBoeTv101wxmPanelTimings = {
    .horizontal_active_px = 800,
    // The timings here don't match the minimum timings in the panel datasheet
    // or typical timings in the DDIC datasheets.
    // The panel datasheet specifies a minimum value of 40, while the DDIC
    // datasheets specify a typical value of 18.
    .horizontal_front_porch_px = 20,
    // This value matches the typical value from the panel datasheet, while the
    // DDIC datasheets specify a typical value of 18.
    .horizontal_sync_width_px = 20,
    // The panel datasheet specifies a minimum value of 20, while the DDIC
    // datasheets specify a typical value of 18.
    .horizontal_back_porch_px = 50,
    .vertical_active_lines = 1280,
    .vertical_front_porch_lines = 20,
    .vertical_sync_width_lines = 4,
    // This value matches the typical value from the panel datasheet, while the
    // DDIC datasheets specify a typical value of 10.
    .vertical_back_porch_lines = 20,
    .pixel_clock_frequency_khz = 70'702,  // refresh rate: 60 Hz
    .fields_per_frame = display::FieldsPerFrame::kProgressive,
    .hsync_polarity = display::SyncPolarity::kNegative,
    .vsync_polarity = display::SyncPolarity::kNegative,
    .vblank_alternates = false,
    .pixel_repetition = 0,
};

// K&D KD070D82-39TI-A010
//
// K&D KD070D82-39TI-A010 spec Section 6.1 "Timing Setting Table", page 8
// covers the timings below that aren't fixed by the MIPI DSI specification.
//
// These are the typical timings specified in the panel datasheet.
//
// The following timings also satisfy the minimum / maximum requirements
// specified in:
// - JD9364 datasheet, Section 11.3.3 "Timings for DSI Video Mode",
//   pages 211-214;
// - JD9365 datasheet, Section 11.3.3 "Timings for DSI Video Mode",
//   pages 197-200.
constexpr display::DisplayTiming kKdKd070d82PanelTimings = {
    .horizontal_active_px = 600,
    .horizontal_front_porch_px = 80,
    .horizontal_sync_width_px = 10,
    .horizontal_back_porch_px = 80,
    .vertical_active_lines = 1024,
    .vertical_front_porch_lines = 20,
    .vertical_sync_width_lines = 6,
    .vertical_back_porch_lines = 20,
    .pixel_clock_frequency_khz = 49'434,  // refresh rate: 60 Hz
    .fields_per_frame = display::FieldsPerFrame::kProgressive,
    .hsync_polarity = display::SyncPolarity::kNegative,
    .vsync_polarity = display::SyncPolarity::kNegative,
    .vblank_alternates = false,
    .pixel_repetition = 0,
};

constexpr PanelConfig kBoeTv070wsmFitipowerJd9364AstroPanelConfig = {
    .name = "BOE_TV070WSM_FITIPOWER_JD9364_ASTRO",
    .dsi_on = lcd_init_sequence_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,
    // BOE TV070WSM-TG1 spec, Section 6.1 "Signal Timing Spec", page 18 states
    // that the panel supports a MIPI (presumably D-PHY) clock between
    // 145-250 MHz.
    //
    // JD9364 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 207
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 180'000'000,

    .display_timing = kBoeTv070wsmPanelTimings,
};

// See PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON for model information.
constexpr PanelConfig kBoeTv070wsmFitipowerJd9364NelsonPanelConfig = {
    .name = "BOE_TV070WSM_FITIPOWER_JD9364_NELSON",
    .dsi_on = lcd_init_sequence_BOE_TV070WSM_FITIPOWER_JD9364_NELSON,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,
    // BOE TV070WSM-TG1 spec, Section 6.1 "Signal Timing Spec", page 18 states
    // that the panel supports a MIPI (presumably D-PHY) clock between
    // 145-250 MHz.
    //
    // JD9364 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 207
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 180'000'000,

    .display_timing = kBoeTv070wsmPanelTimings,
};

// See PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364 for model information.
constexpr PanelConfig kInnoluxP070acbFitipowerJd9364PanelConfig = {
    .name = "INNOLUX_P070ACB_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_INNOLUX_P070ACB_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,

    // Innolux P070ACB-DB0 spec Section 3.2.2.1.1 "High Speed Mode", page 11
    // states that during a 4-lane D-PHY transmission, the allowed range of the
    // DSI (presumably D-PHY) clock period is between 4ns-8ns. Therefore, the
    // panel-supported clock lane frequency is between 1/(8ns) = 125 MHz and
    // 1/(4ns) = 250 MHz.
    //
    // JD9364 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 207
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 200'000'000,

    // Innolux P070ACB-DB0 spec Section 3.5 "Signal Timing Specifications",
    // page 20 covers the timings below that aren't fixed by the MIPI DSI
    // specification.
    //
    // These are the typical values specified in the datasheet.
    //
    // The following timings also satisfy the minimum / maximum requirements
    // specified in:
    // - JD9364 datasheet, Section 11.3.3 "Timings for DSI Video Mode",
    //   pages 211-214;
    // - JD9365D datasheet, Section 11.3.3 "Timings for DSI Video Mode",
    //   pages 197-200.
    .display_timing =
        {
            .horizontal_active_px = 600,
            .horizontal_front_porch_px = 80,
            .horizontal_sync_width_px = 10,
            .horizontal_back_porch_px = 80,
            .vertical_active_lines = 1024,
            .vertical_front_porch_lines = 20,
            .vertical_sync_width_lines = 6,
            .vertical_back_porch_lines = 20,
            .pixel_clock_frequency_khz = 49'434,  // refresh rate: 60 Hz
            .fields_per_frame = display::FieldsPerFrame::kProgressive,
            .hsync_polarity = display::SyncPolarity::kNegative,
            .vsync_polarity = display::SyncPolarity::kNegative,
            .vblank_alternates = false,
            .pixel_repetition = 0,
        },
};

// See PANEL_BOE_TV101WXM_FITIPOWER_JD9364 for model information.
constexpr PanelConfig kBoeTv101wxmFitipowerJd9364PanelConfig = {
    .name = "BOE_TV101WXM_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_BOE_TV101WXM_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,

    // The current value exceeds both the typical value of the panel spec and
    // the limit of the display driver IC.
    //
    // BOE TV101WXM-AG0 spec, Section 6.1 "Signal Timing Spec", page 18 states
    // that the panel supports a typical MIPI (presumably D-PHY) clock of
    // 250 MHz.
    //
    // JD9364 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 207
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 283'000'000,

    .display_timing = kBoeTv101wxmPanelTimings,
};

// See PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364 for model information.
constexpr PanelConfig kInnoluxP101dezFitipowerJd9364PanelConfig = {
    .name = "INNOLUX_P101DEZ_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_INNOLUX_P101DEZ_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,

    // The current value exceeds the frequency limits of the display driver IC.
    //
    // JD9364 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 207
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 283'000'000,

    // The following timings satisfy the minimum / maximum requirements
    // specified in:
    // - JD9364 datasheet, Section 11.3.3 "Timings for DSI Video Mode",
    //   pages 211-214;
    .display_timing =
        {
            .horizontal_active_px = 800,
            // The DDIC datasheet specifies a typical value of 18.
            .horizontal_front_porch_px = 46,
            // The DDIC datasheet specifies a typical value of 18.
            .horizontal_sync_width_px = 24,
            // The DDIC datasheet specifies a typical value of 18.
            .horizontal_back_porch_px = 20,
            .vertical_active_lines = 1280,
            .vertical_front_porch_lines = 20,
            .vertical_sync_width_lines = 4,
            // The DDIC datasheet specifies a typical value of 10.
            .vertical_back_porch_lines = 20,
            .pixel_clock_frequency_khz = 70'702,  // refresh rate: 60 Hz
            .fields_per_frame = display::FieldsPerFrame::kProgressive,
            .hsync_polarity = display::SyncPolarity::kNegative,
            .vsync_polarity = display::SyncPolarity::kNegative,
            .vblank_alternates = false,
            .pixel_repetition = 0,
        },
};

// See PANEL_BOE_TV101WXM_FITIPOWER_JD9365 for model information.
constexpr PanelConfig kBoeTv101wxmFitipowerJd9365PanelConfig = {
    .name = "BOE_TV101WXM_FITIPOWER_JD9365",
    .dsi_on = lcd_init_sequence_BOE_TV101WXM_FITIPOWER_JD9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,

    // The current value exceeds both the typical value of the panel spec and
    // the limit of the display driver IC.
    //
    // BOE TV101WXM-AG0 spec, Section 6.1 "Signal Timing Spec", page 18 states
    // that the panel supports a typical MIPI (presumably D-PHY) clock of
    // 250 MHz.
    //
    // JD9365 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 193
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 283'000'000,

    .display_timing = kBoeTv101wxmPanelTimings,
};

// See PANEL_BOE_TV070WSM_FITIPOWER_JD9365 for model information.
constexpr PanelConfig kBoeTv070wsmFitipowerJd9365PanelConfig = {
    .name = "BOE_TV070WSM_FITIPOWER_JD9365",
    .dsi_on = lcd_init_sequence_BOE_TV070WSM_FITIPOWER_JD9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,
    // BOE TV070WSM-TG1 spec, Section 6.1 "Signal Timing Spec", page 18 states
    // that the panel supports a MIPI (presumably D-PHY) clock between
    // 145-250 MHz.
    //
    // JD9365 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 193
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 180'000'000,

    .display_timing = kBoeTv070wsmPanelTimings,
};

// See PANEL_KD_KD070D82_FITIPOWER_JD9364 for model information.
constexpr PanelConfig kKdKd070d82FitipowerJd9364PanelConfig = {
    .name = "KD_KD070D82_FITIPOWER_JD9364",
    .dsi_on = lcd_init_sequence_KD_KD070D82_FITIPOWER_JD9364,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,
    // The panel datasheet doesn't specify the D-PHY clock frequency limit.
    //
    // JD9364 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 207
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 200'000'000,

    .display_timing = kKdKd070d82PanelTimings,
};

// See PANEL_KD_KD070D82_FITIPOWER_JD9365 for model information.
constexpr PanelConfig kKdKd070d82FitipowerJd9365PanelConfig = {
    .name = "KD_KD070D82_FITIPOWER_JD9365",
    .dsi_on = lcd_init_sequence_KD_KD070D82_FITIPOWER_JD9365,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForAstroSherlockNelson,
    .power_off = kLcdPowerOffSequenceForAstroSherlockNelson,

    .dphy_data_lane_count = 4,
    // The panel datasheet doesn't specify the D-PHY clock frequency limit.
    //
    // JD9365 datasheet, Section 11.3.2 "DSI D-PHY electronic characteristics",
    // Table 11.11 "Reverse HS Data Transmission Timing Parameters", page 193
    // states that the per-lane data rate must be between 80-500 Mbps (for
    // 4-lane 24-bit pixel data). Therefore, the DDIC-supported D-PHY clock
    // lane frequency is between 40-250 MHz.
    .maximum_dphy_clock_lane_frequency_hz = 200'000'000,

    .display_timing = kKdKd070d82PanelTimings,
};

// See PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596 for model information.
constexpr PanelConfig kMicrotechMtf050fhdi03NovatekNt35596PanelConfig = {
    .name = "MICROTECH_MTF050FHDI03_NOVATEK_NT35596",
    .dsi_on = lcd_init_sequence_MICROTECH_MTF050FHDI03_NOVATEK_NT35596,
    .dsi_off = lcd_shutdown_sequence,
    .power_on = kLcdPowerOnSequenceForVim3Ts050,
    .power_off = kLcdPowerOffSequenceForVim3Ts050,

    .dphy_data_lane_count = 4,

    // Microtech MTF050FHDI-03 spec, Section 3.4 "MIPI-DSI characteristics",
    // page 9 states that the allowed range of the DSI (presumably D-PHY) clock
    // period is between 2ns and 25ns.
    // Therefore, the clock lane frequency must be between 1/(25ns) = 40 MHz
    // and 1/(2ns) = 500 MHz.
    //
    // This exactly matches the value stated in the Novatek NT35596 DDIC
    // datasheet v0.05, Section 5.5.2.4.5 "Parameters", page 132, and
    // Section 7.3.1 "MIPI Interface Characteristics", page 222.
    .maximum_dphy_clock_lane_frequency_hz = 500'000'000,

    // Microtech MTF050FHDI-03 spec doesn't contain the display timing values.
    //
    // NT35596 datasheet doesn't have any constraints on the timing parameters
    // (where all the constraints are TBD) in Section 5.5.2.4.5 "Parameters",
    // page 132.
    //
    // The following values were provided by Microtech engineers and tweaked
    // by Khadas.
    .display_timing =
        {
            .horizontal_active_px = 1080,
            .horizontal_front_porch_px = 15,
            .horizontal_sync_width_px = 2,
            .horizontal_back_porch_px = 23,
            .vertical_active_lines = 1920,
            .vertical_front_porch_lines = 2,
            .vertical_sync_width_lines = 4,
            .vertical_back_porch_lines = 7,
            .pixel_clock_frequency_khz = 120'000,  // refresh rate: 55.428 Hz
            .fields_per_frame = display::FieldsPerFrame::kProgressive,
            .hsync_polarity = display::SyncPolarity::kNegative,
            .vsync_polarity = display::SyncPolarity::kNegative,
            .vblank_alternates = false,
            .pixel_repetition = 0,
        },
};

}  // namespace

const PanelConfig* GetPanelConfig(uint32_t panel_type) {
  // LINT.IfChange
  switch (panel_type) {
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO:
      return &kBoeTv070wsmFitipowerJd9364AstroPanelConfig;
    case PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364:
      return &kInnoluxP070acbFitipowerJd9364PanelConfig;
    case PANEL_BOE_TV101WXM_FITIPOWER_JD9364:
      return &kBoeTv101wxmFitipowerJd9364PanelConfig;
    case PANEL_INNOLUX_P101DEZ_FITIPOWER_JD9364:
      return &kInnoluxP101dezFitipowerJd9364PanelConfig;
    case PANEL_BOE_TV101WXM_FITIPOWER_JD9365:
      return &kBoeTv101wxmFitipowerJd9365PanelConfig;
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9365:
      return &kBoeTv070wsmFitipowerJd9365PanelConfig;
    case PANEL_KD_KD070D82_FITIPOWER_JD9364:
      return &kKdKd070d82FitipowerJd9364PanelConfig;
    case PANEL_KD_KD070D82_FITIPOWER_JD9365:
      return &kKdKd070d82FitipowerJd9365PanelConfig;
    case PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596:
      return &kMicrotechMtf050fhdi03NovatekNt35596PanelConfig;
    case PANEL_BOE_TV070WSM_FITIPOWER_JD9364_NELSON:
      return &kBoeTv070wsmFitipowerJd9364NelsonPanelConfig;
  }
  // LINT.ThenChange(//src/graphics/display/lib/device-protocol-display/include/lib/device-protocol/display-panel.h)
  return nullptr;
}

display_setting_t ToDisplaySetting(const PanelConfig& config) {
  const int64_t maximum_per_data_lane_megabit_per_second =
      (config.maximum_per_data_lane_bit_per_second() + 500'000) / 1'000'000;
  return display_setting_t{
      .lane_num = static_cast<uint32_t>(config.dphy_data_lane_count),
      .bit_rate_max = static_cast<uint32_t>(maximum_per_data_lane_megabit_per_second),
      .lcd_clock = static_cast<uint32_t>(config.display_timing.pixel_clock_frequency_khz * 1000),
      .h_active = static_cast<uint32_t>(config.display_timing.horizontal_active_px),
      .v_active = static_cast<uint32_t>(config.display_timing.vertical_active_lines),
      .h_period = static_cast<uint32_t>(config.display_timing.horizontal_total_px()),
      .v_period = static_cast<uint32_t>(config.display_timing.vertical_total_lines()),
      .hsync_width = static_cast<uint32_t>(config.display_timing.horizontal_sync_width_px),
      .hsync_bp = static_cast<uint32_t>(config.display_timing.horizontal_back_porch_px),
      .hsync_pol =
          config.display_timing.hsync_polarity == display::SyncPolarity::kPositive ? 1u : 0u,
      .vsync_width = static_cast<uint32_t>(config.display_timing.vertical_sync_width_lines),
      .vsync_bp = static_cast<uint32_t>(config.display_timing.vertical_back_porch_lines),
      .vsync_pol =
          config.display_timing.vsync_polarity == display::SyncPolarity::kPositive ? 1u : 0u,
  };
}

}  // namespace amlogic_display
