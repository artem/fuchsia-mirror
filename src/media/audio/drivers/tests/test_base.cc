// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/tests/test_base.h"

#include <fcntl.h>
#include <fuchsia/component/cpp/fidl.h>
#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <fuchsia/media/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/enum.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <sys/types.h>

#include <algorithm>
#include <cstddef>
#include <cstring>
#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/media/audio/drivers/tests/audio_device_enumerator_stub.h"

inline constexpr bool kAllowCompositeDriverUnsupportedRingBufferFormats = true;

namespace media::audio::drivers::test {

using component_testing::ChildRef;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::RealmBuilder;
using component_testing::RealmRoot;
using component_testing::Route;

// Device discovery is done once at binary open; a fresh FIDL channel is used for each test.
// TODO(b/301003578): When virtual audio is DFv2, remove DFv1-specific trampoline support.
void TestBase::SetUp() {
  media::audio::test::TestFixture::SetUp();

  auto& entry = device_entry();
  if (entry.isA2DP()) {
    ConnectToBluetoothDevice();
  } else {
    switch (entry.driver_type) {
      case DriverType::Codec:
        CreateCodecFromChannel(
            ConnectWithTrampoline<fuchsia::hardware::audio::Codec,
                                  fuchsia::hardware::audio::CodecConnectorPtr>(device_entry()));
        break;
      case DriverType::Composite:
        // Use DFv1-specific trampoline Connector API for the virtual composite driver (DFv1),
        // for all other non-virtual composite drivers (DFv2) do not use the trampoline.
        if (entry.device_type == DeviceType::Virtual) {
          CreateCompositeFromChannel(
              ConnectWithTrampoline<fuchsia::hardware::audio::Composite,
                                    fuchsia::hardware::audio::CompositeConnectorPtr>(
                  device_entry()));
        } else {
          CreateCompositeFromChannel(
              Connect<fuchsia::hardware::audio::CompositePtr>(device_entry()));
        }
        break;
      case DriverType::Dai:
        CreateDaiFromChannel(
            ConnectWithTrampoline<fuchsia::hardware::audio::Dai,
                                  fuchsia::hardware::audio::DaiConnectorPtr>(device_entry()));
        break;
      case DriverType::StreamConfigInput:
        [[fallthrough]];
      case DriverType::StreamConfigOutput:
        CreateStreamConfigFromChannel(
            ConnectWithTrampoline<fuchsia::hardware::audio::StreamConfig,
                                  fuchsia::hardware::audio::StreamConfigConnectorPtr>(
                device_entry()));

        break;
    }
  }
}

void TestBase::TearDown() {
  if (device_entry().isCodec()) {
    codec_.Unbind();
  } else if (device_entry().isComposite()) {
    composite_.Unbind();
  } else if (device_entry().isDai()) {
    dai_.Unbind();
  } else if (device_entry().isStreamConfig()) {
    stream_config_.Unbind();
  }

  if (realm_.has_value()) {
    // We're about to shut down the realm; unbind to unhook the error handler.
    audio_binder_.Unbind();
    bool complete = false;
    realm_.value().Teardown(
        [&complete](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&complete]() { return complete; });
  }

  // Audio drivers can have multiple StreamConfig channels open, but only one can be 'privileged':
  // the one that can in turn create a RingBuffer channel. Each test case starts from scratch,
  // opening and closing channels. If we create a StreamConfig channel before the previous one is
  // cleared, a new StreamConfig channel will not be privileged and Admin tests will fail.
  //
  // When disconnecting a StreamConfig, there's no signal to wait on before proceeding (potentially
  // immediately executing other tests); insert a 10-ms wait (needing >3.5ms was never observed).
  zx::nanosleep(zx::deadline_after(zx::msec(10)));

  TestFixture::TearDown();
}

void TestBase::ConnectToBluetoothDevice() {
  std::unique_ptr<AudioDeviceEnumeratorStub> audio_device_enumerator_impl =
      std::make_unique<AudioDeviceEnumeratorStub>();
  auto audio_device_enumerator_impl_ptr = audio_device_enumerator_impl.get();

  auto builder = RealmBuilder::Create();
  // The component binding must live as long as the Realm, so std::move the
  // unique_ptr into the component function.
  builder.AddLocalChild(
      "audio-device-enumerator",
      [audio_device_enumerator_impl = std::move(audio_device_enumerator_impl)]() mutable {
        // Note: This lambda does not create a new instance,
        // so the component can only be started once.
        return std::move(audio_device_enumerator_impl);
      });
  builder.AddChild("audio-device-output-harness", "#meta/audio-device-output-harness.cm");
  builder.AddRoute(Route{.capabilities = {Protocol{fuchsia::media::AudioDeviceEnumerator::Name_}},
                         .source = ChildRef{"audio-device-enumerator"},
                         .targets = {ChildRef{"audio-device-output-harness"}}});
  builder.AddRoute(Route{.capabilities = {Protocol{fuchsia::logger::LogSink::Name_}},
                         .source = ParentRef{},
                         .targets = {ChildRef{"audio-device-output-harness"}}});
  builder.AddRoute(Route{
      .capabilities = {Protocol{.name = fuchsia::component::Binder::Name_, .as = "audio-binder"}},
      .source = ChildRef{"audio-device-output-harness"},
      .targets = {ParentRef{}}});
  realm_ = builder.Build();
  ASSERT_EQ(ZX_OK,
            realm_->component().Connect("audio-binder", audio_binder_.NewRequest().TakeChannel()));
  audio_binder_.set_error_handler([](zx_status_t status) {
    FAIL() << "audio-device-output-harness exited: " << zx_status_get_string(status);
  });

  // Wait for the Bluetooth harness to AddDeviceByChannel, then pass it on
  RunLoopUntil([impl = audio_device_enumerator_impl_ptr]() {
    return impl->channel_available() || HasFailure();
  });
  CreateStreamConfigFromChannel(audio_device_enumerator_impl_ptr->TakeChannel());
}

// Given this device_entry, use its channel to open the device.
template <typename DeviceType, typename ConnectorType>
fidl::InterfaceHandle<DeviceType> TestBase::ConnectWithTrampoline(const DeviceEntry& device_entry) {
  auto connector = Connect<ConnectorType>(device_entry);
  fidl::InterfaceHandle<DeviceType> client;
  fidl::InterfaceRequest<DeviceType> server = client.NewRequest();
  connector->Connect(std::move(server));

  auto channel = client.TakeChannel();
  FX_LOGS(TRACE) << "Successfully opened devnode '" << device_entry.filename << "' for audio "
                 << driver_type();
  return fidl::InterfaceHandle<DeviceType>(std::move(channel));
}

template <typename DeviceType>
DeviceType TestBase::Connect(const DeviceEntry& device_entry) {
  DeviceType device;
  ZX_ASSERT(device_entry.dir.index() == 1u);
  ZX_ASSERT(fdio_service_connect_at(std::get<1>(device_entry.dir).channel()->get(),
                                    device_entry.filename.c_str(),
                                    device.NewRequest().TakeChannel().release()) == ZX_OK);
  device.set_error_handler([this](zx_status_t status) {
    FAIL() << status << "Err " << status << ", failed to open channel for audio " << driver_type();
  });
  return std::move(device);
}

void TestBase::CreateCodecFromChannel(
    fidl::InterfaceHandle<fuchsia::hardware::audio::Codec> channel) {
  codec_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!codec_.is_bound()) {
    FAIL() << "Failed to get codec channel for this device";
    __UNREACHABLE;
  }
  AddErrorHandler(codec_, "Codec");
}

