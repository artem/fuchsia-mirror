// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "thermistor-channel.h"

#include <ddktl/fidl.h>

namespace thermal {

void ThermistorChannel::GetTemperatureCelsius(GetTemperatureCelsiusCompleter::Sync& completer) {
  uint32_t sample;
  zx_status_t status = adc_->GetSample(adc_channel_, &sample);
  if (status != ZX_OK) {
    completer.Reply(status, 0.0f);
  } else {
    float norm = static_cast<float>(sample) / static_cast<float>(((1 << adc_->Resolution()) - 1));
    float temperature;
    status = ntc_.GetTemperatureCelsius(norm, &temperature);
    completer.Reply(status, temperature);
  }
}

void ThermistorChannel::GetSensorName(GetSensorNameCompleter::Sync& completer) {
  completer.Reply(fidl::StringView::FromExternal(name_));
}

void RawChannel::GetSample(GetSampleCompleter::Sync& completer) {
  uint32_t sample;
  zx_status_t status = adc_->GetSample(adc_channel_, &sample);
  if (status == ZX_OK) {
    completer.ReplySuccess(sample);
  } else {
    completer.ReplyError(status);
  }
}

void RawChannel::GetNormalizedSample(GetNormalizedSampleCompleter::Sync& completer) {
  uint32_t sample;
  zx_status_t status = adc_->GetSample(adc_channel_, &sample);
  if (status == ZX_OK) {
    completer.ReplySuccess(static_cast<float>(sample) /
                           static_cast<float>(((1 << adc_->Resolution()) - 1)));

  } else {
    completer.ReplyError(status);
  }
}

void RawChannel::GetResolution(GetResolutionCompleter::Sync& completer) {
  completer.ReplySuccess(adc_->Resolution());
}
}  // namespace thermal
