// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite-server.h"

#include <lib/driver/component/cpp/driver_base.h>
#include <zircon/errors.h>

#include <algorithm>
#include <numeric>

#include <fbl/algorithm.h>

#include "src/lib/memory_barriers/memory_barriers.h"

namespace audio::aml_g12 {

// In C++23 we can remove this and just use std::ranges::contains.
template <typename Container, typename T>
bool contains(Container c, T v) {
  return std::find(std::begin(c), std::end(c), v) != std::end(c);
}

// The Composite interface returns DriverError upon error, not zx_status_t, so convert any error.
fuchsia_hardware_audio::DriverError ZxStatusToDriverError(zx_status_t status) {
  switch (status) {
    case ZX_ERR_NOT_SUPPORTED:
      return fuchsia_hardware_audio::DriverError::kNotSupported;
    case ZX_ERR_INVALID_ARGS:
      return fuchsia_hardware_audio::DriverError::kInvalidArgs;
    case ZX_ERR_WRONG_TYPE:
      return fuchsia_hardware_audio::DriverError::kWrongType;
    case ZX_ERR_SHOULD_WAIT:
      return fuchsia_hardware_audio::DriverError::kShouldWait;
    default:
      return fuchsia_hardware_audio::DriverError::kInternalError;
  }
}

AudioCompositeServer::AudioCompositeServer(
    std::array<std::optional<fdf::MmioBuffer>, kNumberOfTdmEngines> mmios, zx::bti bti,
    async_dispatcher_t* dispatcher, metadata::AmlVersion aml_version,
    fidl::WireSyncClient<fuchsia_hardware_clock::Clock> clock_gate_client,
    fidl::WireSyncClient<fuchsia_hardware_clock::Clock> pll_client,
    std::vector<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> gpio_sclk_clients)
    : dispatcher_(dispatcher),
      bti_(std::move(bti)),
      clock_gate_(std::move(clock_gate_client)),
      pll_(std::move(pll_client)),
      gpio_sclk_clients_(std::move(gpio_sclk_clients)) {
  for (auto& dai : kDaiIds) {
    element_completers_[dai].first_response_sent = false;
    element_completers_[dai].completer = {};
  }
  for (auto& ring_buffer : kRingBufferIds) {
    element_completers_[ring_buffer].first_response_sent = false;
    element_completers_[ring_buffer].completer = {};
  }
  topology_completer_.first_response_sent = false;

  for (size_t i = 0; i < kNumberOfPipelines; ++i) {
    // Default supported DAI formats.
    supported_dai_formats_[i].number_of_channels(
        AmlTdmConfigDevice::GetSupportedNumberOfChannels());
    supported_dai_formats_[i].sample_formats(AmlTdmConfigDevice::GetFidlSupportedSampleFormats());
    supported_dai_formats_[i].frame_formats(AmlTdmConfigDevice::GetFidlSupportedFrameFormats());
    supported_dai_formats_[i].frame_rates(AmlTdmConfigDevice::GetSupportedFrameRates());
    supported_dai_formats_[i].bits_per_slot(AmlTdmConfigDevice::GetSupportedBitsPerSlot());
    supported_dai_formats_[i].bits_per_sample(AmlTdmConfigDevice::GetSupportedBitsPerSample());

    // Take values from supported list to define current DAI format.
    current_dai_formats_[i].emplace(
        supported_dai_formats_[i].number_of_channels()[0],
        (1 << supported_dai_formats_[i].number_of_channels()[0]) - 1,  // enable all channels.
        supported_dai_formats_[i].sample_formats()[0], supported_dai_formats_[i].frame_formats()[0],
        supported_dai_formats_[i].frame_rates()[0], supported_dai_formats_[i].bits_per_slot()[0],
        supported_dai_formats_[i].bits_per_sample()[0]);
  }

  ZX_ASSERT(StartSocPower() == ZX_OK);

  // Output engines.
  ZX_ASSERT(ConfigEngine(0, 0, false, std::move(mmios[0].value()), aml_version) == ZX_OK);
  ZX_ASSERT(ConfigEngine(1, 1, false, std::move(mmios[1].value()), aml_version) == ZX_OK);
  ZX_ASSERT(ConfigEngine(2, 2, false, std::move(mmios[2].value()), aml_version) == ZX_OK);

  // Input engines.
  ZX_ASSERT(ConfigEngine(3, 0, true, std::move(mmios[3].value()), aml_version) == ZX_OK);
  ZX_ASSERT(ConfigEngine(4, 1, true, std::move(mmios[4].value()), aml_version) == ZX_OK);
  ZX_ASSERT(ConfigEngine(5, 2, true, std::move(mmios[5].value()), aml_version) == ZX_OK);
  // Unconditional reset on construction.
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    ZX_ASSERT(ResetEngine(i) == ZX_OK);
  }

