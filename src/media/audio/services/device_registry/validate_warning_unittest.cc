// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <cmath>
#include <cstdint>
#include <optional>
#include <set>
#include <vector>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/signal_processing_utils.h"
#include "src/media/audio/services/device_registry/signal_processing_utils_unittest.h"
#include "src/media/audio/services/device_registry/validate.h"

// These cases unittest the Validate... functions with inputs that cause WARNING log output.

namespace media_audio {
namespace {

namespace fha = fuchsia_hardware_audio;
namespace fhasp = fuchsia_hardware_audio_signalprocessing;

// Negative-test ValidateStreamProperties
fha::StreamProperties ValidStreamProperties() {
  return {{
      .is_input = false,
      .min_gain_db = 0.0f,
      .max_gain_db = 0.0f,
      .gain_step_db = 0.0f,
      .plug_detect_capabilities = fha::PlugDetectCapabilities::kCanAsyncNotify,
      .clock_domain = fha::kClockDomainMonotonic,
  }};
}
TEST(ValidateWarningTest, StreamPropertiesInvalid) {
  auto stream_properties = ValidStreamProperties();
  ASSERT_TRUE(ValidateStreamProperties(stream_properties)) << "Baseline setup unsuccessful";

  // missing is_input
  stream_properties = ValidStreamProperties();
  stream_properties.is_input() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // missing min_gain_db
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // bad min_gain_db (NAN, inf)
  stream_properties.min_gain_db() = NAN;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties.min_gain_db() = -INFINITY;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // missing max_gain_db
  stream_properties = ValidStreamProperties();
  stream_properties.max_gain_db() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // bad max_gain_db (NAN, inf)
  stream_properties.max_gain_db() = NAN;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties.max_gain_db() = INFINITY;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // bad max_gain_db (below min_gain_db)
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = 0.0f;
  stream_properties.max_gain_db() = -1.0f;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // missing gain_step_db
  stream_properties = ValidStreamProperties();
  stream_properties.gain_step_db() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // bad gain_step_db (NAN, inf)
  stream_properties.gain_step_db() = NAN;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties.gain_step_db() = INFINITY;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // gain_step_db too large (max-min)
  stream_properties.gain_step_db() = 1.0f;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = -42.0f;
  stream_properties.max_gain_db() = 26.0f;
  stream_properties.gain_step_db() = 68.1f;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // current mute state impossible (implicit)
  stream_properties = ValidStreamProperties();
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.muted = true, .gain_db = 0.0f}}));
  // current mute state impossible (explicit)
  stream_properties.can_mute() = false;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.muted = true, .gain_db = 0.0f}}));

  // current agc state impossible (implicit)
  stream_properties = ValidStreamProperties();
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.agc_enabled = true, .gain_db = 0.0f}}));
  // current agc state impossible (explicit)
  stream_properties.can_agc() = false;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.agc_enabled = true, .gain_db = 0.0f}}));

  // current gain_db out of range
  stream_properties = ValidStreamProperties();
  EXPECT_FALSE(ValidateStreamProperties(stream_properties, fha::GainState{{.gain_db = -0.1f}}));
  EXPECT_FALSE(ValidateStreamProperties(stream_properties, fha::GainState{{.gain_db = 0.1f}}));

  // missing plug_detect_capabilities
  stream_properties = ValidStreamProperties();
  stream_properties.plug_detect_capabilities() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // current plug state impossible
  stream_properties = ValidStreamProperties();
  stream_properties.plug_detect_capabilities() = fha::PlugDetectCapabilities::kHardwired;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties, fha::GainState{{.gain_db = 0.0f}},
                                        fha::PlugState{{.plugged = false, .plug_state_time = 0}}));

  // missing clock_domain
  stream_properties = ValidStreamProperties();
  stream_properties.clock_domain() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
}

// Negative-test ValidateRingBufferFormatSets
fha::SupportedFormats CompliantFormatSet() {
  return fha::SupportedFormats{{
      .pcm_supported_formats = fha::PcmSupportedFormats{{
          .channel_sets = {{
              fha::ChannelSet{{
                  .attributes = {{
                      fha::ChannelAttributes{{
                          .min_frequency = 20,
                          .max_frequency = 20000,
                      }},
                  }},
              }},
          }},
          .sample_formats = {{fha::SampleFormat::kPcmSigned}},
          .bytes_per_sample = {{2}},
          .valid_bits_per_sample = {{16}},
          .frame_rates = {{48000}},
      }},
  }};
}
TEST(ValidateWarningTest, SupportedFormatsInvalid) {
  std::vector<fha::SupportedFormats> supported_formats;

  // Empty top-level vector
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.push_back(CompliantFormatSet());
  EXPECT_TRUE(ValidateRingBufferFormatSets(supported_formats));

  // No pcm_supported_formats (one supported_formats[] vector entry, but it is empty)
  supported_formats.emplace_back();
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for frame_rates
TEST(ValidateWarningTest, SupportedFormatsFrameRatesInvalid) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing frame_rates
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty frame_rates vector
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too low frame_rate
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{999}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too high frame_rate
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{192001}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Out-of-order frame_rates
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{48000, 44100}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for channel_sets
TEST(ValidateWarningTest, SupportedFormatsChannelSetsInvalid) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing channel_sets
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty channel_sets vector
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Missing attributes
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {},
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty attributes vector
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {
          .attributes = {{}},
      },
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Duplicate channel_set lengths
  // Two channel_sets entries - both with a single channel
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {{
          .attributes = {{
              {},
          }},
      }},
      {{
          .attributes = {{
              {},
          }},
      }},
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0)
      .pcm_supported_formats()
      ->channel_sets()
      ->at(0)
      .attributes()
      ->emplace_back();
  ASSERT_TRUE(ValidateRingBufferFormatSets(supported_formats));

  // Too high min_frequency
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .min_frequency = 24001,
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Min > max
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .min_frequency = 16001,
      .max_frequency = 16000,
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too high max_frequency (passes but emits WARNING, thus is in the "warning" suite)
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .max_frequency = 192000,
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for sample_formats
TEST(ValidateWarningTest, SupportedFormatsSampleFormatsInvalid) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};
  // Missing sample_formats
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty sample_formats vector
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Duplicate sample_format
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {{
      fha::SampleFormat::kPcmSigned,
      fha::SampleFormat::kPcmSigned,
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for bytes_per_sample
TEST(ValidateWarningTest, SupportedFormatsBytesPerSampleInvalid) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing bytes_per_sample
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty bytes_per_sample vector
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Out-of-order bytes_per_sample
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{4, 2}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Bad bytes_per_sample - unsigned
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fha::SampleFormat::kPcmUnsigned}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{0, 1}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{1, 2}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Bad bytes_per_sample - signed
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fha::SampleFormat::kPcmSigned}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{1, 2}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{3, 4}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{2, 8}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Bad bytes_per_sample - float
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fha::SampleFormat::kPcmFloat}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{2, 4}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{6, 8}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{4, 16}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for valid_bits_per_sample
TEST(ValidateWarningTest, SupportedFormatsValidBitsPerSampleInvalid) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty valid_bits_per_sample vector
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Out-of-order valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{16, 15}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too low valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{0, 16}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too high valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{16, 18}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateGainState
TEST(ValidateWarningTest, GainStateInvalid) {
  // empty
  EXPECT_FALSE(ValidateGainState(fha::GainState{}));

  // missing gain_db
  EXPECT_FALSE(ValidateGainState(fha::GainState{{
                                     .muted = false,
                                     .agc_enabled = false,
                                 }},
                                 std::nullopt));

  //  bad gain_db
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = NAN,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = INFINITY,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // gain_db out-of-range
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = -12.1f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = 12.1f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // bad muted (implicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = true,
                            .agc_enabled = false,
                            .gain_db = 0.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            // can_mute (optional) is missing: CANNOT mute
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // bad muted (explicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = true,
                            .agc_enabled = false,
                            .gain_db = 0.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = false,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // bad agc_enabled (implicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = true,
                            .gain_db = 0.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            // can_agc ia missing: CANNOT agc
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // bad agc_enabled (explicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = true,
                            .gain_db = 0.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = false,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
}

