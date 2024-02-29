// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_LOGGING_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_LOGGING_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <optional>
#include <ostream>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

#define ADR_LOG_OBJECT(CONDITION)                         \
  FX_LAZY_STREAM(FX_LOG_STREAM(INFO, nullptr), CONDITION) \
      << kClassName << "(" << this << ")::" << __func__ << ": "

#define ADR_LOG_CLASS(CONDITION) \
  FX_LAZY_STREAM(FX_LOG_STREAM(INFO, nullptr), CONDITION) << kClassName << "::" << __func__ << ": "

#define ADR_LOG(CONDITION) \
  FX_LAZY_STREAM(FX_LOG_STREAM(INFO, nullptr), CONDITION) << __func__ << ": "

#define ADR_WARN_OBJECT() FX_LOGS(WARNING) << kClassName << "(" << this << ")::" << __func__ << ": "

inline constexpr bool kLogMain = true;

inline constexpr bool kLogDeviceDetection = false;
inline constexpr bool kLogDeviceInitializationProgress = false;
inline constexpr bool kLogAudioDeviceRegistryMethods = false;
inline constexpr bool kLogSummaryFinalDeviceInfo = true;
inline constexpr bool kLogDetailedFinalDeviceInfo = true;

inline constexpr bool kLogDeviceMethods = false;

inline constexpr bool kLogStreamConfigFidlCalls = false;
inline constexpr bool kLogStreamConfigFidlResponses = false;
inline constexpr bool kLogStreamConfigFidlResponseValues = false;

inline constexpr bool kLogCodecFidlCalls = false;
inline constexpr bool kLogCodecFidlResponses = false;
inline constexpr bool kLogCodecFidlResponseValues = true;

inline constexpr bool kLogRingBufferMethods = false;
inline constexpr bool kLogRingBufferFidlCalls = false;
inline constexpr bool kLogRingBufferFidlResponses = false;
inline constexpr bool kLogRingBufferFidlResponseValues = false;

inline constexpr bool kLogObjectLifetimes = false;
inline constexpr bool kLogDeviceState = false;
inline constexpr bool kLogObjectCounts = false;
inline constexpr bool kLogNotifyMethods = false;

inline constexpr bool kLogControlCreatorServerMethods = false;
inline constexpr bool kLogControlCreatorServerResponses = false;

inline constexpr bool kLogControlServerMethods = false;
inline constexpr bool kLogControlServerResponses = false;

inline constexpr bool kLogObserverServerMethods = false;
inline constexpr bool kLogObserverServerResponses = false;

inline constexpr bool kLogProviderServerMethods = false;
inline constexpr bool kLogProviderServerResponses = false;

inline constexpr bool kLogRegistryServerMethods = false;
inline constexpr bool kLogRegistryServerResponses = false;

inline constexpr bool kLogRingBufferServerMethods = false;
inline constexpr bool kLogRingBufferServerResponses = false;

std::string UidToString(std::optional<UniqueId> unique_instance_id);

void LogStreamProperties(const fuchsia_hardware_audio::StreamProperties& props);
void LogSupportedFormats(const std::vector<fuchsia_hardware_audio::SupportedFormats>& formats);
void LogGainState(const fuchsia_hardware_audio::GainState& gain_state);
void LogPlugState(const fuchsia_hardware_audio::PlugState& plug_state);

void LogCodecProperties(const fuchsia_hardware_audio::CodecProperties& props);
void LogCodecFormatInfo(std::optional<fuchsia_hardware_audio::CodecFormatInfo> format_info);

void LogDeviceInfo(const fuchsia_audio_device::Info& device_info);

void LogDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);
void LogDaiFormat(std::optional<fuchsia_hardware_audio::DaiFormat> format);

void LogRingBufferProperties(const fuchsia_hardware_audio::RingBufferProperties& props);
void LogRingBufferFormat(const fuchsia_hardware_audio::Format& format);
void LogRingBufferVmo(const zx::vmo& vmo, uint32_t num_frames,
                      fuchsia_hardware_audio::Format format);
void LogDelayInfo(const fuchsia_hardware_audio::DelayInfo& info);
void LogActiveChannels(uint64_t channel_bitmask, zx::time set_time);

// Enabled by kLogObjectCounts.
void LogObjectCounts();

// TODO(https://fxbug.dev/327533694): consider using fostr formatters for these.