  // Make sure the DMAs are stopped before releasing quarantine.
  for (auto& engine : engines_) {
    engine.device->Stop();
  }
  // Make sure that all reads/writes have gone through.
  BarrierBeforeRelease();
  ZX_ASSERT(bti_.release_quarantine() == ZX_OK);

#if 0
  // TODO(b/309153055): Once integration with the Power Framework is completed, we can remove
  // this testing code.
  TestPowerManagement();
#endif
}

zx_status_t AudioCompositeServer::ConfigEngine(size_t index, size_t dai_index, bool input,
                                               fdf::MmioBuffer mmio,
                                               metadata::AmlVersion aml_version) {
  // Common configuration.
  engines_[index].config = {};
  engines_[index].config.version = aml_version;
  engines_[index].config.mClockDivFactor = 10;
  engines_[index].config.sClockDivFactor = 25;

  // Supported ring buffer formats.
  // We take some values from supported DAI formats.
  fuchsia_hardware_audio::PcmSupportedFormats pcm_formats;

  // Vector with number_of_channels empty attributes supported in Ring Buffer
  // equal to the number of channels supported on DAI.
  std::vector<fuchsia_hardware_audio::ChannelAttributes> attributes(
      supported_dai_formats_[dai_index].number_of_channels()[0]);
  fuchsia_hardware_audio::ChannelSet channel_set;
  channel_set.attributes(std::move(attributes));
  pcm_formats.channel_sets(std::vector{channel_set});

  // Frame rates supported on DAI are supported in Ring Buffer.
  pcm_formats.frame_rates(supported_dai_formats_[dai_index].frame_rates());

  // Sample format is PCM signed.
  pcm_formats.sample_formats(std::vector{fuchsia_hardware_audio::SampleFormat::kPcmSigned});

  // Bits per slot supported on DAI are supported in Ring Buffer as bytes per sample.
  auto v = supported_dai_formats_[dai_index].bits_per_slot();
  std::transform(v.begin(), v.end(), v.begin(), [](const uint8_t bits_per_slot) -> uint8_t {
    ZX_ASSERT(bits_per_slot % 8 == 0);
    return bits_per_slot / 8;
  });
  pcm_formats.bytes_per_sample(v);

  // Bits per sample supported on DAI are supported in Ring Buffer.
  pcm_formats.valid_bits_per_sample(supported_dai_formats_[dai_index].bits_per_sample());

  supported_ring_buffer_formats_[index] = std::move(pcm_formats);

  engines_[index].dai_index = dai_index;
  engines_[index].config.is_input = input;

  switch (dai_index) {
      // clang-format off
    case 0: engines_[index].config.bus = metadata::AmlBus::TDM_A; break;
    case 1: engines_[index].config.bus = metadata::AmlBus::TDM_B; break;
    case 2: engines_[index].config.bus = metadata::AmlBus::TDM_C; break;
      // clang-format on
    default:
      return ZX_ERR_INVALID_ARGS;
  }

  engines_[index].device.emplace(engines_[index].config, std::move(mmio));

  // Unconditional reset on configuration.
  return ResetEngine(index);
}