// Negative-test ValidatePlugState
TEST(ValidateWarningTest, PlugStateInvalid) {
  // empty
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{}));

  // missing plugged
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{{
                                     // plugged (required) is missing
                                     .plug_state_time = zx::clock::get_monotonic().get(),
                                 }},
                                 fha::PlugDetectCapabilities::kCanAsyncNotify));

  // bad plugged
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{{
                                     .plugged = false,
                                     .plug_state_time = zx::clock::get_monotonic().get(),
                                 }},
                                 fha::PlugDetectCapabilities::kHardwired));

  // missing plug_state_time
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{{
                                     .plugged = false,
                                     // plug_state_time (required) is missing
                                 }},
                                 fha::PlugDetectCapabilities::kCanAsyncNotify));

  // bad plug_state_time
  EXPECT_FALSE(
      ValidatePlugState(fha::PlugState{{
                            .plugged = true,
                            .plug_state_time = (zx::clock::get_monotonic() + zx::hour(6)).get(),
                        }},
                        fha::PlugDetectCapabilities::kHardwired));
}

// TODO(https://fxbug.dev/42069012): Negative-test ValidateDeviceInfo
// TEST(ValidateWarningTest, DeviceInfoInvalid) {}

// Negative-test ValidateRingBufferProperties
TEST(ValidateWarningTest, RingBufferPropertiesInvalid) {
  // empty
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{}));

  // missing needs_cache_flush_or_invalidate
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .turn_on_delay = 125,
      .driver_transfer_bytes = 128,
  }}));

  // bad turn_on_delay
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      .turn_on_delay = -1,
      .driver_transfer_bytes = 128,
  }}));

  // missing driver_transfer_bytes
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      .turn_on_delay = 125,
  }}));

  // TODO(b/311694769): Resolve driver_transfer_bytes lower limit: specifically is 0 allowed?
  // bad driver_transfer_bytes (too small)
  // EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
  //               .needs_cache_flush_or_invalidate = true,
  //               .turn_on_delay = 125,
  //               .driver_transfer_bytes = 0,
  //           }}),
  //           ZX_ERR_INVALID_ARGS);

  // TODO(b/311694769): Resolve driver_transfer_bytes upper limit: no limit? Soft guideline?
  // bad driver_transfer_bytes (too large)
  // EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
  //               .needs_cache_flush_or_invalidate = true,
  //               .turn_on_delay = 125,
  //               .driver_transfer_bytes = 0xFFFFFFFF,
  //           }}),
  //           ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormat
TEST(ValidateWarningTest, RingBufferFormatInvalid) {
  // missing pcm_format
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{}));

  // bad value number_of_channels
  // Is there an upper limit on number_of_channels?
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 0,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }}));

  // bad value bytes_per_sample
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 0,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 5,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }}));

  // bad value valid_bits_per_sample
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 0,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmUnsigned,
          .bytes_per_sample = 1,
          .valid_bits_per_sample = 9,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 17,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 4,
          .valid_bits_per_sample = 33,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmFloat,
          .bytes_per_sample = 4,
          .valid_bits_per_sample = 33,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmFloat,
          .bytes_per_sample = 8,
          .valid_bits_per_sample = 65,
          .frame_rate = 48000,
      }},
  }}));

  // bad value frame_rate
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 16,
          .frame_rate = 999,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 16,
          .frame_rate = 192001,
      }},
  }}));
}

// Negative-test ValidateSampleFormatCompatibility
TEST(ValidateWarningTest, FormatIncompatibility) {
  const std::set<std::pair<uint8_t, fha::SampleFormat>> kAllowedFormats{
      {1, fha::SampleFormat::kPcmUnsigned}, {2, fha::SampleFormat::kPcmSigned},
      {4, fha::SampleFormat::kPcmSigned},   {4, fha::SampleFormat::kPcmFloat},
      {8, fha::SampleFormat::kPcmFloat},
  };
  const std::vector<uint8_t> kSampleSizesToTest{
      0, 1, 2, 3, 4, 6, 8,
  };
  const std::vector<fha::SampleFormat> kSampleFormatsToTest{
      fha::SampleFormat::kPcmUnsigned,
      fha::SampleFormat::kPcmSigned,
      fha::SampleFormat::kPcmFloat,
  };

  for (auto sample_size : kSampleSizesToTest) {
    for (auto sample_format : kSampleFormatsToTest) {
      if (kAllowedFormats.find({sample_size, sample_format}) == kAllowedFormats.end()) {
        EXPECT_FALSE(ValidateSampleFormatCompatibility(sample_size, sample_format));
      }
    }
  }
}