// fuchsia_hardware_audio types
inline std::ostream& operator<<(std::ostream& out,
                                const fuchsia_hardware_audio::SampleFormat& format) {
  switch (format) {
    case fuchsia_hardware_audio::SampleFormat::kPcmSigned:
      return (out << "PCM_SIGNED");
    case fuchsia_hardware_audio::SampleFormat::kPcmUnsigned:
      return (out << "PCM_UNSIGNED");
    case fuchsia_hardware_audio::SampleFormat::kPcmFloat:
      return (out << "PCM_FLOAT");
  }
}
inline std::ostream& operator<<(std::ostream& out,
                                const fuchsia_hardware_audio::PcmFormat& format) {
  return (out << "[" << static_cast<uint16_t>(format.number_of_channels()) << "-channel, "
              << format.sample_format() << ", " << static_cast<uint16_t>(format.bytes_per_sample())
              << " bytes/sample, " << static_cast<uint16_t>(format.valid_bits_per_sample())
              << " valid bits per sample, " << format.frame_rate() << " Hz]");
}
inline std::ostream& operator<<(std::ostream& out,
                                const fuchsia_hardware_audio::PlugDetectCapabilities& plug_caps) {
  switch (plug_caps) {
    case fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired:
      return (out << "HARDWIRED");
    case fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify:
      return (out << "CAN_ASYNC_NOTIFY");
  }
}
inline std::ostream& operator<<(std::ostream& out,
                                const fuchsia_hardware_audio::DaiSampleFormat& sample_format) {
  switch (sample_format) {
    case fuchsia_hardware_audio::DaiSampleFormat::kPdm:
      return (out << "PDM");
    case fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned:
      return (out << "PCM SIGNED");
    case fuchsia_hardware_audio::DaiSampleFormat::kPcmUnsigned:
      return (out << "PCM UNSIGNED");
    case fuchsia_hardware_audio::DaiSampleFormat::kPcmFloat:
      return (out << "PCM FLOAT");
    default:
      return (out << "OTHER (unknown enum)");
  }
}
inline std::ostream& operator<<(std::ostream& out,
                                const fuchsia_hardware_audio::DaiFrameFormat& frame_format) {
  if (!frame_format.frame_format_custom().has_value() &&
      !frame_format.frame_format_standard().has_value()) {
    return (out << "FrameFormat UNKNOWN union value");
  }

  if (frame_format.Which() == fuchsia_hardware_audio::DaiFrameFormat::Tag::kFrameFormatCustom) {
    return (out << "FrameFormatCustom(left_justified "
                << frame_format.frame_format_custom()->left_justified() << ", sclk_on_raising "
                << frame_format.frame_format_custom()->sclk_on_raising()
                << ", frame_sync_sclks_offset "
                << static_cast<int16_t>(
                       frame_format.frame_format_custom()->frame_sync_sclks_offset()))
           << ", frame_sync_size "
           << static_cast<uint16_t>(frame_format.frame_format_custom()->frame_sync_size()) << ")";
  }

  if (frame_format.Which() == fuchsia_hardware_audio::DaiFrameFormat::Tag::kFrameFormatStandard) {
    out << "FrameFormatStandard::";
    switch (frame_format.frame_format_standard().value()) {
      case fuchsia_hardware_audio::DaiFrameFormatStandard::kNone:
        return (out << "NONE");
      case fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S:
        return (out << "I2S");
      case fuchsia_hardware_audio::DaiFrameFormatStandard::kStereoLeft:
        return (out << "STEREO_LEFT");
      case fuchsia_hardware_audio::DaiFrameFormatStandard::kStereoRight:
        return (out << "STEREO_RIGHT");
      case fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm1:
        return (out << "TDM1");
      case fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm2:
        return (out << "TDM2");
      case fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm3:
        return (out << "TDM3");
      default:
        return (out << "OTHER (unknown enum)");
    }
  }

  return (out << "FrameFormat UNKNOWN union tag");
}

inline std::ostream& operator<<(
    std::ostream& out,
    const std::optional<fuchsia_hardware_audio_signalprocessing::Element>& sp_element) {
  if (sp_element) {
    return (out << "DISPLAY IS UNSUPPORTED (for now)");
  }
  return (out << "NONE");
}

inline std::ostream& operator<<(
    std::ostream& out,
    const std::optional<fuchsia_hardware_audio_signalprocessing::Topology>& sp_topology) {
  if (sp_topology) {
    return (out << "DISPLAY IS UNSUPPORTED (for now)");
  }
  return (out << "NONE");
}

inline std::ostream& operator<<(std::ostream& out, const fuchsia_audio::SampleType& sample_type) {
  switch (sample_type) {
    case fuchsia_audio::SampleType::kUint8:
      return (out << "UINT_8");
    case fuchsia_audio::SampleType::kInt16:
      return (out << "INT_16");
    case fuchsia_audio::SampleType::kInt32:
      return (out << "INT_32");
    case fuchsia_audio::SampleType::kFloat32:
      return (out << "FLOAT_32");
    case fuchsia_audio::SampleType::kFloat64:
      return (out << "FLOAT_64");
    default:
      return (out << "UNKNOWN");
  }
}

// fuchsia_audio_device types
inline std::ostream& operator<<(
    std::ostream& out, const std::optional<fuchsia_audio_device::DeviceType>& device_type) {
  if (device_type) {
    switch (*device_type) {
      case fuchsia_audio_device::DeviceType::kCodec:
        return (out << "  CODEC  ");
      case fuchsia_audio_device::DeviceType::kComposite:
        return (out << "COMPOSITE");
      case fuchsia_audio_device::DeviceType::kDai:
        return (out << "   DAI   ");
      case fuchsia_audio_device::DeviceType::kInput:
        return (out << "  INPUT  ");
      case fuchsia_audio_device::DeviceType::kOutput:
        return (out << "  OUTPUT ");
      default:
        return (out << "[UNKNOWN]");
    }
  }
  return (out << "NONE (non-compliant)");
}
inline std::ostream& operator<<(
    std::ostream& out,
    const std::optional<fuchsia_audio_device::PlugDetectCapabilities>& plug_caps) {
  if (plug_caps) {
    switch (*plug_caps) {
      case fuchsia_audio_device::PlugDetectCapabilities::kHardwired:
        return (out << "HARDWIRED");
      case fuchsia_audio_device::PlugDetectCapabilities::kPluggable:
        return (out << "PLUGGABLE");
      default:
        return (out << "OTHER (unknown enum)");
    }
  }
  return (out << "NONE (non-compliant)");
}
inline std::ostream& operator<<(std::ostream& out,
                                const fuchsia_audio_device::PlugState& plug_state) {
  switch (plug_state) {
    case fuchsia_audio_device::PlugState::kPlugged:
      return (out << "PLUGGED");
    case fuchsia_audio_device::PlugState::kUnplugged:
      return (out << "UNPLUGGED");
    default:
      return (out << "OTHER (unknown enum)");
  }
}

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_LOGGING_H_