void TestBase::CreateCompositeFromChannel(
    fidl::InterfaceHandle<fuchsia::hardware::audio::Composite> channel) {
  composite_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!composite_.is_bound()) {
    FAIL() << "Failed to get composite channel for this device";
    __UNREACHABLE;
  }
  AddErrorHandler(composite_, "Composite");
}

void TestBase::CreateDaiFromChannel(fidl::InterfaceHandle<fuchsia::hardware::audio::Dai> channel) {
  dai_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!dai_.is_bound()) {
    FAIL() << "Failed to get DAI channel for this device";
    __UNREACHABLE;
  }
  AddErrorHandler(dai_, "DAI");
}

void TestBase::CreateStreamConfigFromChannel(
    fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig> channel) {
  stream_config_ = channel.Bind();

  // If no device was enumerated, don't waste further time.
  if (!stream_config_.is_bound()) {
    FAIL() << "Failed to get stream channel for this device";
    __UNREACHABLE;
  }
  AddErrorHandler(stream_config_, "StreamConfig");
}

// Request that the driver return the format ranges that it supports.
void TestBase::RetrieveDaiFormats() {
  if (device_entry().isCodec()) {
    codec()->GetDaiFormats(AddCallback(
        "Codec::GetDaiFormats",
        [this](fuchsia::hardware::audio::Codec_GetDaiFormats_Result result) {
          ASSERT_FALSE(result.is_err());
          auto& supported_dai_formats = result.response().formats;
          EXPECT_FALSE(supported_dai_formats.empty());

          for (size_t i = 0; i < supported_dai_formats.size(); ++i) {
            SCOPED_TRACE(testing::Message() << "Codec supported_dai_formats[" << i << "]");
            dai_formats_.push_back(std::move(supported_dai_formats[i]));
          }
        }));
  } else if (device_entry().isComposite()) {
    RequestTopologies();

    // If there is a dai id, request the DAI formats for this endpoint.
    if (dai_id_.has_value()) {
      composite()->GetDaiFormats(
          dai_id_.value(),
          AddCallback("Composite::GetDaiFormats",
                      [this](fuchsia::hardware::audio::Composite_GetDaiFormats_Result result) {
                        ASSERT_FALSE(result.is_err());
                        auto& supported_formats = result.response().dai_formats;
                        EXPECT_FALSE(supported_formats.empty());

                        for (size_t i = 0; i < supported_formats.size(); ++i) {
                          SCOPED_TRACE(testing::Message()
                                       << "Composite supported_formats[" << i << "]");
                          dai_formats_.push_back(std::move(supported_formats[i]));
                        }
                      }));
    } else {
      // "No DAI" is also valid (Composite can replace StreamConfig); do nothing in that case.
      return;
    }
  } else if (device_entry().isDai()) {
    dai()->GetDaiFormats(AddCallback(
        "Dai::GetDaiFormats", [this](fuchsia::hardware::audio::Dai_GetDaiFormats_Result result) {
          ASSERT_FALSE(result.is_err());
          auto& supported_dai_formats = result.response().dai_formats;
          EXPECT_FALSE(supported_dai_formats.empty());

          for (size_t i = 0; i < supported_dai_formats.size(); ++i) {
            SCOPED_TRACE(testing::Message() << "DAI supported_dai_formats[" << i << "]");
            dai_formats_.push_back(std::move(supported_dai_formats[i]));
          }
        }));
  } else {
    return;  // StreamConfig, nothing to do
  }
  ExpectCallbacks();

  ValidateDaiFormatSets(dai_formats());
  if (!HasFailure()) {
    SetMinMaxDaiFormats();
  }
}