// Negative-test ValidateRingBufferVmo
TEST(ValidateWarningTest, RingBufferVmoInvalid) {
  constexpr uint64_t kVmoContentSize = 8192;
  zx::vmo vmo;
  auto status = zx::vmo::create(kVmoContentSize, 0, &vmo);
  ASSERT_EQ(status, ZX_OK) << "could not create VMO for test input";

  constexpr uint8_t kChannelCount = 1;
  constexpr uint8_t kSampleSize = 2;
  fha::Format format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = kChannelCount,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = kSampleSize,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }};
  uint32_t num_frames = static_cast<uint32_t>(kVmoContentSize / kChannelCount / kSampleSize);

  // Bad VMO (get_size failed)
  EXPECT_FALSE(ValidateRingBufferVmo(zx::vmo(), num_frames, format));

  // bad num_frames (too large for VMO)
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames + 1, format));

  // Bad format (flagged by the encapsulated ValidateRingBufferFormat)
  format.pcm_format()->frame_rate() = 999;
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames, format));
  format.pcm_format()->frame_rate() = 192001;
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames, format));

  // Bad format (flagged by the encapsulated ValidateSampleFormatCompatibility)
  format.pcm_format()->frame_rate() = 48000;
  format.pcm_format()->sample_format() = fha::SampleFormat::kPcmFloat;
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames, format));
}

// Negative-test ValidateDelayInfo for internal_delay
TEST(ValidateWarningTest, InternalDelayInfoInvalid) {
  // empty
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{}));

  // missing internal_delay
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{{
      .external_delay = 0,
  }}));

  // bad internal_delay
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{{
      .internal_delay = -1,
  }}));
}

// Negative-test ValidateDelayInfo for external_delay
TEST(ValidateWarningTest, ExternalDelayInfoInvalid) {
  // bad external_delay
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{{
      .internal_delay = 0,
      .external_delay = -1,
  }}));
}

// Unittest ValidateCodecProperties -- the missing, minimal and maximal possibilities
TEST(ValidateWarningTest, CodecPropertiesInvalid) {
  EXPECT_FALSE(ValidateCodecProperties(fha::CodecProperties{{
      .is_input = false, .manufacturer = "manufacturer", .product = "product", .unique_id = {{}},
      // plug_detect_capabilities missing
  }})) << "missing plug_detect_capabilities";
}

// Unittest ValidateDaiFormatSets
TEST(ValidateWarningTest, DaiSupportedFormatsInvalid) {
  // Entirely empty
  EXPECT_FALSE(ValidateDaiFormatSets(std::vector<fha::DaiSupportedFormats>{}));

  // each empty
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          // .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          // .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          // .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
          //     fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          // .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          // .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          // .bits_per_sample = {16},
      }},
  }}));

  const fha::DaiSupportedFormats valid = {{
      .number_of_channels = {1},
      .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
      .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
          fha::DaiFrameFormatStandard::kI2S)},
      .frame_rates = {48000},
      .bits_per_slot = {32},
      .bits_per_sample = {16},
  }};

  // values too small
  fha::DaiSupportedFormats fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.number_of_channels({0, 1, 2}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.frame_rates({0, 48000}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_slot({0, 32}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_sample({0, 16}),
  }}));

  // values too large
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.number_of_channels({1, 2, 65}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.frame_rates({48000, 2'000'000'000}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_slot({32, 65}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_sample({16, 33}),
  }}));

  // values out of order
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.number_of_channels({2, 1}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.frame_rates({48000, 44100}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_slot({32, 16}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_sample({16, 8}),
  }}));
}

// Unittest ValidateDaiFormat
TEST(ValidateWarningTest, DaiFormatInvalid) {
  // empty
  EXPECT_FALSE(ValidateDaiFormat({{}}));

  // each missing
  EXPECT_FALSE(ValidateDaiFormat({{
      // .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));
  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      // .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));

  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      // .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));

  // Missing FrameFormat is impossible since DaiFrameFormat (and thus DaiFormat) has a custom ctor.

  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      // .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));

  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      // .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));
  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      // .bits_per_sample = 16,
  }}));

  // Values too low
  const fha::DaiFormat valid = {{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }};
  fha::DaiFormat fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.number_of_channels(0)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.channels_to_use_bitmask(0x00)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.frame_rate(0)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_slot(0).bits_per_sample(0)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_sample(0)));

  // values too large
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.number_of_channels(65)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.channels_to_use_bitmask(0x04)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.frame_rate(2'000'000'000)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_slot(kMaxSupportedDaiFormatBitsPerSlot + 1)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_sample(33)));
}

// Unittest ValidateCodecFormatInfo
TEST(ValidateWarningTest, CodecFormatInfoInvalid) {
  // These durations cannot be negative.
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .external_delay = -1,
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_on_delay = -1,
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_off_delay = -1,
  }}));
  // ...that includes INT64_MIN (check for erroneously treating it as unsigned).
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .external_delay = zx::time::infinite_past().get(),
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_on_delay = zx::time::infinite_past().get(),
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_off_delay = zx::time::infinite_past().get(),
  }}));
}

// signalprocessing functions
//
TEST(ValidateWarningTest, TopologyListInvalid) {
  EXPECT_FALSE(ValidateTopologies(kEmptyTopologies, MapElements(kElements)));

  // List contains two topologies with the same id.
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithDuplicateId, MapElements(kElements)));

  // There are elements that are not mentioned in at least one of the topologies.
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithoutAllElements, MapElements(kElements)));

  // Topology list with a bad Topology: all the ValidateTopology negative cases
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithMissingId, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithMissingEdgePairs, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithEmptyEdgePairs, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithUnknownElementId, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithLoop, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithTerminalNotEndpoint, MapElements(kElements)));

  // empty element_map
  EXPECT_FALSE(ValidateTopologies(kTopologies, kEmptyElementMap));
}

TEST(ValidateWarningTest, TopologyInvalid) {
  // This topology has no 'id'.
  EXPECT_FALSE(ValidateTopology(kTopologyMissingId, MapElements(kElements)));

  // This topology has no 'processing_elements_edge_pairs'.
  EXPECT_FALSE(ValidateTopology(kTopologyMissingEdgePairs, MapElements(kElements)));

  // This topology has an 'processing_elements_edge_pairs' vector that is empty.
  EXPECT_FALSE(ValidateTopology(kTopologyEmptyEdgePairs, MapElements(kElements)));

  // This topology references an element_id that is not included in the element_map.
  EXPECT_FALSE(ValidateTopology(kTopologyUnknownElementId, MapElements(kElements)));

  // This topology includes an edge that connects one element_id to itself.
  EXPECT_FALSE(ValidateTopology(kTopologyEdgePairLoop, MapElements(kElements)));

  // This topology has a terminal (source or destination) element that is not an Endpoint.
  EXPECT_FALSE(ValidateTopology(kTopologyTerminalNotEndpoint, MapElements(kElements)));

  // empty element_map
  EXPECT_FALSE(ValidateTopology(kTopologyDaiRb, kEmptyElementMap));
}

