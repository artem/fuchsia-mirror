// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/media/cpp/fidl.h>
#include <fuchsia/media/tuning/cpp/fidl.h>

#include "src/media/audio/audio_core/testing/integration/hermetic_audio_test.h"

namespace media::audio::test {

class AudioTunerTest : public HermeticAudioTest {
 protected:
  void TearDown() override {
    audio_renderer_.Unbind();
    audio_capturer_.Unbind();

    HermeticAudioTest::TearDown();
  }

 private:
  fuchsia::media::AudioRendererPtr audio_renderer_;
  fuchsia::media::AudioCapturerPtr audio_capturer_;
};

// Test that the user is connected to the audio tuner.
// TODO(https://fxbug.dev/42130322): Flesh out
TEST_F(AudioTunerTest, ConnectToAudioTuner) {
  fuchsia::media::tuning::AudioTunerPtr audio_tuner;
  realm().Connect(audio_tuner.NewRequest());
  AddErrorHandler(audio_tuner, "AudioTuner");
  audio_tuner->GetAvailableAudioEffects(AddCallback("GetAvailableAudioEffects"));
  ExpectCallbacks();
}

}  // namespace media::audio::test
