// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/tests/basic_test.h"

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <fuchsia/media/cpp/fidl.h>
#include <lib/fdio/fdio.h>
#include <lib/syslog/cpp/macros.h>

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <string_view>

namespace media::audio::drivers::test {

constexpr bool kLogGainValues = false;
void LogGainState(std::string_view prologue,
                  const fuchsia::hardware::audio::GainState& gain_state) {
  if constexpr (kLogGainValues) {
    FX_LOGS(INFO)
        << prologue
        << (gain_state.has_gain_db() ? std::to_string(gain_state.gain_db()) : "UNSPECIFIED")
        << " dB, muted is "
        << (gain_state.has_muted() ? (gain_state.muted() ? "true" : "false") : "UNSPECIFIED")
        << ", AGC is "
        << (gain_state.has_agc_enabled() ? (gain_state.agc_enabled() ? "enabled" : "disabled")
                                         : "UNSPECIFIED");
  }
}

void BasicTest::TearDown() {
  // Restore previous_gain_state_, if we changed the gain in this test case.
  if (stream_config().is_bound() && previous_gain_state_ && changed_gain_state_) {
    LogGainState("Restoring previous gain: ", *previous_gain_state_);
    stream_config()->SetGain(std::move(*previous_gain_state_));
    previous_gain_state_.reset();
  }

  TestBase::TearDown();
}

// Requests on protocols that are composesd into StreamConfig/Dai/Codec/Composite.
//
// fuchsia.hardware.audio.Health
// We expect a response, and we allow 'healthy' to be either unspecified or TRUE.
void BasicTest::RequestHealthState() {
  GetHealthState(AddCallback("GetHealthState", [](fuchsia::hardware::audio::HealthState state) {
    EXPECT_TRUE(!state.has_healthy() || state.healthy());
  }));
  ExpectCallbacks();
}

void BasicTest::GetHealthState(fuchsia::hardware::audio::Health::GetHealthStateCallback cb) {
  if (device_entry().isStreamConfig()) {
    stream_config()->GetHealthState(std::move(cb));
  } else if (device_entry().isDai()) {
    dai()->GetHealthState(std::move(cb));
  } else if (device_entry().isCodec()) {
    codec()->GetHealthState(std::move(cb));
  } else if (device_entry().isComposite()) {
    composite()->GetHealthState(std::move(cb));
  }
}

// fuchsia.hardware.audio.signalprocessing
// TODO(fxbug.dev/126734): Add testing for SignalProcessing methods.
// Proposed test cases listed below:
//
// BasicTest cases:
// SignalProcessingSupport
//    SignalProcessingConnector::SignalProcessingConnect returns and does not close channel.
//    child protocol channel stays bound if supported, and closes with ZX_ERR_NOT_SUPPORTED if not.
// SignalProcessingElements
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If GetElements closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    GetElements returns a vector with [1,64] entries.
//    Implies that GetTopologies must return a non-empty vector.
//    For each element:
//      id and type are required.
//      ElementType matches the TypeSpecificElement.
//      Save the elements in a set, for recognition in later cases.
// SignalProcessingTopologies
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If GetTopologies closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    GetTopologies returns a vector with [1,64] entries
//    Implies that GetElements must return a non-empty vector.
//    WatchTopology returns a value that is in the range returned by GetTopologies.
//    For each topology element:
//        id and processing_elements_edge_pairs are required.
//    For each processing_elements_edge_pairs entry:
//        processing_element_id_from and processing_element_id_to are both known (in elements set).
// WatchTopologyWhilePending
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If GetTopologies closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    GetTopologies returns a vector with [1,64] entries
//    WatchTopology returns a value that is in the range returned by GetTopologies.
//    WatchTopology (again) closes the protocol channel with ZX_ERR_BAD_STATE
// InitialElementState
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If WatchElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState immediately returns when initially called.
//    Callback contains a valid complete ElementState that matches the ElementType.
// WatchElementStateBadId
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    WatchElementState(badId) returns ZX_ERR_INVALID_ARGS and does not close. Fail on other error.
// WatchElementStateWhilePending
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If WatchElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState immediately returns when initially called.
//    WatchElementState (again) closes the protocol channel with ZX_ERR_BAD_STATE

// AdminTest cases:
// SetTopologySupported
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve topologies. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetTopology returns callback.
//    WatchTopology acknowledges the change made by SetTopology.
// SetTopologyBadId
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve topologies. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetTopology(badId) returns ZX_ERR_INVALID_ARGS, does not close channel. Fail on other error.
//    WatchTopology does not return.
// SetTopologyInvalidated
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve topologies. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    First make a change that invalidates the SignalProcessing configuration, then
//    WatchTopology should ... return ZX_ERR_BAD_STATE and not close channel?
//    SetTopology should return ZX_ERR_BAD_STATE and not close channel.
// SetTopologyReconfigured
//    If SignalProcessingConnect not supported earlier, SKIP.
//    First invalidate the SignalProcessing configuration, then retrieve the new topologies.
//    SetTopology returns callback (does not fail or close channel).
//    WatchTopology acknowledges the change made by SetTopology.
// SetElementState
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If SetElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetElementState returns callback.  Any other observable state?
// SetElementStateElementSpecific
//    Detailed checks of specific input or output fields that are unique to the element type.
//    ... likely multiple test cases here, one for each ElementType
// SetElementStateNoChange
//    If SignalProcessingConnect not supported earlier, SKIP.
//    If SetElementState closes channel with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    SetElementState does not returns callback, trigger WatchElementStateChange or close channel.
// SetElementStateBadId
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetElementState(badId) returns ZX_ERR_INVALID_ARGS, not close channel. Fail on other error.
// SetElementStateBadValues
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    SetElementState(badVal) returns ZX_ERR_INVALID_ARGS, not close channel. Fail on other error.
// SetElementStateInvalidated
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    First make a change that invalidates the SignalProcessing configuration, then
//    SetElementState should return ZX_ERR_BAD_STATE and not close channel.
// SetElementStateReconfigured
//    If SignalProcessingConnect not supported earlier, SKIP.
//    First invalidate the SignalProcessing configuration, then retrieve new elements/topologies.
//    SetElementState returns callback (does not fail or close channel).
// WatchElementStateChange
//    If SignalProcessingConnect not supported earlier, SKIP.
//    Retrieve elements. If closes with ZX_ERR_NOT_SUPPORTED, SKIP. Fail on any other error.
//    Else set a static var for this driver instance that SignalProcessing is supported.
//    WatchElementState pends until SetElementState is called.
//    Upon change, returns callback with values that match SetElementState.

// Codec channel requests
//
// TODO(fxbug.dev/124865): Add tests for Codec protocol methods.
// Proposed test cases listed below:
//
// BasicTest cases:
// GetProperties -
//    Codec::GetProperties must return callback.  plug_detect_capabilities is required.
//    Shareable with Dai::GetDaiFormats?
//
// InitialPlugState
//    Codec::WatchPlugState immediately return from first call.
//    .plug_state and .plug_change_time are always required.
//    Returned value must match GetProperties.plug_detect_capabilities.
// SubsequentPlugState (only for virtual drivers where we can inject plug changes)
//    WatchPlugState doesn't return until VAD plug state change.
//    .plug_state and .plug_change_time are always required.
//    Returned value must match GetProperties.plug_detect_capabilities.
//    .plug_change_time > time of VAD plug state change, but < now.
//    Presumably this is OK for a BasicTest to change, since this is an ephemeral virtual instance.
//
// GetFormats
//    Codec::GetDaiFormats returns a vector with [1, 64] DaiSupportedFormats entries.
//    For each entry:
//      .number_of_channels has at least 1 value, these are distinct (and increasing?).
//      .sample_formats has at least 1 value, these are distinct.
//      .frame_formats has at least 1 value, these are distinct.
//      .frame_rates has at least 1 value, these are distinct and increasing.
//      .bits_per_slot has at least 1 value, these are distinct and increasing.
//      .bits_per_sample has at least 1 value, these are distinct and increasing.
//      .bits_per_sample <= .bits_per_slot max value.
//    (Is there ANY scenario in which we expect ERROR instead of valid response?)
//
// CheckIsBridgeable
//    Codec::IsBridgeable returns a callback, with `supports_bridged_mode` that we save for later.

// AdminTest cases:
// SetBridgeModeFalse
//    Codec::SetBridgeMode(false) returns ZX_OK and does not close channel.
// SetBridgeModeTrue
//    GTEST_SKIP if Codec::IsBridgeable returned FALSE earlier.
//    Else, Codec::SetBridgeMode(true) returns ZX_OK and does not close channel.
//    (Does any other observable change occur?)
//    Presumably this is OK for a BasicTest to change, as long as we restore the previous value.
//
// SetFormatSupported
//    Codec::SetDaiFormat returns a CodecFormatInfo that matches the earlier DaiSupportedFormats.
//    Is this OK for a BasicTest to change? We can't retrieve/restore the previous state.
// SetFormatUnsupported
//    Codec::SetDaiFormat returns the expected zx_status_t (what is expected?).
//    Codec should still be usable, after an error is returned.
//
// Reset
//    Codec::Reset returns a callback.
//    (Does this reset SignalProcessing state?  What other observable state is reset?)
//    Shareable with Dai::Reset?
//    Is this OK for a BasicTest to change? We can't retrieve/restore the previous state.
//
// Start
//    Codec::Start returns a callback, with `start_time` > when Start was called, but < now.
//    (Does any other observable change occur?)
//    Is this OK for a BasicTest to change? We can't retrieve/restore the previous state.
// StartWhileStarted
//    (We should define what we expect in this error case. Return callback? Don't close channel?)
//    Codec should still be usable, after an error is returned.
//
// Stop
//    Codec::Stop returns a callback, with `stop_time` > when Stop was called, but < now.
//    (Does any other observable change occur?)
//    Is this OK for a BasicTest to change? We can't retrieve/restore the previous state.
// StopWhileStopped
//    Codec::Stop returns a callback, and does not close channel.

// Stream channel requests
//
// Request stream properties including unique ID (which must be unique between input and output).
// TODO(mpuryear): actually ensure that this differs between input and output.
void BasicTest::RequestStreamProperties() {
  stream_config()->GetProperties(AddCallback(
      "StreamConfig::GetProperties", [this](fuchsia::hardware::audio::StreamProperties prop) {
        stream_props_ = std::move(prop);

        if (stream_props_->has_unique_id()) {
          char id_buf[2 * kUniqueIdLength + 1];
          std::snprintf(id_buf, sizeof(id_buf),
                        "%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x",
                        stream_props_->unique_id()[0], stream_props_->unique_id()[1],
                        stream_props_->unique_id()[2], stream_props_->unique_id()[3],
                        stream_props_->unique_id()[4], stream_props_->unique_id()[5],
                        stream_props_->unique_id()[6], stream_props_->unique_id()[7],
                        stream_props_->unique_id()[8], stream_props_->unique_id()[9],
                        stream_props_->unique_id()[10], stream_props_->unique_id()[11],
                        stream_props_->unique_id()[12], stream_props_->unique_id()[13],
                        stream_props_->unique_id()[14], stream_props_->unique_id()[15]);
          FX_LOGS(DEBUG) << "Received unique_id " << id_buf;
        }

        ASSERT_TRUE(stream_props_->has_is_input());
        if (driver_type() == DriverType::StreamConfigInput) {
          ASSERT_TRUE(prop.is_input());
        } else {
          ASSERT_FALSE(prop.is_input());
        }

        if (stream_props_->has_can_mute()) {
          *stream_props_->mutable_can_mute() = stream_props_->can_mute();
        }
        if (stream_props_->has_can_agc()) {
          *stream_props_->mutable_can_agc() = stream_props_->can_agc();
        }

        ASSERT_TRUE(stream_props_->has_min_gain_db());
        ASSERT_TRUE(stream_props_->has_max_gain_db());
        ASSERT_TRUE(stream_props_->has_gain_step_db());
        ASSERT_TRUE(stream_props_->min_gain_db() <= stream_props_->max_gain_db());
        ASSERT_TRUE(stream_props_->gain_step_db() >= 0);
        if (stream_props_->max_gain_db() > stream_props_->min_gain_db()) {
          EXPECT_GE(stream_props_->gain_step_db(), 0.0f);
        } else {
          EXPECT_EQ(stream_props_->gain_step_db(), 0.0f);
        }

        ASSERT_TRUE(stream_props_->has_plug_detect_capabilities());

        if (stream_props_->has_manufacturer()) {
          FX_LOGS(DEBUG) << "Received manufacturer " << stream_props_->manufacturer();
        }
        if (stream_props_->has_product()) {
          FX_LOGS(DEBUG) << "Received product " << stream_props_->product();
        }

        ASSERT_TRUE(stream_props_->has_clock_domain());
      }));
  ExpectCallbacks();
}

// Fail if the returned formats are not complete, unique and within ranges.
void BasicTest::ValidateFormatCorrectness() {
  for (size_t i = 0; i < ring_buffer_pcm_formats().size(); ++i) {
    SCOPED_TRACE(testing::Message() << "ring_buffer_pcm_format[" << i << "]");
    auto& format_set = ring_buffer_pcm_formats()[i];

    ASSERT_TRUE(format_set.has_channel_sets());
    ASSERT_TRUE(format_set.has_sample_formats());
    ASSERT_TRUE(format_set.has_bytes_per_sample());
    ASSERT_TRUE(format_set.has_valid_bits_per_sample());
    ASSERT_TRUE(format_set.has_frame_rates());

    ASSERT_FALSE(format_set.channel_sets().empty());
    ASSERT_FALSE(format_set.sample_formats().empty());
    ASSERT_FALSE(format_set.bytes_per_sample().empty());
    ASSERT_FALSE(format_set.valid_bits_per_sample().empty());
    ASSERT_FALSE(format_set.frame_rates().empty());

    EXPECT_LE(format_set.channel_sets().size(), fuchsia::hardware::audio::MAX_COUNT_CHANNEL_SETS);
    EXPECT_LE(format_set.sample_formats().size(),
              fuchsia::hardware::audio::MAX_COUNT_SUPPORTED_SAMPLE_FORMATS);
    EXPECT_LE(format_set.bytes_per_sample().size(),
              fuchsia::hardware::audio::MAX_COUNT_SUPPORTED_BYTES_PER_SAMPLE);
    EXPECT_LE(format_set.valid_bits_per_sample().size(),
              fuchsia::hardware::audio::MAX_COUNT_SUPPORTED_VALID_BITS_PER_SAMPLE);
    EXPECT_LE(format_set.frame_rates().size(), fuchsia::hardware::audio::MAX_COUNT_SUPPORTED_RATES);

    for (size_t j = 0; j < format_set.channel_sets().size(); ++j) {
      SCOPED_TRACE(testing::Message() << "channel_set[" << j << "]");
      auto& channel_set = format_set.channel_sets()[j];

      ASSERT_TRUE(channel_set.has_attributes());
      ASSERT_FALSE(channel_set.attributes().empty());
      EXPECT_LE(channel_set.attributes().size(),
                fuchsia::hardware::audio::MAX_COUNT_CHANNELS_IN_RING_BUFFER);

      // Ensure each `ChannelSet` contains a unique number of channels.
      for (size_t k = j + 1; k < format_set.channel_sets().size(); ++k) {
        size_t other_channel_set_size = format_set.channel_sets()[k].attributes().size();
        EXPECT_NE(channel_set.attributes().size(), other_channel_set_size)
            << "same channel count as channel_set[" << k << "]: " << other_channel_set_size;
      }

      for (size_t k = 0; k < channel_set.attributes().size(); ++k) {
        SCOPED_TRACE(testing::Message() << "attributes[" << k << "]");
        auto& attribs = channel_set.attributes()[k];

        // Ensure channel_set.attributes are within the required range.
        if (attribs.has_min_frequency()) {
          EXPECT_LT(attribs.min_frequency(), fuchsia::media::MAX_PCM_FRAMES_PER_SECOND);
        }
        if (attribs.has_max_frequency()) {
          EXPECT_GT(attribs.max_frequency(), fuchsia::media::MIN_PCM_FRAMES_PER_SECOND);
          EXPECT_LE(attribs.max_frequency(), fuchsia::media::MAX_PCM_FRAMES_PER_SECOND);
          if (attribs.has_min_frequency()) {
            EXPECT_LE(attribs.min_frequency(), attribs.max_frequency());
          }
        }
      }
    }

    // Ensure sample_formats are unique.
    for (size_t j = 0; j < format_set.sample_formats().size(); ++j) {
      for (size_t k = j + 1; k < format_set.sample_formats().size(); ++k) {
        EXPECT_NE(static_cast<uint16_t>(fidl::ToUnderlying(format_set.sample_formats()[j])),
                  static_cast<uint16_t>(fidl::ToUnderlying(format_set.sample_formats()[k])))
            << "sample_formats[" << j << "] ("
            << static_cast<uint16_t>(fidl::ToUnderlying(format_set.sample_formats()[j]))
            << ") must not equal sample_formats[" << k << "] ("
            << static_cast<uint16_t>(fidl::ToUnderlying(format_set.sample_formats()[k])) << ")";
      }
    }

    // Ensure bytes_per_sample are unique and listed in ascending order.
    uint16_t max_bytes = 0;
    for (size_t j = 0; j < format_set.bytes_per_sample().size(); ++j) {
      EXPECT_GT(format_set.bytes_per_sample()[j], 0)
          << "bytes_per_sample[" << j << "] ("
          << static_cast<uint16_t>(format_set.bytes_per_sample()[j])
          << ") must be greater than zero";
      if (j > 0) {
        EXPECT_GT(format_set.bytes_per_sample()[j], format_set.bytes_per_sample()[j - 1])
            << "bytes_per_sample[" << j << "] ("
            << static_cast<uint16_t>(format_set.bytes_per_sample()[j])
            << ") must exceed bytes_per_sample[" << j - 1 << "] ("
            << static_cast<uint16_t>(format_set.bytes_per_sample()[j - 1]) << ")";
      }
      max_bytes = std::max(max_bytes, static_cast<uint16_t>(format_set.bytes_per_sample()[j]));
    }

    // Ensure valid_bits_per_sample are unique and listed in ascending order.
    for (size_t j = 0; j < format_set.valid_bits_per_sample().size(); ++j) {
      EXPECT_GT(format_set.valid_bits_per_sample()[j], 0)
          << "valid_bits_per_sample[" << j << "] ("
          << static_cast<uint16_t>(format_set.valid_bits_per_sample()[j])
          << ") must be greater than zero";
      if (j > 0) {
        EXPECT_GT(format_set.valid_bits_per_sample()[j], format_set.valid_bits_per_sample()[j - 1])
            << "valid_bits_per_sample[" << j << "] ("
            << static_cast<uint16_t>(format_set.valid_bits_per_sample()[j])
            << ") must exceed than valid_bits_per_sample[" << j - 1 << "] ("
            << static_cast<uint16_t>(format_set.valid_bits_per_sample()[j - 1]) << ")";
      }
      EXPECT_LE(format_set.valid_bits_per_sample()[j], max_bytes * 8)
          << "valid_bits_per_sample[" << j << "] ("
          << static_cast<uint16_t>(format_set.valid_bits_per_sample()[j])
          << ") must fit into the maximum bytes_per_sample (" << max_bytes << ")";
    }

    // Ensure frame_rates are in range and unique and listed in ascending order.
    for (size_t j = 0; j < format_set.frame_rates().size(); ++j) {
      EXPECT_GE(format_set.frame_rates()[j], fuchsia::media::MIN_PCM_FRAMES_PER_SECOND)
          << "frame_rates[" << j << "] (" << format_set.frame_rates()[j]
          << ") cannot be less than MIN_PCM_FRAMES_PER_SECOND ("
          << fuchsia::media::MIN_PCM_FRAMES_PER_SECOND << ")";
      EXPECT_LE(format_set.frame_rates()[j], fuchsia::media::MAX_PCM_FRAMES_PER_SECOND)
          << "frame_rates[" << j << "] (" << format_set.frame_rates()[j]
          << ") cannot exceed MAX_PCM_FRAMES_PER_SECOND ("
          << fuchsia::media::MAX_PCM_FRAMES_PER_SECOND << ")";

      if (j > 0) {
        EXPECT_GT(format_set.frame_rates()[j], format_set.frame_rates()[j - 1])
            << "frame_rates[" << j << "] (" << format_set.frame_rates()[j]
            << ") must exceed frame_rates[" << j - 1 << "] (" << format_set.frame_rates()[j - 1]
            << ")";
      }
    }
  }
}

// Request that the driver return its gain capabilities and current state, expecting a response.
void BasicTest::WatchGainStateAndExpectUpdate() {
  ASSERT_TRUE(stream_props_);

  // We reconnect the stream every time we run a test, and by driver interface definition the driver
  // must reply to the first watch request, so we get gain state by issuing a watch FIDL call.
  stream_config()->WatchGainState(
      AddCallback("WatchGainState", [this](fuchsia::hardware::audio::GainState gain_state) {
        EXPECT_TRUE(gain_state.has_gain_db());
        EXPECT_GE(gain_state.gain_db(), stream_props_->min_gain_db());
        EXPECT_LE(gain_state.gain_db(), stream_props_->max_gain_db());

        // If we're muted, then we must be capable of muting.
        EXPECT_TRUE(!gain_state.has_muted() || !gain_state.muted() || stream_props_->can_mute());
        // If AGC is enabled, then we must be capable of AGC.
        EXPECT_TRUE(!gain_state.has_agc_enabled() || !gain_state.agc_enabled() ||
                    stream_props_->can_agc());

        LogGainState((previous_gain_state_ ? "Storing previous gain: " : "Received new gain: "),
                     gain_state);
        if (!previous_gain_state_) {
          previous_gain_state_ = std::move(gain_state);
        }
      }));
  ExpectCallbacks();
}

// Request that the driver return its current gain state, expecting no response (no change).
void BasicTest::WatchGainStateAndExpectNoUpdate() {
  ASSERT_TRUE(stream_props_);
  ASSERT_TRUE(previous_gain_state_);
  stream_config()->WatchGainState([](fuchsia::hardware::audio::GainState gain_state) {
    LogGainState("Unexpected gain update received: ", gain_state);
    FAIL();
  });
}

// Determine an appropriate gain state to request, then call other method to request that driver set
// gain. This method assumes that the driver already successfully responded to a GetInitialGainState
// request. If this device's gain is fixed and cannot be changed, then SKIP the test.
void BasicTest::RequestSetGain() {
  // We reconnect the stream every time we run a test. By interface definition a driver must reply
  // to the first watch request, so we get gain state by issuing a watch FIDL call before this case.
  ASSERT_TRUE(stream_props_);
  ASSERT_TRUE(previous_gain_state_);

  if (stream_props_->max_gain_db() == stream_props_->min_gain_db() && !stream_props_->can_mute() &&
      !stream_props_->can_agc()) {
    GTEST_SKIP() << "*** Audio " << driver_type() << " has fixed gain ("
                 << previous_gain_state_->gain_db()
                 << " dB) and cannot MUTE or AGC. Skipping SetGain test. ***";
  }
  changed_gain_state_ = true;

  // Base our new gain settings on the old ones: avoid existing values.
  fuchsia::hardware::audio::GainState set_gain_state;
  EXPECT_EQ(previous_gain_state_->Clone(&set_gain_state), ZX_OK);
  // Change to a different gain_db.
  *set_gain_state.mutable_gain_db() = stream_props_->min_gain_db();
  if (previous_gain_state_->gain_db() == stream_props_->min_gain_db()) {
    *set_gain_state.mutable_gain_db() += stream_props_->gain_step_db();
  }
  // Toggle muted if we can change it (explicitly set it to false, if we can't).
  *set_gain_state.mutable_muted() =
      stream_props_->can_mute() && !(set_gain_state.has_muted() && set_gain_state.muted());

  // Toggle AGC if we can change it (explicitly set it to false, if we can't).
  *set_gain_state.mutable_agc_enabled() =
      stream_props_->can_agc() &&
      !(set_gain_state.has_agc_enabled() && set_gain_state.agc_enabled());

  LogGainState("Setting gain: ", set_gain_state);
  stream_config()->SetGain(std::move(set_gain_state));
}

// Request that the driver return its current plug state, expecting a valid response.
void BasicTest::WatchPlugStateAndExpectUpdate() {
  // Since we reconnect to the audio stream every time we run this test and we are guaranteed by
  // the audio driver interface definition that the driver will reply to the first watch request,
  // we can get the plug state by issuing a watch FIDL call.
  stream_config()->WatchPlugState(
      AddCallback("WatchPlugState", [this](fuchsia::hardware::audio::PlugState state) {
        plug_state_ = std::move(state);

        ASSERT_TRUE(plug_state_.has_plugged());
        if (!plug_state_.plugged()) {
          ASSERT_TRUE(stream_props_);
          ASSERT_TRUE(stream_props_->has_plug_detect_capabilities());
          EXPECT_NE(stream_props_->plug_detect_capabilities(),
                    fuchsia::hardware::audio::PlugDetectCapabilities::HARDWIRED)
              << "Device reported plug capabilities as HARDWIRED, but now reports as unplugged";
        }

        EXPECT_TRUE(plug_state_.has_plug_state_time());
        EXPECT_LT(plug_state_.plug_state_time(), zx::clock::get_monotonic().get());
        FX_LOGS(DEBUG) << "Plug_state_time: " << plug_state_.plug_state_time();
      }));
  ExpectCallbacks();
}

// Request that the driver return its current plug state, expecting no response (no change).
void BasicTest::WatchPlugStateAndExpectNoUpdate() {
  stream_config()->WatchPlugState([](fuchsia::hardware::audio::PlugState state) {
    FAIL() << "Unexpected plug update received";
  });
}

#define DEFINE_BASIC_TEST_CLASS(CLASS_NAME, CODE)                               \
  class CLASS_NAME : public BasicTest {                                         \
   public:                                                                      \
    explicit CLASS_NAME(const DeviceEntry& dev_entry) : BasicTest(dev_entry) {} \
    void TestBody() override { CODE }                                           \
  }

// Test cases that target each of the various Stream channel commands

// Verify the driver responds to the GetHealthState query.
DEFINE_BASIC_TEST_CLASS(Health, { RequestHealthState(); });

// Verify a valid unique_id, manufacturer, product and gain capabilities is successfully received.
DEFINE_BASIC_TEST_CLASS(StreamProperties, { RequestStreamProperties(); });

// Verify the initial WatchGainState responses are successfully received.
DEFINE_BASIC_TEST_CLASS(GetInitialGainState, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestStreamProperties());

