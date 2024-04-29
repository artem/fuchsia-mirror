// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>

#include <re2/re2.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/strings/string_number_conversions.h"
#include "src/media/audio/tools/signal_generator/signal_generator.h"

namespace {

constexpr char kNumChannelsSwitch[] = "chans";
constexpr char kNumChannelsDefault[] = "2";
constexpr char kInt16FormatSwitch[] = "int16";
constexpr char kInt24FormatSwitch[] = "int24";
constexpr char kFrameRateSwitch[] = "rate";
constexpr char kFrameRateDefaultHz[] = "48000";

constexpr char kSineWaveSwitch[] = "sine";
constexpr char kSquareWaveSwitch[] = "square";
constexpr char kPulseWaveSwitch[] = "pulse";
constexpr char kSawtoothWaveSwitch[] = "saw";
constexpr char kTriangleWaveSwitch[] = "tri";
constexpr char kFrequencyDefaultHz[] = "440.0";
constexpr char kWhiteNoiseSwitch[] = "noise";
constexpr char kPinkNoiseSwitch[] = "pink";
constexpr char kImpulseSwitch[] = "impulse";
constexpr char kImpulseInitialDelaySecs[] = "1.0";

constexpr char kDurationSwitch[] = "dur";
constexpr char kDurationDefaultSecs[] = "2.0";
constexpr char kAmplitudeSwitch[] = "amp";
constexpr char kAmplitudeNoValueScale[] = "1.0";
constexpr char kAmplitudeNotSpecifiedScale[] = "0.25";
constexpr char kDutyCycleSwitch[] = "duty";
constexpr char kDutyCycleDefaultPercent[] = "50.0";

constexpr char kSaveToFileSwitch[] = "wav";
constexpr char kSaveToFileDefaultName[] = "/tmp/signal_generator.wav";

constexpr char kFlexibleClockSwitch[] = "flexible-clock";
constexpr char kMonotonicClockSwitch[] = "monotonic-clock";
constexpr char kCustomClockSwitch[] = "custom-clock";
constexpr char kClockRateSwitch[] = "rate-adjust";
constexpr char kClockRateDefault[] = "-75";

constexpr char kFramesPerPacketSwitch[] = "frames";
constexpr char kFramesPerPacketDefault[] = "480";

constexpr char kFramesPerPayloadBufferSwitch[] = "buffer";
constexpr char kFramesPerPayloadBufferDefault[] = "48000";

constexpr char kNumPayloadBuffersSwitch[] = "num-bufs";
constexpr char kNumPayloadBuffersDefault[] = "1";

constexpr char kRefStartTimeSwitch[] = "ref-start";
constexpr char kMediaStartPtsSwitch[] = "media-start";
constexpr char kMediaStartPtsDefault[] = "0";

constexpr char kUsePacketPtsSwitch[] = "packet-pts";
constexpr char kPtsUnitSwitch[] = "pts-unit";
constexpr char kPtsContinuityThresholdSwitch[] = "pts-threshold";
constexpr char kPtsContinuityThresholdDefaultSecs[] = "0.000125";

constexpr char kStreamGainSwitch[] = "gain";
constexpr char kStreamGainDefaultDb[] = "0.0";
constexpr char kStreamMuteSwitch[] = "mute";
constexpr char kStreamMuteDefault[] = "1";

constexpr char kStreamRampSwitch[] = "ramp";
constexpr char kStreamRampDurationSwitch[] = "ramp-dur";
constexpr char kStreamRampTargetGainSwitch[] = "end-gain";
constexpr char kStreamRampTargetGainDefaultDb[] = "-75.0";

constexpr char kRenderUsageSwitch[] = "usage";
constexpr char kRenderUsageDefault[] = "MEDIA";

constexpr char kRenderUsageGainSwitch[] = "usage-gain";
constexpr char kRenderUsageGainDefaultDb[] = "0.0";
constexpr char kRenderUsageVolumeSwitch[] = "usage-vol";
constexpr char kRenderUsageVolumeDefault[] = "1.0";

constexpr char kOnlineSwitch[] = "online";

constexpr char kUltrasoundSwitch[] = "ultrasound";

constexpr char kVerboseSwitch[] = "v";

constexpr char kHelpSwitch[] = "help";
constexpr char kHelp2Switch[] = "?";

constexpr std::array<const char*, 16> kUltrasoundInvalidOptions = {
    kNumChannelsSwitch,          kInt16FormatSwitch,
    kInt24FormatSwitch,          kFrameRateSwitch,
    kFlexibleClockSwitch,        kMonotonicClockSwitch,
    kCustomClockSwitch,          kClockRateSwitch,
    kStreamGainSwitch,           kStreamMuteSwitch,
    kStreamRampSwitch,           kStreamRampDurationSwitch,
    kStreamRampTargetGainSwitch, kRenderUsageSwitch,
    kRenderUsageGainSwitch,      kRenderUsageVolumeSwitch,
};

}  // namespace