// Fail if the returned formats are not complete, unique and within ranges.
// Consider defining FIDL constants for min/max channels, frame_rate, bits_per_slot/sample, etc.
void TestBase::ValidateDaiFormatSets(
    const std::vector<fuchsia::hardware::audio::DaiSupportedFormats>& dai_format_sets) {
  ASSERT_FALSE(dai_format_sets.empty()) << "No dai formats returned";
  for (size_t i = 0; i < dai_format_sets.size(); ++i) {
    SCOPED_TRACE(testing::Message() << "dai_format[" << i << "]");
    auto& format_set = dai_format_sets[i];

    ASSERT_FALSE(format_set.number_of_channels.empty());
    // Ensure number_of_channels are unique.
    for (size_t j = 0; j < format_set.number_of_channels.size(); ++j) {
      EXPECT_GT(format_set.number_of_channels[j], 0u);
      EXPECT_LE(format_set.number_of_channels[j], 256u);
      for (size_t k = j + 1; k < format_set.number_of_channels.size(); ++k) {
        EXPECT_NE(format_set.number_of_channels[j], format_set.number_of_channels[k])
            << "number_of_channels[" << j << "] must not equal number_of_channels[" << k << "]";
      }
    }

    ASSERT_FALSE(format_set.sample_formats.empty());
    // Ensure sample_formats are unique.
    for (size_t j = 0; j < format_set.sample_formats.size(); ++j) {
      for (size_t k = j + 1; k < format_set.sample_formats.size(); ++k) {
        EXPECT_NE(static_cast<uint16_t>(fidl::ToUnderlying(format_set.sample_formats[j])),
                  static_cast<uint16_t>(fidl::ToUnderlying(format_set.sample_formats[k])))
            << "sample_formats[" << j << "] must not equal sample_formats[" << k << "]";
      }
    }

    ASSERT_FALSE(format_set.frame_formats.empty());
    // Ensure frame_formats are unique.
    for (size_t j = 0; j < format_set.frame_formats.size(); ++j) {
      auto& format_1 = format_set.frame_formats[j];
      for (size_t k = j + 1; k < format_set.frame_formats.size(); ++k) {
        auto& format_2 = format_set.frame_formats[k];
        if (format_1.Which() != format_2.Which()) {
          continue;
        }
        if (format_1.is_frame_format_standard()) {
          if (format_1.frame_format_standard() != format_2.frame_format_standard()) {
            continue;
          }
        } else {
          if (format_1.frame_format_custom().left_justified !=
                  format_2.frame_format_custom().left_justified ||
              format_1.frame_format_custom().sclk_on_raising !=
                  format_2.frame_format_custom().sclk_on_raising ||
              format_1.frame_format_custom().frame_sync_sclks_offset !=
                  format_2.frame_format_custom().frame_sync_sclks_offset ||
              format_1.frame_format_custom().frame_sync_size !=
                  format_2.frame_format_custom().frame_sync_size) {
            continue;
          }
        }
        FAIL() << "frame_formats[" << j << "] must not equal frame_formats[" << k << "]";
        __UNREACHABLE;
      }
    }

    ASSERT_FALSE(format_set.frame_rates.empty());
    // Ensure frame_rates are unique and ascending.
    for (size_t j = 0; j < format_set.frame_rates.size(); ++j) {
      EXPECT_GE(format_set.frame_rates[j], 1'000u);
      EXPECT_LE(format_set.frame_rates[j], 768'000u);
      if (j > 0) {
        EXPECT_LT(format_set.frame_rates[j - 1], format_set.frame_rates[j])
            << "frame_rates[" << j - 1 << "] must be less than frame_rates[" << j << "]";
      }
    }

    ASSERT_FALSE(format_set.bits_per_slot.empty());
    // Ensure bits_per_slot are unique and ascending.
    uint8_t max_bits_per_slot = 0;
    for (size_t j = 0; j < format_set.bits_per_slot.size(); ++j) {
      EXPECT_GT(static_cast<uint16_t>(format_set.bits_per_slot[j]), 0u);
      EXPECT_LE(static_cast<uint16_t>(format_set.bits_per_slot[j]), 64u);
      max_bits_per_slot = std::max(max_bits_per_slot, format_set.bits_per_slot[j]);
      if (j > 0) {
        EXPECT_LT(static_cast<uint16_t>(format_set.bits_per_slot[j - 1]),
                  static_cast<uint16_t>(format_set.bits_per_slot[j]))
            << "bits_per_slot[" << j - 1 << "] must be less than bits_per_slot[" << j << "]";
      }
    }

    ASSERT_FALSE(format_set.bits_per_sample.empty());
    // Ensure bits_per_sample are unique and ascending.
    for (size_t j = 0; j < format_set.bits_per_sample.size(); ++j) {
      EXPECT_GT(static_cast<uint16_t>(format_set.bits_per_sample[j]), 0u);
      EXPECT_LE(static_cast<uint16_t>(format_set.bits_per_sample[j]),
                static_cast<uint16_t>(max_bits_per_slot));
      if (j > 0) {
        EXPECT_LT(static_cast<uint16_t>(format_set.bits_per_sample[j - 1]),
                  static_cast<uint16_t>(format_set.bits_per_sample[j]))
            << "bits_per_sample[" << j - 1 << "] must be less than bits_per_sample[" << j << "]";
      }
    }
  }
}

// For debugging purposes
void TestBase::LogDaiFormatSets(
    const std::vector<fuchsia::hardware::audio::DaiSupportedFormats>& dai_format_sets,
    std::string tag) {
  FX_LOGS(INFO) << tag << ": dai_format_sets[" << dai_format_sets.size() << "]";
  for (auto i = 0u; i < dai_format_sets.size(); ++i) {
    auto& format_set = dai_format_sets[i];
    FX_LOGS(INFO) << tag << ":   [" << i << "]   number_of_channels["
                  << format_set.number_of_channels.size() << "]";
    for (auto num : format_set.number_of_channels) {
      FX_LOGS(INFO) << tag << ":             " << num;
    }
    FX_LOGS(INFO) << tag << ":         sample_formats    [" << format_set.sample_formats.size()
                  << "]";
    for (auto fmt : format_set.sample_formats) {
      FX_LOGS(INFO) << tag << ":             " << fmt;
    }
    FX_LOGS(INFO) << tag << ":         frame_formats     [" << format_set.frame_formats.size()
                  << "]";
    for (auto& fmt : format_set.frame_formats) {
      fuchsia::hardware::audio::DaiFrameFormat dai_frame_format = {};
      EXPECT_EQ(fuchsia::hardware::audio::Clone(fmt, &dai_frame_format), ZX_OK);
      FX_LOGS(INFO) << tag << ":             " << std::move(dai_frame_format);
    }
    FX_LOGS(INFO) << tag << ":         frame_rates       [" << format_set.frame_rates.size() << "]";
    for (auto rate : format_set.frame_rates) {
      FX_LOGS(INFO) << tag << ":             " << rate;
    }
    FX_LOGS(INFO) << tag << ":         bits_per_slot     [" << format_set.bits_per_slot.size()
                  << "]";
    for (auto bits : format_set.bits_per_slot) {
      FX_LOGS(INFO) << tag << ":             " << static_cast<uint16_t>(bits);
    }
    FX_LOGS(INFO) << tag << ":         bits_per_sample   [" << format_set.bits_per_sample.size()
                  << "]";
    for (auto bits : format_set.bits_per_sample) {
      FX_LOGS(INFO) << tag << ":             " << static_cast<uint16_t>(bits);
    }
  }
}

void TestBase::SetMinMaxDaiFormats() {
  for (size_t i = 0; i < dai_formats_.size(); ++i) {
    auto& format_set = dai_formats_[i];
    fuchsia::hardware::audio::DaiSampleFormat sample_format = format_set.sample_formats[0];
    fuchsia::hardware::audio::DaiFrameFormat frame_format;
    if (format_set.frame_formats[0].is_frame_format_standard()) {
      frame_format.set_frame_format_standard(format_set.frame_formats[0].frame_format_standard());
    } else {
      frame_format.set_frame_format_custom(format_set.frame_formats[0].frame_format_custom());
    }

    uint32_t min_number_of_channels = ~0, max_number_of_channels = 0;
    for (auto& number_of_channels : format_set.number_of_channels) {
      if (number_of_channels < min_number_of_channels) {
        min_number_of_channels = number_of_channels;
      }
      if (number_of_channels > max_number_of_channels) {
        max_number_of_channels = number_of_channels;
      }
    }

    uint8_t min_bits_per_slot = ~0, max_bits_per_slot = 0;
    for (auto& bits_per_slot : format_set.bits_per_slot) {
      if (bits_per_slot < min_bits_per_slot) {
        min_bits_per_slot = bits_per_slot;
      }
      if (bits_per_slot > max_bits_per_slot) {
        max_bits_per_slot = bits_per_slot;
      }
    }

    uint8_t min_bits_per_sample = ~0, max_bits_per_sample = 0;
    for (auto& bits_per_sample : format_set.bits_per_sample) {
      if (bits_per_sample < min_bits_per_sample) {
        min_bits_per_sample = bits_per_sample;
      }
      if (bits_per_sample > max_bits_per_sample) {
        max_bits_per_sample = bits_per_sample;
      }
    }

    uint32_t min_frame_rate = ~0, max_frame_rate = 0;
    for (auto& frame_rate : format_set.frame_rates) {
      if (frame_rate < min_frame_rate) {
        min_frame_rate = frame_rate;
      }
      if (frame_rate > max_frame_rate) {
        max_frame_rate = frame_rate;
      }
    }

    // Save, if less than min.
    auto bit_rate = min_number_of_channels * min_bits_per_sample * min_frame_rate;
    if (i == 0 || bit_rate < min_dai_format_->number_of_channels *
                                 min_dai_format_->bits_per_sample * min_dai_format_->frame_rate) {
      min_dai_format_ = fuchsia::hardware::audio::DaiFormat{
          .number_of_channels = min_number_of_channels,
          .channels_to_use_bitmask = (1u << min_number_of_channels) - 1u,
          .sample_format = sample_format,
          .frame_format = std::move(frame_format),
          .frame_rate = min_frame_rate,
          .bits_per_slot = min_bits_per_slot,
          .bits_per_sample = min_bits_per_sample,
      };
    }
    // Save, if more than max.
    bit_rate = max_number_of_channels * max_bits_per_sample * max_frame_rate;
    if (i == 0 || bit_rate > max_dai_format_->number_of_channels *
                                 max_dai_format_->bits_per_sample * max_dai_format_->frame_rate) {
      max_dai_format_ = fuchsia::hardware::audio::DaiFormat{
          .number_of_channels = max_number_of_channels,
          .channels_to_use_bitmask = (1u << max_number_of_channels) - 1u,
          .sample_format = sample_format,
          .frame_format = std::move(frame_format),
          .frame_rate = max_frame_rate,
          .bits_per_slot = max_bits_per_slot,
          .bits_per_sample = max_bits_per_sample,
      };
    }
  }
}

void TestBase::GetMinDaiFormat(fuchsia::hardware::audio::DaiFormat& min_dai_format_out) {
  if (!min_dai_format_) {
    RetrieveDaiFormats();
  }

  if (dai_formats_.empty()) {
    GTEST_SKIP() << "*** this audio device returns no DaiFormats. Skipping this test. ***";
    __UNREACHABLE;
  }

  EXPECT_EQ(fuchsia::hardware::audio::Clone(*min_dai_format_, &min_dai_format_out), ZX_OK);
}

void TestBase::GetMaxDaiFormat(fuchsia::hardware::audio::DaiFormat& max_dai_format_out) {
  if (!max_dai_format_) {
    RetrieveDaiFormats();
  }

  if (dai_formats_.empty()) {
    GTEST_SKIP() << "*** this audio device returns no DaiFormats. Skipping this test. ***";
    __UNREACHABLE;
  }

  EXPECT_EQ(fuchsia::hardware::audio::Clone(*max_dai_format_, &max_dai_format_out), ZX_OK);
}

void TestBase::ValidateDaiFormat(const fuchsia::hardware::audio::DaiFormat& dai_format) {
  EXPECT_GT(dai_format.bits_per_sample, 0u);
  EXPECT_LE(dai_format.bits_per_sample, sizeof(double) * 8u);
  EXPECT_GE(dai_format.bits_per_slot, dai_format.bits_per_sample);
  EXPECT_LE(dai_format.bits_per_slot, sizeof(double) * 8u);
  EXPECT_GE(dai_format.frame_rate, 1'000u);
  EXPECT_LE(dai_format.frame_rate, 768'000u);
  EXPECT_GT(dai_format.number_of_channels, 0u);
  EXPECT_LE(dai_format.number_of_channels, 64u);
  EXPECT_GT(dai_format.channels_to_use_bitmask, 0u);
}

// For debugging purposes
void TestBase::LogDaiFormat(const fuchsia::hardware::audio::DaiFormat& format,
                            const std::string& tag) {
  fuchsia::hardware::audio::DaiFrameFormat dai_frame_format = {};
  EXPECT_EQ(fuchsia::hardware::audio::Clone(format.frame_format, &dai_frame_format), ZX_OK);
  FX_LOGS(INFO) << tag << ": " << format.frame_rate << ", " << format.sample_format << ", "
                << std::move(dai_frame_format) << ", " << std::dec
                << static_cast<uint16_t>(format.bits_per_sample) << "-in-"
                << static_cast<uint16_t>(format.bits_per_slot) << ", " << format.number_of_channels
                << "-chan (0x" << std::hex << format.channels_to_use_bitmask << ")";
}

const std::vector<fuchsia::hardware::audio::DaiSupportedFormats>& TestBase::dai_formats() const {
  return dai_formats_;
}

// Request that the driver return the format ranges that it supports.
void TestBase::RetrieveRingBufferFormats() {
  if (device_entry().isCodec()) {
    return;  // Codec, nothing to do
  }
  if (device_entry().isComposite()) {
    RequestTopologies();

    // If ring_buffer_id_ is set, then a ring-buffer endpoint exists for this composite device.
    // Retrieve the supported ring buffer formats for that node.
    if (ring_buffer_id_.has_value()) {
      composite()->GetRingBufferFormats(
          ring_buffer_id_.value(),
          AddCallback("GetRingBufferFormats", [this](fuchsia::hardware::audio::
                                                         Composite_GetRingBufferFormats_Result
                                                             result) {
            if (!device_entry().isVirtual() && !device_entry().isA2DP() &&
                kAllowCompositeDriverUnsupportedRingBufferFormats) {
              if (result.is_err() &&
                  (result.err() == fuchsia::hardware::audio::DriverError::NOT_SUPPORTED)) {
                GTEST_SKIP()
                    << "*** this audio device is known to not (yet) support GetRingBufferFormats(). Skipping this test. ***";
                __UNREACHABLE;
              }
            }
            ASSERT_FALSE(result.is_err()) << static_cast<int32_t>(result.err());
            auto& supported_formats = result.response().ring_buffer_formats;
            EXPECT_FALSE(supported_formats.empty());

            for (size_t i = 0; i < supported_formats.size(); ++i) {
              SCOPED_TRACE(testing::Message() << "Composite supported_formats[" << i << "]");
              ASSERT_TRUE(supported_formats[i].has_pcm_supported_formats());
              auto& format_set = *supported_formats[i].mutable_pcm_supported_formats();
              ring_buffer_pcm_formats_.push_back(std::move(format_set));
            }
          }));
    } else {
      // "No ring buffer" is valid (Composite can replace Codec); do nothing in that case.
      return;
    }
  } else if (device_entry().isDai()) {
    dai()->GetRingBufferFormats(
        AddCallback("GetRingBufferFormats",
                    [this](fuchsia::hardware::audio::Dai_GetRingBufferFormats_Result result) {
                      ASSERT_FALSE(result.is_err());
                      auto& supported_rb_formats = result.response().ring_buffer_formats;
                      EXPECT_FALSE(supported_rb_formats.empty());

                      for (size_t i = 0; i < supported_rb_formats.size(); ++i) {
                        SCOPED_TRACE(testing::Message() << "DAI supported_rb_formats[" << i << "]");
                        ASSERT_TRUE(supported_rb_formats[i].has_pcm_supported_formats());
                        auto& format_set = *supported_rb_formats[i].mutable_pcm_supported_formats();
                        ring_buffer_pcm_formats_.push_back(std::move(format_set));
                      }
                    }));
  } else if (device_entry().isStreamConfig()) {
    stream_config()->GetSupportedFormats(AddCallback(
        "GetSupportedFormats",
        [this](std::vector<fuchsia::hardware::audio::SupportedFormats> supported_formats) {
          EXPECT_FALSE(supported_formats.empty());

          for (size_t i = 0; i < supported_formats.size(); ++i) {
            SCOPED_TRACE(testing::Message() << "StreamConfig supported_formats[" << i << "]");
            ASSERT_TRUE(supported_formats[i].has_pcm_supported_formats());
            auto& format_set = *supported_formats[i].mutable_pcm_supported_formats();
            ring_buffer_pcm_formats_.push_back(std::move(format_set));
          }
        }));
  } else {
    FAIL() << "unknown driver type";
    __UNREACHABLE;
  }
  ExpectCallbacks();

  ValidateRingBufferFormatSets(ring_buffer_pcm_formats());
  if (!HasFailure()) {
    SetMinMaxRingBufferFormats();
  }
}

// Fail if the returned formats are not complete, unique and within ranges.
void TestBase::ValidateRingBufferFormatSets(
    const std::vector<fuchsia::hardware::audio::PcmSupportedFormats>& rb_format_sets) {
  for (size_t i = 0; i < rb_format_sets.size(); ++i) {
    SCOPED_TRACE(testing::Message() << "ring_buffer_pcm_format[" << i << "]");
    auto& format_set = rb_format_sets[i];

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

void TestBase::SetMinMaxRingBufferFormats() {
  for (size_t i = 0; i < ring_buffer_pcm_formats_.size(); ++i) {
    SCOPED_TRACE(testing::Message() << "pcm_format[" << i << "]");
    size_t min_chans = ~0, max_chans = 0;
    uint8_t min_bytes_per_sample = ~0, max_bytes_per_sample = 0;
    uint8_t min_valid_bits_per_sample = ~0, max_valid_bits_per_sample = 0;
    uint32_t min_frame_rate = ~0, max_frame_rate = 0;

    auto& format_set = ring_buffer_pcm_formats_[i];
    fuchsia::hardware::audio::SampleFormat sample_format = format_set.sample_formats()[0];

    for (auto& channel_set : format_set.channel_sets()) {
      if (channel_set.attributes().size() < min_chans) {
        min_chans = channel_set.attributes().size();
      }
      if (channel_set.attributes().size() > max_chans) {
        max_chans = channel_set.attributes().size();
      }
    }

    for (auto& bytes_per_sample : format_set.bytes_per_sample()) {
      if (bytes_per_sample < min_bytes_per_sample) {
        min_bytes_per_sample = bytes_per_sample;
      }
      if (bytes_per_sample > max_bytes_per_sample) {
        max_bytes_per_sample = bytes_per_sample;
      }
    }

    for (auto& valid_bits : format_set.valid_bits_per_sample()) {
      if (valid_bits < min_valid_bits_per_sample) {
        min_valid_bits_per_sample = valid_bits;
      }
      if (valid_bits > max_valid_bits_per_sample) {
        max_valid_bits_per_sample = valid_bits;
      }
    }

    for (auto& frame_rate : format_set.frame_rates()) {
      if (frame_rate < min_frame_rate) {
        min_frame_rate = frame_rate;
      }
      if (frame_rate > max_frame_rate) {
        max_frame_rate = frame_rate;
      }
    }

    // Save, if less than min.
    auto bit_rate = min_chans * min_bytes_per_sample * min_frame_rate;
    if (i == 0 || bit_rate < static_cast<size_t>(min_ring_buffer_format_.number_of_channels) *
                                 min_ring_buffer_format_.bytes_per_sample *
                                 min_ring_buffer_format_.frame_rate) {
      min_ring_buffer_format_ = {
          .number_of_channels = static_cast<uint8_t>(min_chans),
          .sample_format = sample_format,
          .bytes_per_sample = min_bytes_per_sample,
          .valid_bits_per_sample = min_valid_bits_per_sample,
          .frame_rate = min_frame_rate,
      };
    }

    // Save, if more than max.
    bit_rate = max_chans * max_bytes_per_sample * max_frame_rate;
    if (i == 0 || bit_rate > static_cast<size_t>(max_ring_buffer_format_.number_of_channels) *
                                 max_ring_buffer_format_.bytes_per_sample *
                                 max_ring_buffer_format_.frame_rate) {
      max_ring_buffer_format_ = {
          .number_of_channels = static_cast<uint8_t>(max_chans),
          .sample_format = sample_format,
          .bytes_per_sample = max_bytes_per_sample,
          .valid_bits_per_sample = max_valid_bits_per_sample,
          .frame_rate = max_frame_rate,
      };
    }
  }
}

void TestBase::ValidateRingBufferFormat(const fuchsia::hardware::audio::PcmFormat& rb_format) {
  EXPECT_GT(rb_format.bytes_per_sample, 0u);
  EXPECT_LE(rb_format.bytes_per_sample, sizeof(double));
  EXPECT_GT(rb_format.frame_rate, fuchsia::media::MIN_PCM_FRAMES_PER_SECOND);
  EXPECT_LE(rb_format.frame_rate, fuchsia::media::MAX_PCM_FRAMES_PER_SECOND);
  EXPECT_GT(rb_format.number_of_channels, 0u);
  EXPECT_LE(rb_format.number_of_channels,
            fuchsia::hardware::audio::MAX_COUNT_CHANNELS_IN_RING_BUFFER);
  EXPECT_GT(rb_format.valid_bits_per_sample, 0u);
  EXPECT_LE(rb_format.valid_bits_per_sample, sizeof(double) * 8u);
}

void TestBase::LogRingBufferFormat(const fuchsia::hardware::audio::PcmFormat& format,
                                   const std::string& tag) {
  FX_LOGS(INFO) << tag << ": " << format.frame_rate << ", smp_fmt "
                << static_cast<int>(format.sample_format) << ", "
                << static_cast<uint16_t>(format.valid_bits_per_sample) << "-in-"
                << format.bytes_per_sample * 8u << ", "
                << static_cast<uint16_t>(format.number_of_channels) << "-chan";
}

const fuchsia::hardware::audio::PcmFormat& TestBase::min_ring_buffer_format() const {
  return min_ring_buffer_format_;
}

const fuchsia::hardware::audio::PcmFormat& TestBase::max_ring_buffer_format() const {
  return max_ring_buffer_format_;
}

const std::vector<fuchsia::hardware::audio::PcmSupportedFormats>&
TestBase::ring_buffer_pcm_formats() const {
  return ring_buffer_pcm_formats_;
}

void TestBase::SignalProcessingConnect() {
  if (sp_.is_bound()) {
    return;  // Already connected.
  }
  fidl::InterfaceHandle<fuchsia::hardware::audio::signalprocessing::SignalProcessing> sp_client;
  fidl::InterfaceRequest<fuchsia::hardware::audio::signalprocessing::SignalProcessing> sp_server =
      sp_client.NewRequest();
  composite()->SignalProcessingConnect(std::move(sp_server));
  sp_ = sp_client.Bind();
}

// First retrieve the element list. If signalprocessing is not supported, exit early;
// otherwise save the ID of a RING_BUFFER ENDPOINT, and the ID of a DAI_INTERCONNECT ENDPOINT.
// We will use these IDs later, when performing Dai-specific and RingBuffer-specific checks.
void TestBase::RequestTopologies() {
  SignalProcessingConnect();
  zx_status_t status = ZX_OK;
  ring_buffer_id_.reset();
  dai_id_.reset();
  sp_->GetElements(AddCallback(
      "signalprocessing::Reader::GetElements",
      [this,
       &status](fuchsia::hardware::audio::signalprocessing::Reader_GetElements_Result result) {
        status = result.is_err() ? result.err() : ZX_OK;
        if (status == ZX_OK) {
          elements_ = std::move(result.response().processing_elements);
          for (auto& element : elements_) {
            if (element.type() ==
                fuchsia::hardware::audio::signalprocessing::ElementType::ENDPOINT) {
              if (element.type_specific().endpoint().type() ==
                  fuchsia::hardware::audio::signalprocessing::EndpointType::RING_BUFFER) {
                ring_buffer_id_.emplace(element.id());  // Override any previous.
              } else if (element.type_specific().endpoint().type() ==
                         fuchsia::hardware::audio::signalprocessing::EndpointType::
                             DAI_INTERCONNECT) {
                dai_id_.emplace(element.id());  // Override any previous.
              }
            }
          }
        }
      }));
  ExpectCallbacks();

  // Either we get elements or the API method is not supported.
  ASSERT_TRUE(status == ZX_OK || status == ZX_ERR_NOT_SUPPORTED);
  // We don't check for topologies if GetElements is not supported.
  if (status == ZX_ERR_NOT_SUPPORTED) {
    return;
  }
  // If supported, GetElements must return at least one element.
  ASSERT_TRUE(!elements_.empty());

  sp_->GetTopologies(AddCallback(
      "signalprocessing::Reader::GetTopologies",
      [this](fuchsia::hardware::audio::signalprocessing::Reader_GetTopologies_Result result) {
        ASSERT_FALSE(result.is_err());
        topologies_ = std::move(result.response().topologies);
      }));
  ExpectCallbacks();

  // We only call GetTopologies if we have elements, so we must have at least one topology.
  ASSERT_FALSE(topologies_.empty());
}

// Request that the driver return its gain capabilities and current state, expecting a response.
void TestBase::RetrieveIsBridgeable() {
  ASSERT_TRUE(device_entry().isCodec());

  // We reconnect the stream every time we run a test, and by driver interface definition the driver
  // must reply to the first watch request, so we get gain state by issuing a watch FIDL call.
  codec()->IsBridgeable(AddCallback("IsBridgeable", [this](bool supports_bridged_mode) {
    supports_bridged_mode_ = supports_bridged_mode;
  }));
  ExpectCallbacks();
  ASSERT_TRUE(supports_bridged_mode_);
}

}  // namespace media::audio::drivers::test