TEST(ValidateWarningTest, ElementListInvalid) {
  EXPECT_FALSE(ValidateElements(kEmptyElements));

  // List contains two elements with the same id.
  EXPECT_FALSE(ValidateElements(kElementsDuplicateId));

  // bad Elements: all the ValidateElement negative cases
  EXPECT_FALSE(ValidateElements(kElementsWithNoId));
  EXPECT_FALSE(ValidateElements(kElementsWithNoType));
  EXPECT_FALSE(ValidateElements(kElementsWithNoRequiredTypeSpecific));
  EXPECT_FALSE(ValidateElements(kElementsWithWrongTypeSpecific));
  EXPECT_FALSE(ValidateElements(kElementsWithEmptyDescription));
}

TEST(ValidateWarningTest, ElementInvalid) {
  // This element has no 'id'.
  EXPECT_FALSE(ValidateElement(kElementNoId));

  // This element has no 'type'.
  EXPECT_FALSE(ValidateElement(kElementNoType));

  // This element has no 'type_specific', but its 'type' requires one.
  EXPECT_FALSE(ValidateElement(kElementWithoutRequiredTypeSpecific));

  // This element contains a 'type_specific' that does not match its 'type'.
  EXPECT_FALSE(ValidateElement(kElementWrongTypeSpecific));

  // This element contains a 'description' that is an empty string.
  EXPECT_FALSE(ValidateElement(kElementEmptyDescription));
}

// Test inconsistencies in certain type_specifics
// TODO(https://fxbug.dev/42069012): Negative-test ValidateElement
TEST(ValidateWarningTest, DynamicsElementInvalid) {
  {
    auto dyn_no_bands = kDynamicsElement;
    dyn_no_bands.type_specific()->dynamics()->bands(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElement(dyn_no_bands));
    EXPECT_FALSE(ValidateElement(dyn_no_bands));
  }
  {
    auto dyn_empty_bands = kDynamicsElement;
    dyn_empty_bands.type_specific()->dynamics()->bands({{}});
    EXPECT_FALSE(ValidateDynamicsElement(dyn_empty_bands));
    EXPECT_FALSE(ValidateElement(dyn_empty_bands));
  }
  {
    auto dyn_band_no_id = kDynamicsElement;
    dyn_band_no_id.type_specific()->dynamics()->bands()->at(0).id(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElement(dyn_band_no_id));
    EXPECT_FALSE(ValidateElement(dyn_band_no_id));
  }
}

TEST(ValidateWarningTest, EndpointElementInvalid) {
  {
    auto endp_no_type = kDaiEndpointElement;
    endp_no_type.type_specific()->endpoint()->type(std::nullopt);
    EXPECT_FALSE(ValidateEndpointElement(endp_no_type));
    EXPECT_FALSE(ValidateElement(endp_no_type));
  }
  {
    auto endp_no_plug_caps = kDaiEndpointElement;
    endp_no_plug_caps.type_specific()->endpoint()->plug_detect_capabilities(std::nullopt);
    EXPECT_FALSE(ValidateEndpointElement(endp_no_plug_caps));
    EXPECT_FALSE(ValidateElement(endp_no_plug_caps));
  }
}

// All the EQ-specific ways that an Element can be non-compliant
TEST(ValidateWarningTest, EqualizerElementInvalid) {
  {
    auto eq_no_bands = kEqualizerElement;
    eq_no_bands.type_specific()->equalizer()->bands(std::nullopt);
    EXPECT_FALSE(ValidateEqualizerElement(eq_no_bands));
    EXPECT_FALSE(ValidateElement(eq_no_bands));
  }
  {
    auto eq_empty_bands = kEqualizerElement;
    eq_empty_bands.type_specific()->equalizer()->bands({{}});
    EXPECT_FALSE(ValidateEqualizerElement(eq_empty_bands));
    EXPECT_FALSE(ValidateElement(eq_empty_bands));
  }
  {
    auto eq_no_min_freq = kEqualizerElement;
    eq_no_min_freq.type_specific()->equalizer()->min_frequency(std::nullopt);
    EXPECT_FALSE(ValidateEqualizerElement(eq_no_min_freq));
    EXPECT_FALSE(ValidateElement(eq_no_min_freq));
  }
  {
    auto eq_no_max_freq = kEqualizerElement;
    eq_no_max_freq.type_specific()->equalizer()->max_frequency(std::nullopt);
    EXPECT_FALSE(ValidateEqualizerElement(eq_no_max_freq));
    EXPECT_FALSE(ValidateElement(eq_no_max_freq));
  }
  {
    auto eq_max_freq_too_low = kEqualizerElement;
    eq_max_freq_too_low.type_specific()->equalizer()->min_frequency(
        *kEqualizerElement.type_specific()->equalizer()->max_frequency() + 1);
    EXPECT_FALSE(ValidateEqualizerElement(eq_max_freq_too_low));
    EXPECT_FALSE(ValidateElement(eq_max_freq_too_low));
  }
  {
    auto eq_negative_q = kEqualizerElement;
    eq_negative_q.type_specific()->equalizer()->max_q(-1.0f);
    EXPECT_FALSE(ValidateEqualizerElement(eq_negative_q));
    EXPECT_FALSE(ValidateElement(eq_negative_q));
  }
  {
    auto eq_inf_q = kEqualizerElement;
    eq_inf_q.type_specific()->equalizer()->max_q(INFINITY);
    EXPECT_FALSE(ValidateEqualizerElement(eq_inf_q));
    EXPECT_FALSE(ValidateElement(eq_inf_q));
  }
  {
    auto eq_nan_q = kEqualizerElement;
    eq_nan_q.type_specific()->equalizer()->max_q(NAN);
    EXPECT_FALSE(ValidateEqualizerElement(eq_nan_q));
    EXPECT_FALSE(ValidateElement(eq_nan_q));
  }
  {
    auto eq_no_min_gain = kEqualizerElement;
    eq_no_min_gain.type_specific()->equalizer()->min_gain_db(std::nullopt);
    EXPECT_FALSE(ValidateEqualizerElement(eq_no_min_gain));
    EXPECT_FALSE(ValidateElement(eq_no_min_gain));
  }
  {
    auto eq_inf_min_gain = kEqualizerElement;
    eq_inf_min_gain.type_specific()->equalizer()->min_gain_db(-INFINITY);
    EXPECT_FALSE(ValidateEqualizerElement(eq_inf_min_gain));
    EXPECT_FALSE(ValidateElement(eq_inf_min_gain));
  }
  {
    auto eq_nan_min_gain = kEqualizerElement;
    eq_nan_min_gain.type_specific()->equalizer()->min_gain_db(NAN);
    EXPECT_FALSE(ValidateEqualizerElement(eq_nan_min_gain));
    EXPECT_FALSE(ValidateElement(eq_nan_min_gain));
  }
  {
    auto eq_no_max_gain = kEqualizerElement;
    eq_no_max_gain.type_specific()->equalizer()->max_gain_db(std::nullopt);
    EXPECT_FALSE(ValidateEqualizerElement(eq_no_max_gain));
    EXPECT_FALSE(ValidateElement(eq_no_max_gain));
  }
  {
    auto eq_max_gain_too_low = kEqualizerElement;
    eq_max_gain_too_low.type_specific()->equalizer()->max_gain_db(
        *kEqualizerElement.type_specific()->equalizer()->min_gain_db() - 1.0f);
    EXPECT_FALSE(ValidateEqualizerElement(eq_max_gain_too_low));
    EXPECT_FALSE(ValidateElement(eq_max_gain_too_low));
  }
  {
    auto eq_inf_max_gain = kEqualizerElement;
    eq_inf_max_gain.type_specific()->equalizer()->max_gain_db(INFINITY);
    EXPECT_FALSE(ValidateEqualizerElement(eq_inf_max_gain));
    EXPECT_FALSE(ValidateElement(eq_inf_max_gain));
  }
  {
    auto eq_nan_max_gain = kEqualizerElement;
    eq_nan_max_gain.type_specific()->equalizer()->max_gain_db(NAN);
    EXPECT_FALSE(ValidateEqualizerElement(eq_nan_max_gain));
    EXPECT_FALSE(ValidateElement(eq_nan_max_gain));
  }
}