void usage(const char* prog_name) {
  printf("\nUsage: %s [--option] [...]\n", prog_name);
  printf("Generate and play an audio signal to the preferred output device.\n");
  printf("\nValid options:\n");

  printf("\n    By default, stream format is %s-channel, float32 samples at %s Hz frame rate\n",
         kNumChannelsDefault, kFrameRateDefaultHz);
  printf("  --%s=<NUM_CHANS>\t   Specify number of channels\n", kNumChannelsSwitch);
  printf("  --%s\t\t   Use 16-bit integer samples\n", kInt16FormatSwitch);
  printf("  --%s\t\t   Use 24-in-32-bit integer samples (left-justified 'padded-24')\n",
         kInt24FormatSwitch);
  printf("  --%s=<FRAME_RATE>\t   Set frame rate, in Hz\n", kFrameRateSwitch);

  printf("\n    By default, signal is a sine wave. If no frequency is provided, %s Hz is used\n",
         kFrequencyDefaultHz);
  printf("  --%s[=<FREQ>]  \t   Play sine wave at given frequency, in Hz\n", kSineWaveSwitch);
  printf("  --%s[=<FREQ>]  \t   Play variable-duty-cycle pulse wave at given frequency\n",
         kPulseWaveSwitch);
  printf("  --%s[=<FREQ>]  \t   Play square wave at given frequency\n", kSquareWaveSwitch);
  printf("\t\t\t   (equivalent to '--pulse' with '--duty=50.0')\n");
  printf("  --%s[=<FREQ>]  \t   Play rising sawtooth wave at given frequency\n",
         kSawtoothWaveSwitch);
  printf("  --%s[=<FREQ>]  \t   Play rising-then-falling triangle wave at given frequency\n",
         kTriangleWaveSwitch);
  printf("  --%s  \t\t   Play pseudo-random 'white' noise\n", kWhiteNoiseSwitch);
  printf("  --%s  \t\t   Play pseudo-random 'pink' (1/f) noise\n", kPinkNoiseSwitch);
  printf("  --%s  \t\t   Play a single-frame impulse after %s secs of initial delay\n",
         kImpulseSwitch, kImpulseInitialDelaySecs);

  printf("\n    By default, play signal for %s seconds, at amplitude %s\n", kDurationDefaultSecs,
         kAmplitudeNotSpecifiedScale);
  printf("  --%s=<SECS>\t\t   Set playback length, in seconds\n", kDurationSwitch);
  printf("  --%s[=<AMPL>]\t   Set amplitude (0.0=silence, 1.0=full-scale, %s if only '--%s')\n",
         kAmplitudeSwitch, kAmplitudeNoValueScale, kAmplitudeSwitch);
  printf(
      "  --%s[=<PERCENT>]\t   Set duty cycle, in percent. Only for pulse waves.\n"
      "\t\t\t   (%s%% if only '--%s')\n",
      kDutyCycleSwitch, kDutyCycleDefaultPercent, kDutyCycleSwitch);

  printf("\n  --%s[=<FILEPATH>]\t   Save to .wav file (default '%s')\n", kSaveToFileSwitch,
         kSaveToFileDefaultName);

  printf("\n    Subsequent settings (e.g. gain, timestamps) do not affect .wav file contents\n");

  printf("\n    By default, use a %s stream and do not change this RENDER_USAGE's volume or gain\n",
         kRenderUsageDefault);
  printf("  --%s=<RENDER_USAGE>   Set stream render usage. RENDER_USAGE must be one of:\n\t\t\t   ",
         kRenderUsageSwitch);
  for (auto it = kRenderUsageOptions.cbegin(); it != kRenderUsageOptions.cend(); ++it) {
    printf("%s", it->first);
    if (it + 1 != kRenderUsageOptions.cend()) {
      printf(", ");
    } else {
      printf("\n");
    }
  }
  printf(
      "  --%s[=<VOLUME>]   Set render usage volume (min %.1f, max %.1f, %s if flag with no "
      "value)\n",
      kRenderUsageVolumeSwitch, fuchsia::media::audio::MIN_VOLUME,
      fuchsia::media::audio::MAX_VOLUME, kRenderUsageVolumeDefault);
  printf("  --%s[=<DB>]\t   Set render usage gain, in dB (min %.1f, max %.1f, default %s)\n",
         kRenderUsageGainSwitch, fuchsia::media::audio::MUTED_GAIN_DB, kUnityGainDb,
         kRenderUsageGainDefaultDb);
  printf("    Changes to these system-wide volume/gain settings persist after the utility runs.\n");

  printf("\n    Use the default reference clock unless specified otherwise\n");
  printf(
      "  --%s\t   Request and use the 'flexible' reference clock provided by the Audio service\n",
      kFlexibleClockSwitch);
  printf("  --%s\t   Clone CLOCK_MONOTONIC and use it as this stream's reference clock\n",
         kMonotonicClockSwitch);
  printf("  --%s\t   Create and use a custom clock as this stream's reference clock\n",
         kCustomClockSwitch);
  printf("  --%s[=<PPM>]\t   Run faster/slower than local system clock, in parts-per-million\n",
         kClockRateSwitch);
  printf("\t\t\t   (%d min, %d max, use %s if unspecified). Implies '--%s'\n",
         ZX_CLOCK_UPDATE_MIN_RATE_ADJUST, ZX_CLOCK_UPDATE_MAX_RATE_ADJUST, kClockRateDefault,
         kCustomClockSwitch);

  printf("\n    By default, submit data in non-timestamped packets of %s frames and %s VMO,\n",
         kFramesPerPacketDefault, kNumPayloadBuffersDefault);
  printf("    without specifying a precise reference time or PTS for the start of playback\n");
  printf("  --%s\t\t   Specify a reference time for playback start\n", kRefStartTimeSwitch);
  printf("  --%s[=<PTS>]\t   Specify a PTS value for playback start (%s if no value is provided)\n",
         kMediaStartPtsSwitch, kMediaStartPtsDefault);
  printf("  --%s[=<PTS>]\t   Apply timestamps to packets, starting with the provided value\n",
         kUsePacketPtsSwitch);
  printf("\t\t\t   If no value is specified, the PTS for playback start will be used.\n");
  printf("  --%s=<NUMER/DENOM> Set PTS units per second. If not set, '--%s' and '--%s'\n",
         kPtsUnitSwitch, kMediaStartPtsSwitch, kUsePacketPtsSwitch);
  printf("\t\t\t   use the PTS unit 1 nanosecond (1'000'000'000 / 1)\n");
  printf(
      "  --%s[=<SECS>] Set PTS discontinuity threshold, in seconds (%s if flag but no "
      "value)\n",
      kPtsContinuityThresholdSwitch, kPtsContinuityThresholdDefaultSecs);
  printf("  --%s=<FRAMES>\t   Set packet size, in frames \n", kFramesPerPacketSwitch);
  printf("  --%s=<BUFFERS>\t   Set the number of payload buffers \n", kNumPayloadBuffersSwitch);
  printf("  --%s=<FRAMES>\t   Set size of each payload buffer, in frames \n",
         kFramesPerPayloadBufferSwitch);
  printf("\t\t\t   Payload buffer space must exceed renderer MinLeadTime or signal duration\n");

  printf("\n    By default, submit packets upon previous packet completions\n");
  printf(
      "  --%s\t\t   Emit packets at precisely calculated times, ignoring previous completions.\n",
      kOnlineSwitch);
  printf("\t\t\t   This simulates playback from an external source, such as a network.\n");
  printf("\t\t\t   (This doubles the payload buffer space requirement mentioned above.)\n");

  printf("\n    By default, do not set AudioRenderer gain/mute (unity %.1f dB, unmuted, no ramp)\n",
         kUnityGainDb);
  printf("  --%s[=<GAIN_DB>]\t   Set stream gain (dB; min %.1f, max %.1f, default %s)\n",
         kStreamGainSwitch, fuchsia::media::audio::MUTED_GAIN_DB,
         fuchsia::media::audio::MAX_GAIN_DB, kStreamGainDefaultDb);
  printf(
      "  --%s[=<0|1>]\t   Set stream mute (0=Unmute or 1=Mute; Mute if only '--%s' is provided)\n",
      kStreamMuteSwitch, kStreamMuteSwitch);
  printf("  --%s\t\t   Smoothly ramp gain from initial value to target %s dB by end-of-signal\n",
         kStreamRampSwitch, kStreamRampTargetGainDefaultDb);
  printf("\t\t\t   If '--%s' is not provided, ramping starts at unity stream gain (%.1f dB)\n",
         kStreamGainSwitch, kUnityGainDb);
  printf("  --%s=<SECS>\t   Set a specific ramp duration, in seconds. Implies '--%s'\n",
         kStreamRampDurationSwitch, kStreamRampSwitch);
  printf("  --%s=<END_DB>\t   Set a different ramp target gain, in dB. Implies '--%s'\n",
         kStreamRampTargetGainSwitch, kStreamRampSwitch);

  printf("\n  --%s\t\t   Play signal using an ultrasound renderer\n", kUltrasoundSwitch);

  printf("\n  --%s\t\t\t   Display per-packet information\n", kVerboseSwitch);

  printf("  --%s, --%s\t\t   Show this message\n\n", kHelpSwitch, kHelp2Switch);
}