  WatchGainStateAndExpectUpdate();
  WaitForError();
});

// Verify that no response is received, for a subsequent WatchGainState request.
DEFINE_BASIC_TEST_CLASS(WatchGainSecondTimeNoResponse, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestStreamProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchGainStateAndExpectUpdate());

  WatchGainStateAndExpectNoUpdate();
  WaitForError();
});

// Verify valid set gain responses are successfully received.
DEFINE_BASIC_TEST_CLASS(SetGain, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestStreamProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchGainStateAndExpectUpdate());

  RequestSetGain();
  WaitForError();
});

// Verify that format-retrieval responses are successfully received and are complete and valid.
DEFINE_BASIC_TEST_CLASS(FormatCorrectness, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestStreamProperties());
  ASSERT_NO_FAILURE_OR_SKIP(RequestFormats());

  ValidateFormatCorrectness();
  WaitForError();
});

// Verify that a valid initial plug detect response is successfully received.
DEFINE_BASIC_TEST_CLASS(GetInitialPlugState, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestStreamProperties());

  WatchPlugStateAndExpectUpdate();
  WaitForError();

  // Someday: determine how to trigger the driver's internal hardware-detect mechanism, so it emits
  // unsolicited PLUG/UNPLUG events -- otherwise driver plug detect updates are not fully testable.
});