TEST(ValidateWarningTest, GainElementInvalid) {
  {
    auto gain_no_type = kGainElement;
    gain_no_type.type_specific()->gain()->type(std::nullopt);
    EXPECT_FALSE(ValidateGainElement(gain_no_type));
    EXPECT_FALSE(ValidateElement(gain_no_type));
  }
  {
    auto gain_no_min = kGainElement;
    gain_no_min.type_specific()->gain()->min_gain(std::nullopt);
    EXPECT_FALSE(ValidateGainElement(gain_no_min));
    EXPECT_FALSE(ValidateElement(gain_no_min));
  }
  {
    auto gain_inf_min = kGainElement;
    gain_inf_min.type_specific()->gain()->min_gain(-INFINITY);
    EXPECT_FALSE(ValidateGainElement(gain_inf_min));
    EXPECT_FALSE(ValidateElement(gain_inf_min));
  }
  {
    auto gain_nan_min = kGainElement;
    gain_nan_min.type_specific()->gain()->min_gain(NAN);
    EXPECT_FALSE(ValidateGainElement(gain_nan_min));
    EXPECT_FALSE(ValidateElement(gain_nan_min));
  }
  {
    auto gain_no_max = kGainElement;
    gain_no_max.type_specific()->gain()->max_gain(std::nullopt);
    EXPECT_FALSE(ValidateGainElement(gain_no_max));
    EXPECT_FALSE(ValidateElement(gain_no_max));
  }
  {
    auto gain_max_too_low = kGainElement;
    gain_max_too_low.type_specific()->gain()->max_gain(
        *kGainElement.type_specific()->gain()->min_gain() - 1.0f);
    EXPECT_FALSE(ValidateGainElement(gain_max_too_low));
    EXPECT_FALSE(ValidateElement(gain_max_too_low));
  }
  {
    auto gain_inf_max = kGainElement;
    gain_inf_max.type_specific()->gain()->max_gain(INFINITY);
    EXPECT_FALSE(ValidateGainElement(gain_inf_max));
    EXPECT_FALSE(ValidateElement(gain_inf_max));
  }
  {
    auto gain_nan_max = kGainElement;
    gain_nan_max.type_specific()->gain()->max_gain(NAN);
    EXPECT_FALSE(ValidateGainElement(gain_nan_max));
    EXPECT_FALSE(ValidateElement(gain_nan_max));
  }
  {
    auto gain_no_step = kGainElement;
    gain_no_step.type_specific()->gain()->min_gain_step(std::nullopt);
    EXPECT_FALSE(ValidateGainElement(gain_no_step));
    EXPECT_FALSE(ValidateElement(gain_no_step));
  }
  {
    auto gain_neg_step = kGainElement;
    gain_neg_step.type_specific()->gain()->min_gain_step(-1.0f);
    EXPECT_FALSE(ValidateGainElement(gain_neg_step));
    EXPECT_FALSE(ValidateElement(gain_neg_step));
  }
  {
    auto gain_step_too_large = kGainElement;
    gain_step_too_large.type_specific()->gain()->min_gain_step(
        *kGainElement.type_specific()->gain()->max_gain() -
        *kGainElement.type_specific()->gain()->min_gain() + 1.0f);
    EXPECT_FALSE(ValidateGainElement(gain_step_too_large));
    EXPECT_FALSE(ValidateElement(gain_step_too_large));
  }
  {
    auto gain_nan_step = kGainElement;
    gain_nan_step.type_specific()->gain()->min_gain_step(NAN);
    EXPECT_FALSE(ValidateGainElement(gain_nan_step));
    EXPECT_FALSE(ValidateElement(gain_nan_step));
  }
}

// ElementState tests
TEST(ValidateWarningTest, ElementStateWithMissingFields) {
  EXPECT_FALSE(ValidateElementState(kElementStateEmpty, kDaiEndpointElement));

  ASSERT_TRUE(ValidateElementState(kEndpointElementState, kDaiEndpointElement));  // Baseline

  // The `started` field is required.
  fhasp::ElementState state_without_started = kEndpointElementState;
  state_without_started.started(std::nullopt);
  EXPECT_FALSE(ValidateElementState(state_without_started, kDaiEndpointElement));

  // For this ElementType (endpoint), `type_specific` is required.
  fhasp::ElementState state_without_type_specific = kEndpointElementState;
  state_without_type_specific.type_specific(std::nullopt);
  EXPECT_FALSE(ValidateElementState(state_without_type_specific, kDaiEndpointElement));
}