int main(int argc, const char** argv) {
  printf(
      "WARNING: signal_generator is deprecated. Please use `ffx audio gen`\n"
      "to generate signals and `ffx audio play`/`ffx audio device play`\n"
      "to play them. For more information, run `ffx audio gen --help`,\n"
      "`ffx audio play --help`, and `ffx audio device play --help`\n");

  const auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);

  if (command_line.HasOption(kHelpSwitch) || command_line.HasOption(kHelp2Switch)) {
    usage(argv[0]);
    return 0;
  }

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto component_context = sys::ComponentContext::CreateAndServeOutgoingDirectory();

  media::tools::MediaApp media_app(
      [&loop]() { async::PostTask(loop.dispatcher(), [&loop]() { loop.Quit(); }); });

  if (command_line.HasOption(kUltrasoundSwitch)) {
    media_app.set_ultrasound(true);

    for (auto& invalid_option : kUltrasoundInvalidOptions) {
      if (command_line.HasOption(std::string(invalid_option))) {
        usage(argv[0]);
        fprintf(stderr, "--ultrasound cannot be used with --%s\n\n", invalid_option);
        return 1;
      }
    }
  }

  media_app.set_online(command_line.HasOption(kOnlineSwitch));
  media_app.set_verbose(command_line.HasOption(kVerboseSwitch));

  // Handle channels and frame-rate
  std::string num_channels_str =
      command_line.GetOptionValueWithDefault(kNumChannelsSwitch, kNumChannelsDefault);
  media_app.set_num_channels(fxl::StringToNumber<uint32_t>(num_channels_str));

  std::string frame_rate_str =
      command_line.GetOptionValueWithDefault(kFrameRateSwitch, kFrameRateDefaultHz);
  uint32_t frame_rate = fxl::StringToNumber<uint32_t>(frame_rate_str);
  media_app.set_frame_rate(frame_rate);

  // Handle signal format
  if (command_line.HasOption(kInt16FormatSwitch)) {
    // Don't allow the user to specify more than one container format
    if (command_line.HasOption(kInt24FormatSwitch)) {
      usage(argv[0]);
      fprintf(stderr, "Cannot specify more than one sample format ('--%s' and '--%s')\n\n",
              kInt16FormatSwitch, kInt24FormatSwitch);
      return 1;
    }
    media_app.set_sample_format(fuchsia::media::AudioSampleFormat::SIGNED_16);
  }

  if (command_line.HasOption(kInt24FormatSwitch)) {
    media_app.set_sample_format(fuchsia::media::AudioSampleFormat::SIGNED_24_IN_32);
  }

  if (command_line.HasOption(kRenderUsageSwitch)) {
    std::string usage_option;
    command_line.GetOptionValue(kRenderUsageSwitch, &usage_option);
    auto it = std::find_if(kRenderUsageOptions.cbegin(), kRenderUsageOptions.cend(),
                           [&usage_option](auto usage_string_and_usage) {
                             return usage_option == usage_string_and_usage.first;
                           });
    if (it == kRenderUsageOptions.cend()) {
      usage(argv[0]);
      fprintf(stderr, "Unrecognized AudioRenderUsage %s\n\n", usage_option.c_str());
      return 1;
    }
    media_app.set_usage(it->second);
  }

  // Handle signal type and frequency specifications.
  // If >1 type is specified, obey usage order: sine, square, saw, noise.
  std::string frequency_str = "";
  if (command_line.HasOption(kSineWaveSwitch)) {
    media_app.set_output_type(kOutputTypeSine);
    command_line.GetOptionValue(kSineWaveSwitch, &frequency_str);
  } else if (command_line.HasOption(kSquareWaveSwitch)) {
    media_app.set_output_type(kOutputTypePulse);
    media_app.set_duty_cycle_percent(std::stof(kDutyCycleDefaultPercent));
    command_line.GetOptionValue(kSquareWaveSwitch, &frequency_str);
  } else if (command_line.HasOption(kPulseWaveSwitch)) {
    media_app.set_output_type(kOutputTypePulse);
    command_line.GetOptionValue(kPulseWaveSwitch, &frequency_str);
  } else if (command_line.HasOption(kSawtoothWaveSwitch)) {
    media_app.set_output_type(kOutputTypeSawtooth);
    command_line.GetOptionValue(kSawtoothWaveSwitch, &frequency_str);
  } else if (command_line.HasOption(kTriangleWaveSwitch)) {
    media_app.set_output_type(kOutputTypeTriangle);
    command_line.GetOptionValue(kTriangleWaveSwitch, &frequency_str);
  } else if (command_line.HasOption(kWhiteNoiseSwitch)) {
    media_app.set_output_type(kOutputTypeNoise);
  } else if (command_line.HasOption(kPinkNoiseSwitch)) {
    media_app.set_output_type(kOutputTypePinkNoise);
  } else if (command_line.HasOption(kImpulseSwitch)) {
    media_app.set_output_type(kOutputTypeImpulse);
    media_app.set_initial_delay(std::stod(kImpulseInitialDelaySecs));
  } else {
    media_app.set_output_type(kOutputTypeSine);
  }
  if (frequency_str == "") {
    frequency_str = kFrequencyDefaultHz;
  }

  media_app.set_frequency(std::stod(frequency_str));

  // Handle amplitude and duration of generated signal
  std::string amplitude_str;
  if (command_line.GetOptionValue(kAmplitudeSwitch, &amplitude_str)) {
    if (amplitude_str == "") {
      amplitude_str = kAmplitudeNoValueScale;
    }
  } else {
    amplitude_str = kAmplitudeNotSpecifiedScale;
  }
  media_app.set_amplitude(std::stof(amplitude_str));

  std::string duration_str =
      command_line.GetOptionValueWithDefault(kDurationSwitch, kDurationDefaultSecs);
  if (duration_str != "") {
    media_app.set_duration(std::stod(duration_str));
  }

  if (command_line.HasOption(kPulseWaveSwitch)) {
    std::string duty_cycle_str =
        command_line.GetOptionValueWithDefault(kDutyCycleSwitch, kDutyCycleDefaultPercent);
    if (duty_cycle_str != "") {
      media_app.set_duty_cycle_percent(std::stof(duty_cycle_str));
    } else {
      media_app.set_duty_cycle_percent(std::stof(kDutyCycleDefaultPercent));
    }
  }

  // Handle packet size
  std::string frames_per_packet_str =
      command_line.GetOptionValueWithDefault(kFramesPerPacketSwitch, kFramesPerPacketDefault);
  media_app.set_frames_per_packet(fxl::StringToNumber<uint32_t>(frames_per_packet_str));

  // Handle payload buffer size
  std::string frames_per_payload_str = command_line.GetOptionValueWithDefault(
      kFramesPerPayloadBufferSwitch, kFramesPerPayloadBufferDefault);
  media_app.set_frames_per_payload_buffer(fxl::StringToNumber<uint32_t>(frames_per_payload_str));

  // Set the number of buffers to use.
  std::string num_payload_buffers_str =
      command_line.GetOptionValueWithDefault(kNumPayloadBuffersSwitch, kNumPayloadBuffersDefault);
  media_app.set_num_payload_buffers(fxl::StringToNumber<uint32_t>(num_payload_buffers_str));

  // Handle any explicit reference clock selection. We allow Monotonic to be rate-adjusted,
  // otherwise rate-adjustment implies a custom clock which starts at value zero.
  if (command_line.HasOption(kMonotonicClockSwitch)) {
    media_app.set_clock_type(ClockType::Monotonic);
  } else if (command_line.HasOption(kCustomClockSwitch) ||
             command_line.HasOption(kClockRateSwitch)) {
    media_app.set_clock_type(ClockType::Custom);
  } else if (command_line.HasOption(kFlexibleClockSwitch)) {
    media_app.set_clock_type(ClockType::Flexible);
  } else {
    media_app.set_clock_type(ClockType::Default);
  }
  if (command_line.HasOption(kClockRateSwitch)) {
    std::string rate_adjustment_str;
    command_line.GetOptionValue(kClockRateSwitch, &rate_adjustment_str);
    if (rate_adjustment_str == "") {
      rate_adjustment_str = kClockRateDefault;
    }
    media_app.adjust_clock_rate(fxl::StringToNumber<int32_t>(rate_adjustment_str));
  }

  // Handle timestamp usage
  media_app.specify_ref_start_time(command_line.HasOption(kRefStartTimeSwitch));

  std::string pts_start_str;
  if (command_line.GetOptionValue(kMediaStartPtsSwitch, &pts_start_str)) {
    if (pts_start_str == "") {
      pts_start_str = kMediaStartPtsDefault;
    }
    media_app.set_media_start_pts(std::stoi(pts_start_str));
  }

  // Check whether we apply timestamps to each packet (otherwise use NO_TIMESTAMP)
  std::string packet_pts_start_str;
  if (command_line.GetOptionValue(kUsePacketPtsSwitch, &packet_pts_start_str)) {
    if (packet_pts_start_str != "") {
      media_app.set_packet_start_pts(std::stoi(packet_pts_start_str));
    }
    media_app.use_packet_pts(true);
  }

  // Check whether a PTS unit was specified (otherwise assume 1'000'000'000 per second)
  std::string pts_unit_str;
  if (command_line.GetOptionValue(kPtsUnitSwitch, &pts_unit_str)) {
    uint64_t numerator, denominator;
    std::string err_str;
    const std::string fraction_regex("(\\d+)/(\\d+)");

    // Make numerous checks but show error only at the end, to avoid I/O sync issues with usage()
    bool success = !pts_unit_str.empty() &&
                   RE2::FullMatch(pts_unit_str, fraction_regex, &numerator, &denominator);
    if (!success) {
      // if no values or malformed values, display general error message
      // All other error paths set their own error messages
      err_str = "'--";
      err_str.append(kPtsUnitSwitch)
          .append("' requires integral numerator and denominator values, separated by '/'");
    }
    if (success) {
      if (numerator > std::numeric_limits<uint32_t>::max()) {
        success = false;
        err_str = "Numerator too large (must fit into a uint32)";
      } else if (numerator == 0) {
        success = false;
        err_str = "Numerator must be positive";
      }
    }
    if (success) {
      if (denominator > std::numeric_limits<uint32_t>::max()) {
        success = false;
        err_str = "Denominator too large (must fit into a uint32)";
      } else if (denominator == 0) {
        success = false;
        err_str = "Denominator must be positive";
      }
    }
    if (!success) {
      usage(argv[0]);
      fprintf(stderr, "%s\n\n", err_str.c_str());
      return 1;
    }
    media_app.set_pts_units(static_cast<uint32_t>(numerator), static_cast<uint32_t>(denominator));
  }
  // Check whether PTS continuity threshold was set (previous-packet-end to new-packet-start)
  std::string pts_continuity_threshold_str;
  if (command_line.GetOptionValue(kPtsContinuityThresholdSwitch, &pts_continuity_threshold_str)) {
    if (pts_continuity_threshold_str == "") {
      pts_continuity_threshold_str = kPtsContinuityThresholdDefaultSecs;
    }
    media_app.set_pts_continuity_threshold(std::stof(pts_continuity_threshold_str));
  }

  // Handle stream gain
  std::string stream_gain_str;
  if (command_line.GetOptionValue(kStreamGainSwitch, &stream_gain_str)) {
    if (stream_gain_str == "") {
      stream_gain_str = kStreamGainDefaultDb;
    }
    media_app.set_stream_gain(std::stof(stream_gain_str));
  }

  std::string stream_mute_str;
  if (command_line.GetOptionValue(kStreamMuteSwitch, &stream_mute_str)) {
    if (stream_mute_str == "") {
      stream_mute_str = kStreamMuteDefault;
    }
    media_app.set_stream_mute(fxl::StringToNumber<uint32_t>(stream_mute_str) != 0);
  }

  // Handle stream gain ramping, target gain and ramp duration.
  if (command_line.HasOption(kStreamRampSwitch) ||
      command_line.HasOption(kStreamRampTargetGainSwitch) ||
      command_line.HasOption(kStreamRampDurationSwitch)) {
    std::string target_gain_db_str = command_line.GetOptionValueWithDefault(
        kStreamRampTargetGainSwitch, kStreamRampTargetGainDefaultDb);
    if (target_gain_db_str == "") {
      target_gain_db_str = kStreamRampTargetGainDefaultDb;
    }
    media_app.set_ramp_target_gain_db(std::stof(target_gain_db_str));

    // Convert signal duration of doublefloat seconds, to int64 nanoseconds.
    auto ramp_duration_nsec =
        static_cast<zx_duration_t>(media_app.get_duration() * 1'000'000'000.0);
    if (command_line.HasOption(kStreamRampDurationSwitch)) {
      std::string ramp_duration_str = "";
      command_line.GetOptionValue(kStreamRampDurationSwitch, &ramp_duration_str);

      if (ramp_duration_str != "") {
        // Convert input of doublefloat seconds, to int64 nanoseconds.
        ramp_duration_nsec =
            static_cast<zx_duration_t>(std::stod(ramp_duration_str) * 1'000'000'000.0);
      }
    }
    media_app.set_ramp_duration_nsec(ramp_duration_nsec);
  }

  // Handle render usage volume and gain
  if (command_line.HasOption(kRenderUsageVolumeSwitch)) {
    std::string usage_volume_str;
    command_line.GetOptionValue(kRenderUsageVolumeSwitch, &usage_volume_str);
    if (usage_volume_str == "") {
      usage_volume_str = kRenderUsageVolumeDefault;
    }
    media_app.set_usage_volume(std::stof(usage_volume_str));
  }
  if (command_line.HasOption(kRenderUsageGainSwitch)) {
    std::string usage_gain_str;
    command_line.GetOptionValue(kRenderUsageGainSwitch, &usage_gain_str);
    if (usage_gain_str == "") {
      usage_gain_str = kRenderUsageGainDefaultDb;
    }
    media_app.set_usage_gain(std::stof(usage_gain_str));
  }

  // Handle "generate to file"
  std::string save_file_str;
  if (command_line.GetOptionValue(kSaveToFileSwitch, &save_file_str)) {
    // If just '--wav' is specified, use the default file name.
    if (save_file_str == "") {
      save_file_str = kSaveToFileDefaultName;
    }
    media_app.set_save_file_name(save_file_str);
  }

  media_app.Run(component_context.get());

  // We've set everything going. Wait for our message loop to return.
  loop.Run();

  return 0;
}