zx_status_t AudioCompositeServer::ResetEngine(size_t index) {
  // Resets engine using AmlTdmConfigDevice, so we need to translate the current state
  // into parameters used by AmlTdmConfigDevice.
  ZX_ASSERT(engines_[index].dai_index < kNumberOfPipelines);
  ZX_ASSERT(current_dai_formats_[engines_[index].dai_index].has_value());
  const fuchsia_hardware_audio::DaiFormat& dai_format =
      *current_dai_formats_[engines_[index].dai_index];
  if (dai_format.sample_format() != fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned) {
    FDF_LOG(ERROR, "Sample format not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }
  auto& dai_type = engines_[index].config.dai.type;
  using StandardFormat = fuchsia_hardware_audio::DaiFrameFormatStandard;
  using DaiType = metadata::DaiType;
  switch (dai_format.frame_format().frame_format_standard().value()) {
      // clang-format off
    case StandardFormat::kI2S:        dai_type = DaiType::I2s;                 break;
    case StandardFormat::kStereoLeft: dai_type = DaiType::StereoLeftJustified; break;
    case StandardFormat::kTdm1:       dai_type = DaiType::Tdm1;                break;
    case StandardFormat::kTdm2:       dai_type = DaiType::Tdm2;                break;
    case StandardFormat::kTdm3:       dai_type = DaiType::Tdm3;                break;
      // clang-format on
    case StandardFormat::kNone:
      [[fallthrough]];
    case StandardFormat::kStereoRight:
      [[fallthrough]];
    default:
      FDF_LOG(ERROR, "Frame format not supported");
      return ZX_ERR_NOT_SUPPORTED;
  }
  engines_[index].config.dai.bits_per_sample = dai_format.bits_per_sample();
  engines_[index].config.dai.bits_per_slot = dai_format.bits_per_slot();
  engines_[index].config.dai.number_of_channels =
      static_cast<uint8_t>(dai_format.number_of_channels());
  engines_[index].config.ring_buffer.number_of_channels =
      engines_[index].config.dai.number_of_channels;
  // AMLogic allows channel swapping with a channel number per nibble in registers like
  // EE_AUDIO_TDMOUT_A_SWAP and EE_AUDIO_TDMIN_A_SWAP.
  constexpr uint32_t kNoSwaps = 0x76543210;  // Default channel numbers for no swaps.
  engines_[index].config.swaps = kNoSwaps;
  size_t lane = engines_[index].config.is_input ? 1 : 0;
  engines_[index].config.lanes_enable_mask[lane] =
      (1 << dai_format.number_of_channels()) - 1;  // enable all channels.

  zx_status_t status = AmlTdmConfigDevice::Normalize(engines_[index].config);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to normalize config: %s", zx_status_get_string(status));
    return status;
  }

  status = engines_[index].device->InitHW(
      engines_[index].config, dai_format.channels_to_use_bitmask(), dai_format.frame_rate());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to init hardware: %s", zx_status_get_string(status));
  }
  return status;
}

void AudioCompositeServer::Reset(ResetCompleter::Sync& completer) {
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    if (zx_status_t status = ResetEngine(i); status != ZX_OK) {
      completer.Reply(zx::error(ZxStatusToDriverError(status)));
      return;
    }
  }
  completer.Reply(zx::ok());
}

void AudioCompositeServer::GetProperties(
    fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync& completer) {
  fuchsia_hardware_audio::CompositeProperties props;
  props.clock_domain(fuchsia_hardware_audio::kClockDomainMonotonic);
  completer.Reply(std::move(props));
}

void AudioCompositeServer::GetHealthState(GetHealthStateCompleter::Sync& completer) {
  completer.Reply(fuchsia_hardware_audio::HealthState{}.healthy(true));
}

// Note that if already bound, we close the NEW channel (not the one on which we were called).
void AudioCompositeServer::SignalProcessingConnect(
    SignalProcessingConnectRequest& request, SignalProcessingConnectCompleter::Sync& completer) {
  if (signal_) {
    request.protocol().Close(ZX_ERR_ALREADY_BOUND);
    return;
  }
  signal_.emplace(dispatcher(), std::move(request.protocol()), this,
                  std::mem_fn(&AudioCompositeServer::OnSignalProcessingClosed));
}

void AudioCompositeServer::OnSignalProcessingClosed(fidl::UnbindInfo info) {
  if (info.is_peer_closed()) {
    FDF_LOG(INFO, "Client disconnected");
  } else if (!info.is_user_initiated()) {
    // Do not log canceled cases; these happen particularly frequently in certain test cases.
    if (info.status() != ZX_ERR_CANCELED) {
      FDF_LOG(ERROR, "Client connection unbound: %s", info.status_string());
    }
  }
  if (signal_) {
    signal_.reset();
  }
}