// ElementState's type_specific union must match its Element's type.
TEST(ValidateWarningTest, ElementStateWithIncorrectTypeSpecificState) {
  ASSERT_TRUE(ValidateElementState(kEndpointElementState, kDaiEndpointElement));  // Baseline

  // Element is an Endpoint, but the state has an Equalizer type_specific table.
  fhasp::ElementState state_with_incorrect_type_specific = kEndpointElementState;
  state_with_incorrect_type_specific.type_specific(
      fhasp::TypeSpecificElementState::WithEqualizer({{.band_states = {{{{.id = 0}}}}}}));
  EXPECT_FALSE(ValidateElementState(state_with_incorrect_type_specific, kDaiEndpointElement));
}

// ElementState that violates the capabilities of that element.
TEST(ValidateWarningTest, ElementStateInconsistent) {
  // According to Element properties it cannot stop, but ElementState says it is stopped.
  EXPECT_FALSE(ValidateElementState(kElementStateStopped, kElementCannotStop));

  // According to Element properties it cannot bypass, but ElementState says it is bypassed.
  EXPECT_FALSE(ValidateElementState(kElementStateBypassed, kElementCannotBypass));

  // More negative tests here that are type-specific.
}

TEST(ValidateWarningTest, ElementStateWithNegativeDurations) {
  ASSERT_TRUE(ValidateElementState(kEndpointElementState, kDaiEndpointElement));  // Baseline

  // Test negative Latency here

  // `turn_on_delay` is optional, but if present then it cannot be negative.
  fhasp::ElementState state_with_negative_turn_on_delay = kEndpointElementState;
  state_with_negative_turn_on_delay.turn_on_delay(ZX_NSEC(-1));
  EXPECT_FALSE(ValidateElementState(state_with_negative_turn_on_delay, kDaiEndpointElement));

  // `turn_off_delay` is optional, but if present then it cannot be negative.
  fhasp::ElementState state_with_negative_turn_off_delay = kEndpointElementState;
  state_with_negative_turn_off_delay.turn_off_delay(ZX_NSEC(-1));
  EXPECT_FALSE(ValidateElementState(state_with_negative_turn_off_delay, kDaiEndpointElement));
}

// All the ways that a Dynamics-specific ElementState can be invalid.
TEST(ValidateWarningTest, DynamicsElementStateInvalid) {
  {
    auto dyn_state_band_states_none = kDynamicsElementState;
    dyn_state_band_states_none.type_specific()->dynamics()->band_states(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_band_states_none, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_band_states_none, kDynamicsElement));
  }
  {
    auto dyn_state_band_states_empty = kDynamicsElementState;
    dyn_state_band_states_empty.type_specific()->dynamics()->band_states({{}});
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_band_states_empty, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_band_states_empty, kDynamicsElement));
  }
  {
    auto dyn_state_id_none = kDynamicsElementState;
    dyn_state_id_none.type_specific()->dynamics()->band_states()->at(0).id(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_id_none, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_id_none, kDynamicsElement));
  }
  {
    auto dyn_state_id_unknown = kDynamicsElementState;
    dyn_state_id_unknown.type_specific()->dynamics()->band_states()->at(0).id(-1);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_id_unknown, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_id_unknown, kDynamicsElement));
  }
  {
    auto dyn_state_min_freq_none = kDynamicsElementState;
    dyn_state_min_freq_none.type_specific()->dynamics()->band_states()->at(0).min_frequency(
        std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_min_freq_none, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_min_freq_none, kDynamicsElement));
  }
  {
    auto dyn_state_max_freq_none = kDynamicsElementState;
    dyn_state_max_freq_none.type_specific()->dynamics()->band_states()->at(0).max_frequency(
        std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_max_freq_none, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_max_freq_none, kDynamicsElement));
  }
  {
    auto dyn_state_max_freq_too_low = kDynamicsElementState;
    dyn_state_max_freq_too_low.type_specific()->dynamics()->band_states()->at(0).min_frequency(
        *kDynamicsElementState.type_specific()->dynamics()->band_states()->at(0).max_frequency() +
        1);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_max_freq_too_low, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_max_freq_too_low, kDynamicsElement));
  }
  {
    auto dyn_state_threshold_db_inf = kDynamicsElementState;
    dyn_state_threshold_db_inf.type_specific()->dynamics()->band_states()->at(0).threshold_db(
        INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_threshold_db_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_threshold_db_inf, kDynamicsElement));
    dyn_state_threshold_db_inf.type_specific()->dynamics()->band_states()->at(0).threshold_db(
        -INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_threshold_db_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_threshold_db_inf, kDynamicsElement));
  }
  {
    auto dyn_state_threshold_db_nan = kDynamicsElementState;
    dyn_state_threshold_db_nan.type_specific()->dynamics()->band_states()->at(0).threshold_db(NAN);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_threshold_db_nan, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_threshold_db_nan, kDynamicsElement));
  }
  {
    auto dyn_state_threshold_type_none = kDynamicsElementState;
    dyn_state_threshold_type_none.type_specific()->dynamics()->band_states()->at(0).threshold_type(
        std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_threshold_type_none, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_threshold_type_none, kDynamicsElement));
  }
  {
    auto dyn_state_ratio_none = kDynamicsElementState;
    dyn_state_ratio_none.type_specific()->dynamics()->band_states()->at(0).ratio(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_ratio_none, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_ratio_none, kDynamicsElement));
  }
  {
    auto dyn_state_ratio_inf = kDynamicsElementState;
    dyn_state_ratio_inf.type_specific()->dynamics()->band_states()->at(0).ratio(INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_ratio_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_ratio_inf, kDynamicsElement));
    dyn_state_ratio_inf.type_specific()->dynamics()->band_states()->at(0).ratio(-INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_ratio_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_ratio_inf, kDynamicsElement));
  }
  {
    auto dyn_state_ratio_nan = kDynamicsElementState;
    dyn_state_ratio_nan.type_specific()->dynamics()->band_states()->at(0).ratio(NAN);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_ratio_nan, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_ratio_nan, kDynamicsElement));
  }
  {
    auto dyn_state_knee_neg = kDynamicsElementState;
    dyn_state_knee_neg.type_specific()->dynamics()->band_states()->at(0).knee_width_db(-1.0f);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_knee_neg, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_knee_neg, kDynamicsElement));
  }
  {
    auto dyn_state_knee_inf = kDynamicsElementState;
    dyn_state_knee_inf.type_specific()->dynamics()->band_states()->at(0).knee_width_db(INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_knee_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_knee_inf, kDynamicsElement));
  }
  {
    auto dyn_state_knee_nan = kDynamicsElementState;
    dyn_state_knee_nan.type_specific()->dynamics()->band_states()->at(0).knee_width_db(NAN);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_knee_nan, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_knee_nan, kDynamicsElement));
  }
  {
    auto dyn_state_attack_neg = kDynamicsElementState;
    dyn_state_attack_neg.type_specific()->dynamics()->band_states()->at(0).attack(ZX_USEC(-1));
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_attack_neg, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_attack_neg, kDynamicsElement));
  }
  {
    auto dyn_state_release_neg = kDynamicsElementState;
    dyn_state_release_neg.type_specific()->dynamics()->band_states()->at(0).release(ZX_USEC(-1));
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_release_neg, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_release_neg, kDynamicsElement));
  }
  {
    auto dyn_state_output_gain_inf = kDynamicsElementState;
    dyn_state_output_gain_inf.type_specific()->dynamics()->band_states()->at(0).output_gain_db(
        INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_output_gain_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_output_gain_inf, kDynamicsElement));
    dyn_state_output_gain_inf.type_specific()->dynamics()->band_states()->at(0).output_gain_db(
        -INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_output_gain_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_output_gain_inf, kDynamicsElement));
  }
  {
    auto dyn_state_output_gain_nan = kDynamicsElementState;
    dyn_state_output_gain_nan.type_specific()->dynamics()->band_states()->at(0).output_gain_db(NAN);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_output_gain_nan, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_output_gain_nan, kDynamicsElement));
  }
  {
    auto dyn_state_input_gain_inf = kDynamicsElementState;
    dyn_state_input_gain_inf.type_specific()->dynamics()->band_states()->at(0).input_gain_db(
        INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_input_gain_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_input_gain_inf, kDynamicsElement));
    dyn_state_input_gain_inf.type_specific()->dynamics()->band_states()->at(0).input_gain_db(
        -INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_input_gain_inf, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_input_gain_inf, kDynamicsElement));
  }
  {
    auto dyn_state_input_gain_nan = kDynamicsElementState;
    dyn_state_input_gain_nan.type_specific()->dynamics()->band_states()->at(0).input_gain_db(NAN);
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_input_gain_nan, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_input_gain_nan, kDynamicsElement));
  }
  {
    auto dyn_state_lookahead_neg = kDynamicsElementState;
    dyn_state_lookahead_neg.type_specific()->dynamics()->band_states()->at(0).lookahead(
        ZX_USEC(-1));
    EXPECT_FALSE(ValidateDynamicsElementState(dyn_state_lookahead_neg, kDynamicsElement));
    EXPECT_FALSE(ValidateElementState(dyn_state_lookahead_neg, kDynamicsElement));
  }
}