// Verify that no response is received, for a subsequent WatchPlugState request.
DEFINE_BASIC_TEST_CLASS(WatchPlugSecondTimeNoResponse, {
  ASSERT_NO_FAILURE_OR_SKIP(RequestStreamProperties());
  ASSERT_NO_FAILURE_OR_SKIP(WatchPlugStateAndExpectUpdate());

  WatchPlugStateAndExpectNoUpdate();
  WaitForError();
});

// Verify that a valid toplogy is successfully received.
DEFINE_BASIC_TEST_CLASS(Topology, { RequestTopologies(); });

// Register separate test case instances for each enumerated device
//
// See googletest/docs/advanced.md for details
#define REGISTER_BASIC_TEST(CLASS_NAME, DEVICE)                                                \
  {                                                                                            \
    testing::RegisterTest("BasicTest", TestNameForEntry(#CLASS_NAME, DEVICE).c_str(), nullptr, \
                          DevNameForEntry(DEVICE).c_str(), __FILE__, __LINE__,                 \
                          [&]() -> BasicTest* { return new CLASS_NAME(DEVICE); });             \
  }

void RegisterBasicTestsForDevice(const DeviceEntry& device_entry) {
  if (device_entry.isStreamConfig()) {
    REGISTER_BASIC_TEST(Health, device_entry);
    REGISTER_BASIC_TEST(StreamProperties, device_entry);
    REGISTER_BASIC_TEST(GetInitialGainState, device_entry);
    REGISTER_BASIC_TEST(WatchGainSecondTimeNoResponse, device_entry);
    REGISTER_BASIC_TEST(SetGain, device_entry);
    REGISTER_BASIC_TEST(FormatCorrectness, device_entry);
    REGISTER_BASIC_TEST(GetInitialPlugState, device_entry);
    REGISTER_BASIC_TEST(WatchPlugSecondTimeNoResponse, device_entry);
  } else if (device_entry.isDai()) {
    REGISTER_BASIC_TEST(Health, device_entry);
  } else if (device_entry.isCodec()) {
    REGISTER_BASIC_TEST(Health, device_entry);
  } else if (device_entry.isComposite()) {
    REGISTER_BASIC_TEST(Health, device_entry);
    REGISTER_BASIC_TEST(Topology, device_entry);
  } else {
    FAIL() << "Unknown device type for entry '" << device_entry.filename << "'";
  }
  // TODO(fxbug.dev/124865): Add testing for Dai protocol methods (specifically Reset,
  //   GetProperties, GetDaiFormats and GetRingBufferFormats).
  // TODO(fxbug.dev/126734): Add testing for SignalProcessing methods.
  // TODO(fxbug.dev/124865): Add testing for Composite protocol methods.
  // TODO(fxbug.dev/124865): Add tests for Codec protocol methods (specifically Reset, Start, Stop,
  //   IsBridgeable, SetBridgeMode, GetDaiFormats, SetDaiFormat, WatchPlugState and GetProperties).
  // TODO(b/302704556): Add tests for Watch-while-still-pending (specifically WatchGainState,
  //   WatchPlugState, WatchClockRecoveryPositionInfo, WatchDelayInfo, WatchElementState and
  //   WatchTopology).
}

}  // namespace media::audio::drivers::test