void AudioCompositeServer::GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                                                GetRingBufferFormatsCompleter::Sync& completer) {
  auto ring_buffer =
      std::find(kRingBufferIds.begin(), kRingBufferIds.end(), request.processing_element_id());
  if (ring_buffer == kRingBufferIds.end()) {
    FDF_LOG(ERROR, "Unknown Ring Buffer id (%lu) for format retrieval",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  size_t ring_buffer_index = ring_buffer - kRingBufferIds.begin();
  ZX_ASSERT(ring_buffer_index < kNumberOfTdmEngines);

  fuchsia_hardware_audio::SupportedFormats formats_entry;
  formats_entry.pcm_supported_formats(supported_ring_buffer_formats_[ring_buffer_index]);
  completer.Reply(zx::ok(std::vector{std::move(formats_entry)}));
}

void AudioCompositeServer::CreateRingBuffer(CreateRingBufferRequest& request,
                                            CreateRingBufferCompleter::Sync& completer) {
  auto ring_buffer =
      std::find(kRingBufferIds.begin(), kRingBufferIds.end(), request.processing_element_id());
  if (ring_buffer == kRingBufferIds.end()) {
    FDF_LOG(ERROR, "Unknown Ring Buffer id (%lu) for creation", request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  size_t ring_buffer_index = ring_buffer - kRingBufferIds.begin();
  ZX_ASSERT(ring_buffer_index < kNumberOfTdmEngines);
  auto& supported = supported_ring_buffer_formats_[ring_buffer_index];
  if (!request.format().pcm_format().has_value()) {
    FDF_LOG(ERROR, "No PCM formats provided for Ring Buffer id (%lu)",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  ZX_ASSERT(supported.channel_sets().has_value());
  ZX_ASSERT(supported.sample_formats().has_value());
  ZX_ASSERT(supported.bytes_per_sample().has_value());
  ZX_ASSERT(supported.valid_bits_per_sample().has_value());
  ZX_ASSERT(supported.frame_rates().has_value());

  auto& requested = *request.format().pcm_format();

  bool number_of_channels_found = false;
  for (auto& supported_channel_set : *supported.channel_sets()) {
    ZX_ASSERT(supported_channel_set.attributes().has_value());
    if (supported_channel_set.attributes()->size() == requested.number_of_channels()) {
      number_of_channels_found = true;
      break;
    }
  }
  if (!number_of_channels_found) {
    FDF_LOG(ERROR, "Ring Buffer number of channels for Ring Buffer id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(*supported.sample_formats(), requested.sample_format())) {
    FDF_LOG(ERROR, "Ring Buffer sample format for Ring Buffer id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(*supported.bytes_per_sample(), requested.bytes_per_sample())) {
    FDF_LOG(ERROR, "Ring Buffer bytes per sample for Ring Buffer id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(*supported.valid_bits_per_sample(), requested.valid_bits_per_sample())) {
    FDF_LOG(ERROR, "Ring Buffer valid bits per sample for Ring Buffer id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(*supported.frame_rates(), requested.frame_rate())) {
    FDF_LOG(ERROR, "Ring Buffer frame rate for Ring Buffer id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  auto& engine = engines_[ring_buffer_index];
  if (engine.ring_buffer) {
    // If it exists, we unbind the previous ring buffer channel with a ZX_ERR_NO_RESOURCES
    // epitaph to convey that the previous ring buffer resource is not there anymore.
    engine.ring_buffer->Unbind(ZX_ERR_NO_RESOURCES);
  }
  current_ring_buffer_formats_[ring_buffer_index].emplace(request.format());
  engine.ring_buffer = RingBufferServer::CreateRingBufferServer(dispatcher(), *this, engine,
                                                                std::move(request.ring_buffer()));
  completer.Reply(zx::ok());
}

void AudioCompositeServer::GetDaiFormats(GetDaiFormatsRequest& request,
                                         GetDaiFormatsCompleter::Sync& completer) {
  auto dai = std::find(kDaiIds.begin(), kDaiIds.end(), request.processing_element_id());
  if (dai == kDaiIds.end()) {
    FDF_LOG(ERROR, "Unknown DAI id (%lu) for GetDaiFormats", request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  size_t dai_index = dai - kDaiIds.begin();
  ZX_ASSERT(dai_index < kNumberOfPipelines);
  completer.Reply(zx::ok(std::vector{supported_dai_formats_[dai_index]}));
}

void AudioCompositeServer::SetDaiFormat(SetDaiFormatRequest& request,
                                        SetDaiFormatCompleter::Sync& completer) {
  auto dai = std::find(kDaiIds.begin(), kDaiIds.end(), request.processing_element_id());
  if (dai == kDaiIds.end()) {
    FDF_LOG(ERROR, "Unknown DAI id (%lu) for SetDaiFormat", request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  size_t dai_index = dai - kDaiIds.begin();
  ZX_ASSERT(dai_index < kNumberOfPipelines);
  auto& supported = supported_dai_formats_[dai_index];

  if (!contains(supported.number_of_channels(), request.format().number_of_channels())) {
    FDF_LOG(ERROR, "DAI format number of channels for DAI id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (request.format().channels_to_use_bitmask() >= (1u << request.format().number_of_channels())) {
    FDF_LOG(ERROR, "DAI format channels-to-use 0x%zx out of range, for DAI id (%lu)",
            request.format().channels_to_use_bitmask(), request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(supported.sample_formats(), request.format().sample_format())) {
    FDF_LOG(ERROR, "DAI format sample format for DAI id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(supported.frame_formats(), request.format().frame_format())) {
    FDF_LOG(ERROR, "DAI format frame format for DAI id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(supported.frame_rates(), request.format().frame_rate())) {
    FDF_LOG(ERROR, "DAI format frame rate for DAI id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(supported.bits_per_slot(), request.format().bits_per_slot())) {
    FDF_LOG(ERROR, "DAI format bits per slot for DAI id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!contains(supported.bits_per_sample(), request.format().bits_per_sample())) {
    FDF_LOG(ERROR, "DAI format bits per sample for DAI id (%lu) not supported",
            request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  current_dai_formats_[dai_index] = request.format();
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    if (engines_[i].dai_index == dai_index) {
      if (zx_status_t status = ResetEngine(i); status != ZX_OK) {
        completer.Reply(zx::error(ZxStatusToDriverError(status)));
        return;
      }
    }
  }
  completer.Reply(zx::ok());
}

zx_status_t AudioCompositeServer::StartSocPower() {
  // TODO(b/309153055): Use this method when we integrate with Power Framework.
  // Only if needed (not done previously) so voting on relevant clock ids is not repeated.
  // Each driver instance (audio or any other) may vote independently.
  if (soc_power_started_ == true) {
    return ZX_OK;
  }
  fidl::WireResult clock_gate_result = clock_gate_->Enable();
  if (!clock_gate_result.ok()) {
    FDF_LOG(ERROR, "Failed to send request to enable clock gate: %s",
            clock_gate_result.status_string());
    return clock_gate_result.status();
  }
  if (clock_gate_result->is_error()) {
    FDF_LOG(ERROR, "Send request to enable clock gate error: %s",
            zx_status_get_string(clock_gate_result->error_value()));
    return clock_gate_result->error_value();
  }
  fidl::WireResult pll_result = pll_->Enable();
  if (!pll_result.ok()) {
    FDF_LOG(ERROR, "Failed to send request to enable PLL: %s", pll_result.status_string());
    return pll_result.status();
  }
  if (pll_result->is_error()) {
    FDF_LOG(ERROR, "Send request to enable PLL error: %s",
            zx_status_get_string(pll_result->error_value()));
    return pll_result->error_value();
  }
  constexpr uint32_t kMsecsToStabilizePll = 10;
  zx::nanosleep(zx::deadline_after(zx::msec(kMsecsToStabilizePll)));
  for (auto& gpio_sclk_client : gpio_sclk_clients_) {
    constexpr uint32_t kSclkAltFunction = 1;
    fidl::WireResult result = gpio_sclk_client->SetAltFunction(kSclkAltFunction);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to send request to set GPIO function: %s", result.status_string());
      return result.status();
    }
  }
  soc_power_started_ = true;
  return ZX_OK;
}

zx_status_t AudioCompositeServer::StopSocPower() {
  // TODO(b/309153055): Use this method when we integrate with Power Framework.
  // Only if needed (not done previously) so voting on relevant clock ids is not repeated.
  // Each driver instance (audio or any other) may vote independently.
  if (soc_power_started_ == false) {
    return ZX_OK;
  }
  for (auto& gpio_sclk_client : gpio_sclk_clients_) {
    constexpr uint32_t kGpioAltFunction = 0;
    fidl::WireResult alt_function_result = gpio_sclk_client->SetAltFunction(kGpioAltFunction);
    if (!alt_function_result.ok()) {
      FDF_LOG(ERROR, "Failed to send request to set GPIO function: %s",
              alt_function_result.status_string());
      return alt_function_result.status();
    }
    fidl::WireResult config_out_result = gpio_sclk_client->ConfigOut(0);
    if (!config_out_result.ok()) {
      FDF_LOG(ERROR, "Failed to send request to set GPIO output: %s",
              config_out_result.status_string());
      return config_out_result.status();
    }
  }

  // MMIO access is still valid after clock gating the audio subsystem.
  fidl::WireResult clock_gate_result = clock_gate_->Disable();
  if (!clock_gate_result.ok()) {
    FDF_LOG(ERROR, "Failed to send request to disable clock gate: %s",
            clock_gate_result.status_string());
    return clock_gate_result.status();
  }
  if (clock_gate_result->is_error()) {
    FDF_LOG(ERROR, "Send request to disable clock gate error: %s",
            zx_status_get_string(clock_gate_result->error_value()));
    return clock_gate_result->error_value();
  }
  // MMIO access is still valid after disableing the PLL used.
  fidl::WireResult pll_result = pll_->Disable();
  if (!pll_result.ok()) {
    FDF_LOG(ERROR, "Failed to send request to disable PLL: %s", pll_result.status_string());
    return pll_result.status();
  }
  if (pll_result->is_error()) {
    FDF_LOG(ERROR, "Send request to disable PLL error: %s",
            zx_status_get_string(pll_result->error_value()));
    return pll_result->error_value();
  }
  soc_power_started_ = false;
  return ZX_OK;
}

// static
std::unique_ptr<RingBufferServer> RingBufferServer::CreateRingBufferServer(
    async_dispatcher_t* dispatcher, AudioCompositeServer& owner, Engine& engine,
    fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer> ring_buffer) {
  return std::make_unique<RingBufferServer>(dispatcher, owner, engine, std::move(ring_buffer));
}

RingBufferServer::RingBufferServer(async_dispatcher_t* dispatcher, AudioCompositeServer& owner,
                                   Engine& engine,
                                   fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer> ring_buffer)

    : engine_(engine),
      dispatcher_(dispatcher),
      owner_(owner),
      binding_(fidl::ServerBinding<fuchsia_hardware_audio::RingBuffer>(
          dispatcher, std::move(ring_buffer), this,
          std::mem_fn(&RingBufferServer::OnRingBufferClosed))) {
  ResetRingBuffer();
}

void RingBufferServer::OnRingBufferClosed(fidl::UnbindInfo info) {
  if (info.is_peer_closed()) {
    FDF_LOG(INFO, "Client disconnected");
  } else if (!info.is_user_initiated()) {
    // Do not log canceled cases; these happen particularly frequently in certain test cases.
    if (info.status() != ZX_ERR_CANCELED) {
      FDF_LOG(ERROR, "Client connection unbound: %s", info.status_string());
    }
  }

  ResetRingBuffer();
}

void RingBufferServer::ResetRingBuffer() {
  delay_completer_.first_response_sent = false;
  delay_completer_.completer = {};

  position_completer_.reset();
  expected_notifications_per_ring_ = 0;
  notify_timer_.Cancel();
  notification_period_ = {};

  fetched_ = false;
  engine_.device->Stop();
  started_ = false;
  pinned_ring_buffer_.Unpin();
}

void RingBufferServer::GetProperties(
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync& completer) {
  fuchsia_hardware_audio::RingBufferProperties properties;
  properties.needs_cache_flush_or_invalidate(true).driver_transfer_bytes(
      engine_.device->fifo_depth());
  completer.Reply(std::move(properties));
}

void RingBufferServer::GetVmo(
    GetVmoRequest& request,
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetVmoCompleter::Sync& completer) {
  if (started_) {
    FDF_LOG(ERROR, "GetVmo failed, ring buffer started");
    binding_.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (fetched_) {
    FDF_LOG(ERROR, "GetVmo failed, VMO already retrieved");
    binding_.Close(ZX_ERR_BAD_STATE);
    return;
  }
  uint32_t frame_size =
      engine_.config.ring_buffer.number_of_channels * engine_.config.ring_buffer.bytes_per_sample;
  size_t ring_buffer_size = fbl::round_up<size_t, size_t>(
      request.min_frames() * frame_size + engine_.device->fifo_depth(),
      std::lcm(frame_size, engine_.device->GetBufferAlignment()));
  size_t out_frames = ring_buffer_size / frame_size;
  if (out_frames > std::numeric_limits<uint32_t>::max()) {
    FDF_LOG(ERROR, "out frames too big: %zu", out_frames);
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }
  zx_status_t status = InitBuffer(ring_buffer_size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "failed to init buffer: %s", zx_status_get_string(status));
    completer.Close(status);
    return;
  }

  constexpr uint32_t rights = ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP | ZX_RIGHT_TRANSFER;
  zx::vmo buffer;
  status = ring_buffer_vmo_.duplicate(rights, &buffer);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "GetVmo failed, could not duplicate VMO: %s", zx_status_get_string(status));
    completer.Close(status);
    return;
  }

  status = engine_.device->SetBuffer(pinned_ring_buffer_.region(0).phys_addr, ring_buffer_size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "failed to set buffer: %s", zx_status_get_string(status));
    completer.Close(status);
    return;
  }

  expected_notifications_per_ring_.store(request.clock_recovery_notifications_per_ring());
  fetched_ = true;
  // This is safe because of the overflow check we made above.
  auto out_num_rb_frames = static_cast<uint32_t>(out_frames);
  completer.Reply(zx::ok(
      fuchsia_hardware_audio::RingBufferGetVmoResponse(out_num_rb_frames, std::move(buffer))));
}

void RingBufferServer::Start(StartCompleter::Sync& completer) {
  int64_t start_time = 0;
  if (started_) {
    FDF_LOG(ERROR, "Could not start: already started");
    binding_.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (!fetched_) {
    FDF_LOG(ERROR, "Could not start: first, GetVmo must successfully complete");
    binding_.Close(ZX_ERR_BAD_STATE);
    return;
  }

  start_time = engine_.device->Start();
  started_ = true;

  uint32_t notifs = expected_notifications_per_ring_.load();
  if (notifs) {
    uint32_t frame_size =
        engine_.config.ring_buffer.number_of_channels * engine_.config.ring_buffer.bytes_per_sample;
    ZX_ASSERT(engine_.dai_index < kNumberOfPipelines);
    uint32_t frame_rate = owner_.current_dai_formats(engine_.dai_index).frame_rate();

    // Notification period in usecs scaling by 1'000s to provide good enough resolution in the
    // integer calculation.
    notification_period_ = zx::usec(1'000 * pinned_ring_buffer_.region(0).size /
                                    (frame_size * frame_rate / 1'000 * notifs));
    notify_timer_.PostDelayed(dispatcher_, notification_period_);
  } else {
    notification_period_ = {};
  }
  completer.Reply(start_time);
}

void RingBufferServer::Stop(StopCompleter::Sync& completer) {
  if (!fetched_) {
    FDF_LOG(ERROR, "GetVmo must successfully complete before calling Start or Stop");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (!started_) {
    FDF_LOG(DEBUG, "Stop called while stopped; this is allowed");
  }

  notify_timer_.Cancel();
  notification_period_ = {};

  engine_.device->Stop();
  started_ = false;

  completer.Reply();
}

zx_status_t RingBufferServer::InitBuffer(size_t size) {
  pinned_ring_buffer_.Unpin();
  zx_status_t status = zx_vmo_create_contiguous(owner_.bti().get(), size, 0,
                                                ring_buffer_vmo_.reset_and_get_address());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "failed to allocate ring buffer vmo: %s", zx_status_get_string(status));
    return status;
  }

  status =
      pinned_ring_buffer_.Pin(ring_buffer_vmo_, owner_.bti(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "failed to pin ring buffer vmo: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

void AudioCompositeServer::TestPowerManagement() {
  pm_timer_.PostDelayed(dispatcher_, zx::sec(1));
  static bool enabled = true;
  if (enabled) {
    StartSocPower();
  } else {
    StopSocPower();
  }
  enabled = !enabled;
}

void RingBufferServer::ProcessRingNotification() {
  if (notification_period_.get()) {
    notify_timer_.PostDelayed(dispatcher_, notification_period_);
  } else {
    notify_timer_.Cancel();
    return;
  }
  if (position_completer_) {
    fuchsia_hardware_audio::RingBufferPositionInfo info;
    info.position(engine_.device->GetRingPosition());
    info.timestamp(zx::clock::get_monotonic().get());
    position_completer_->Reply(std::move(info));
    position_completer_.reset();
  }
}

void RingBufferServer::WatchClockRecoveryPositionInfo(
    WatchClockRecoveryPositionInfoCompleter::Sync& completer) {
  if (position_completer_) {
    // The client called WatchClockRecoveryPositionInfo when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    FDF_LOG(ERROR,
            "WatchClockRecoveryPositionInfo was re-called while the previous call was still"
            " pending");
    completer.Close(ZX_ERR_BAD_STATE);
  } else {
    // This completer is kept and responded in ProcessRingNotification.
    position_completer_.emplace(completer.ToAsync());
  }
}

void RingBufferServer::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) {
  if (!delay_completer_.first_response_sent) {
    delay_completer_.first_response_sent = true;

    fuchsia_hardware_audio::DelayInfo delay_info = {};
    // No external delay information is provided by this driver.
    delay_info.internal_delay(internal_delay_.to_nsecs());
    completer.Reply(std::move(delay_info));
  } else if (delay_completer_.completer) {
    // The client called WatchDelayInfo when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    FDF_LOG(ERROR, "WatchDelayInfo was re-called while the previous call was still pending");
    completer.Close(ZX_ERR_BAD_STATE);
  } else {
    // This completer is kept but never used since we are not updating the delay info.
    delay_completer_.completer.emplace(completer.ToAsync());
  }
}

void RingBufferServer::SetActiveChannels(
    fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
    SetActiveChannelsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void AudioCompositeServer::GetElements(GetElementsCompleter::Sync& completer) {
  std::vector<fuchsia_hardware_audio_signalprocessing::Element> elements;

  // One ring buffer per TDM engine.
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    fuchsia_hardware_audio_signalprocessing::Element ring_buffer;
    fuchsia_hardware_audio_signalprocessing::Endpoint ring_buffer_endpoint;
    ring_buffer_endpoint.type(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer)
        .plug_detect_capabilities(
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired);
    ring_buffer.id(kRingBufferIds[i])
        .type(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint)
        .type_specific(fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint(
            std::move(ring_buffer_endpoint)));
    elements.push_back(std::move(ring_buffer));
  }
  // One DAI per pipeline.
  for (size_t i = 0; i < kNumberOfPipelines; ++i) {
    fuchsia_hardware_audio_signalprocessing::Element dai;
    fuchsia_hardware_audio_signalprocessing::Endpoint dai_endpoint;
    dai_endpoint.type(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect)
        .plug_detect_capabilities(
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired);
    dai.id(kDaiIds[i])
        .type(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint)
        .type_specific(fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint(
            std::move(dai_endpoint)));
    elements.push_back(std::move(dai));
  }

  completer.Reply(zx::ok(elements));
}

void AudioCompositeServer::WatchElementState(WatchElementStateRequest& request,
                                             WatchElementStateCompleter::Sync& completer) {
  auto element_completer = element_completers_.find(request.processing_element_id());
  if (element_completer == element_completers_.end()) {
    FDF_LOG(ERROR, "Unknown process element id (%lu) for WatchElementState",
            request.processing_element_id());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& element = element_completer->second;
  if (!element.first_response_sent) {
    element.first_response_sent = true;
    // All elements are endpoints, hardwired hence plugged at time 0.
    fuchsia_hardware_audio_signalprocessing::PlugState plug_state;
    plug_state.plugged(true).plug_state_time(0);
    fuchsia_hardware_audio_signalprocessing::EndpointElementState endpoint_state;
    endpoint_state.plug_state(std::move(plug_state));
    fuchsia_hardware_audio_signalprocessing::ElementState element_state;
    element_state.type_specific(
        fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint(
            std::move(endpoint_state)));
    completer.Reply(std::move(element_state));
  } else if (element.completer) {
    // The client called WatchElementState when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    FDF_LOG(ERROR, "WatchElementState was re-called while the previous call was still pending");
    completer.Close(ZX_ERR_BAD_STATE);
  } else {
    // This completer is kept but never used since we are not updating the state of the elements.
    element.completer = completer.ToAsync();
  }
}

void AudioCompositeServer::SetElementState(SetElementStateRequest& request,
                                           SetElementStateCompleter::Sync& completer) {
  auto element_completer = element_completers_.find(request.processing_element_id());
  if (element_completer == element_completers_.end()) {
    FDF_LOG(ERROR, "Unknown process element id (%lu) for SetElementState",
            request.processing_element_id());
    // Return an error, but no need to close down the entire protocol channel.
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  // All elements are endpoints, no field is expected or acted upon.
  completer.Reply(zx::ok());
}

void AudioCompositeServer::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  fuchsia_hardware_audio_signalprocessing::Topology topology;
  topology.id(kTopologyId);
  std::vector<fuchsia_hardware_audio_signalprocessing::EdgePair> edges;
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    if (!engines_[i].config.is_input) {
      //                        +-----------+    +-----------+
      //        Source       -> |  ENDPOINT | -> +  ENDPOINT | ->     Destination
      //      from client       | RingBuffer|    +    DAI    |    e.g. Bluetooth chip
      //                        +-----------+    +-----------+
      fuchsia_hardware_audio_signalprocessing::EdgePair pair;
      pair.processing_element_id_from(kRingBufferIds[i])
          .processing_element_id_to(kDaiIds[engines_[i].dai_index]);
      edges.push_back(std::move(pair));
    } else {
      //                        +-----------+    +-----------+
      //       Source        -> |  ENDPOINT | -> +  ENDPOINT | ->     Destination
      // e.g. Bluetooth chip    |    DAI    |    + RingBuffer|         to client
      //                        +-----------+    +-----------+
      fuchsia_hardware_audio_signalprocessing::EdgePair pair;
      pair.processing_element_id_from(kDaiIds[engines_[i].dai_index])
          .processing_element_id_to(kRingBufferIds[i]);
      edges.push_back(std::move(pair));
    }
  }

  topology.processing_elements_edge_pairs(edges);
  completer.Reply(zx::ok(std::vector{std::move(topology)}));
}

void AudioCompositeServer::WatchTopology(WatchTopologyCompleter::Sync& completer) {
  if (!topology_completer_.first_response_sent) {
    topology_completer_.first_response_sent = true;
    completer.Reply(kTopologyId);
  } else if (topology_completer_.completer) {
    // The client called WatchTopology when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    FDF_LOG(ERROR, "WatchTopology was re-called while the previous call was still pending");
    completer.Close(ZX_ERR_BAD_STATE);
  } else {
    // This completer is kept but never used since we are not updating the topology.
    topology_completer_.completer.emplace(completer.ToAsync());
  }
}

// This device has only one signalprocessing topology.
void AudioCompositeServer::SetTopology(SetTopologyRequest& request,
                                       SetTopologyCompleter::Sync& completer) {
  if (request.topology_id() == kTopologyId) {
    completer.Reply(zx::ok());
  } else {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
  }
}

}  // namespace audio::aml_g12