// All the ways that an Endpoint ElementState can be invalid.
TEST(ValidateWarningTest, EndpointElementStateInvalid) {
  {
    auto endp_state_plug_state_none = kEndpointElementState;
    endp_state_plug_state_none.type_specific()->endpoint()->plug_state(std::nullopt);
    EXPECT_FALSE(ValidateEndpointElementState(endp_state_plug_state_none, kDaiEndpointElement));
    EXPECT_FALSE(ValidateElementState(endp_state_plug_state_none, kDaiEndpointElement));
  }
  {
    auto endp_state_plugged_none = kEndpointElementState;
    endp_state_plugged_none.type_specific()->endpoint()->plug_state()->plugged(std::nullopt);
    EXPECT_FALSE(ValidateEndpointElementState(endp_state_plugged_none, kDaiEndpointElement));
    EXPECT_FALSE(ValidateElementState(endp_state_plugged_none, kDaiEndpointElement));
  }
  {
    auto endp_state_plugged_unsupported = kEndpointElementState;
    endp_state_plugged_unsupported.type_specific()->endpoint()->plug_state()->plugged(false);
    EXPECT_FALSE(ValidateEndpointElementState(endp_state_plugged_unsupported, kRingBufferElement));
    EXPECT_FALSE(ValidateElementState(endp_state_plugged_unsupported, kRingBufferElement));
  }
  {
    auto endp_state_plug_time_none = kEndpointElementState;
    endp_state_plug_time_none.type_specific()->endpoint()->plug_state()->plug_state_time(
        std::nullopt);
    EXPECT_FALSE(ValidateEndpointElementState(endp_state_plug_time_none, kDaiEndpointElement));
    EXPECT_FALSE(ValidateElementState(endp_state_plug_time_none, kDaiEndpointElement));
  }
}

// All the ways that an Equalizer ElementState can be invalid.
TEST(ValidateWarningTest, EqualizerElementStateInvalid) {
  {
    auto eq_state_band_states_none = kEqualizerElementState;
    eq_state_band_states_none.type_specific()->equalizer()->band_states(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_band_states_none, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_band_states_none, kEqualizerElement));
  }
  {
    auto eq_state_band_states_empty = kEqualizerElementState;
    eq_state_band_states_empty.type_specific()->equalizer()->band_states({{}});
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_band_states_empty, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_band_states_empty, kEqualizerElement));
  }
  {
    auto eq_state_id_none = kEqualizerElementState;
    eq_state_id_none.type_specific()->equalizer()->band_states()->at(0).id(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_id_none, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_id_none, kEqualizerElement));
  }
  {
    auto eq_state_id_unknown = kEqualizerElementState;
    eq_state_id_unknown.type_specific()->equalizer()->band_states()->at(0).id(-1);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_id_unknown, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_id_unknown, kEqualizerElement));
  }
  // Is BandState.EqualizerBandType ever required?
  // Is BandState.frequency ever required, depending on the EqualizerBandType?
  {
    auto eq_state_freq_too_low = kEqualizerElementState;
    eq_state_freq_too_low.type_specific()->equalizer()->band_states()->at(0).frequency(
        *kEqualizerElement.type_specific()->equalizer()->min_frequency());
    auto eq_element = kEqualizerElement;
    eq_element.type_specific()->equalizer()->min_frequency(
        *eq_element.type_specific()->equalizer()->min_frequency() + 1);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_freq_too_low, eq_element));
    EXPECT_FALSE(ValidateElementState(eq_state_freq_too_low, eq_element));
  }
  {
    auto eq_state_freq_too_high = kEqualizerElementState;
    eq_state_freq_too_high.type_specific()->equalizer()->band_states()->at(0).frequency(
        *kEqualizerElement.type_specific()->equalizer()->max_frequency());
    auto eq_element = kEqualizerElement;
    eq_element.type_specific()->equalizer()->max_frequency(
        *eq_element.type_specific()->equalizer()->max_frequency() - 1);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_freq_too_high, eq_element));
    EXPECT_FALSE(ValidateElementState(eq_state_freq_too_high, eq_element));
  }
  {
    auto eq_state_q_zero = kEqualizerElementState;
    eq_state_q_zero.type_specific()->equalizer()->band_states()->at(0).q(0.0f);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_q_zero, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_q_zero, kEqualizerElement));
  }
  {
    auto eq_state_q_inf = kEqualizerElementState;
    eq_state_q_inf.type_specific()->equalizer()->band_states()->at(0).q(INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_q_inf, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_q_inf, kEqualizerElement));
    eq_state_q_inf.type_specific()->equalizer()->band_states()->at(0).q(-INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_q_inf, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_q_inf, kEqualizerElement));
  }
  {
    auto eq_state_gain_nan = kEqualizerElementState;
    eq_state_gain_nan.type_specific()->equalizer()->band_states()->at(0).gain_db(NAN);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_gain_nan, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_gain_nan, kEqualizerElement));
  }
  {
    auto eq_state_gain_inf = kEqualizerElementState;
    eq_state_gain_inf.type_specific()->equalizer()->band_states()->at(0).gain_db(INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_gain_inf, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_gain_inf, kEqualizerElement));
    eq_state_gain_inf.type_specific()->equalizer()->band_states()->at(0).gain_db(-INFINITY);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_gain_inf, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_gain_inf, kEqualizerElement));
  }
  {
    auto eq_state_q_nan = kEqualizerElementState;
    eq_state_q_nan.type_specific()->equalizer()->band_states()->at(0).q(NAN);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_q_nan, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_q_nan, kEqualizerElement));
  }
  {
    auto eq_state_gain_unexpected = kEqualizerElementState;
    eq_state_gain_unexpected.type_specific()->equalizer()->band_states()->at(1).type(
        fhasp::EqualizerBandType::kNotch);
    eq_state_gain_unexpected.type_specific()->equalizer()->band_states()->at(1).gain_db(0.0f);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_gain_unexpected, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_gain_unexpected, kEqualizerElement));
  }
  {
    auto eq_state_gain_expected = kEqualizerElementState;
    eq_state_gain_expected.type_specific()->equalizer()->band_states()->at(0).type(
        fhasp::EqualizerBandType::kPeak);
    eq_state_gain_expected.type_specific()->equalizer()->band_states()->at(0).gain_db(std::nullopt);
    EXPECT_FALSE(ValidateDynamicsElementState(eq_state_gain_expected, kEqualizerElement));
    EXPECT_FALSE(ValidateElementState(eq_state_gain_expected, kEqualizerElement));
  }
}

// All the ways that a Gain ElementState can be invalid.
TEST(ValidateWarningTest, GainElementStateInvalid) {
  {
    auto gain_state_gain_none = kGainElementState;
    gain_state_gain_none.type_specific()->gain()->gain(std::nullopt);
    EXPECT_FALSE(ValidateGainElementState(gain_state_gain_none, kGainElement));
    EXPECT_FALSE(ValidateElementState(gain_state_gain_none, kGainElement));
  }
  {
    auto gain_state_gain_too_low = kGainElementState;
    gain_state_gain_too_low.type_specific()->gain()->gain(
        *kGainElement.type_specific()->gain()->min_gain() - 1.0f);
    EXPECT_FALSE(ValidateGainElementState(gain_state_gain_too_low, kGainElement));
    EXPECT_FALSE(ValidateElementState(gain_state_gain_too_low, kGainElement));
  }
  {
    auto gain_state_gain_too_high = kGainElementState;
    gain_state_gain_too_high.type_specific()->gain()->gain(
        *kGainElement.type_specific()->gain()->max_gain() + 1.0f);
    EXPECT_FALSE(ValidateGainElementState(gain_state_gain_too_high, kGainElement));
    EXPECT_FALSE(ValidateElementState(gain_state_gain_too_high, kGainElement));
  }
  {
    auto gain_state_gain_inf = kGainElementState;
    gain_state_gain_inf.type_specific()->gain()->gain(INFINITY);
    EXPECT_FALSE(ValidateGainElementState(gain_state_gain_inf, kGainElement));
    EXPECT_FALSE(ValidateElementState(gain_state_gain_inf, kGainElement));
    gain_state_gain_inf.type_specific()->gain()->gain(-INFINITY);
    EXPECT_FALSE(ValidateGainElementState(gain_state_gain_inf, kGainElement));
    EXPECT_FALSE(ValidateElementState(gain_state_gain_inf, kGainElement));
  }
  {
    auto gain_state_gain_nan = kGainElementState;
    gain_state_gain_nan.type_specific()->gain()->gain(NAN);
    EXPECT_FALSE(ValidateGainElementState(gain_state_gain_nan, kGainElement));
    EXPECT_FALSE(ValidateElementState(gain_state_gain_nan, kGainElement));
  }
}

// All the ways that a VendorSpecific ElementState can be invalid.
TEST(ValidateWarningTest, VendorSpecificElementStateInvalid) {
  {
    auto vendor_specific_vendor_specific_data_none = kVendorSpecificElementState;
    vendor_specific_vendor_specific_data_none.vendor_specific_data(std::nullopt);
    EXPECT_FALSE(ValidateVendorSpecificElementState(vendor_specific_vendor_specific_data_none,
                                                    kVendorSpecificElement));
    EXPECT_FALSE(
        ValidateElementState(vendor_specific_vendor_specific_data_none, kVendorSpecificElement));
  }
  {
    auto vendor_specific_vendor_specific_data_empty = kVendorSpecificElementState;
    vendor_specific_vendor_specific_data_empty.vendor_specific_data({{}});
    EXPECT_FALSE(ValidateVendorSpecificElementState(vendor_specific_vendor_specific_data_empty,
                                                    kVendorSpecificElement));
    EXPECT_FALSE(
        ValidateElementState(vendor_specific_vendor_specific_data_empty, kVendorSpecificElement));
  }
}

}  // namespace
}  // namespace media_audio
