// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/lib/analysis/dropout.h"

#include <cmath>
#include <limits>

#include <gtest/gtest.h>

#include "src/media/audio/lib/analysis/generators.h"

using ASF = fuchsia::media::AudioSampleFormat;

namespace media::audio {

TEST(DropoutDetectors, PowerChecker_Constant) {
  constexpr int32_t kSamplesPerSecond = 48000;
  constexpr float kConstValue = 0.12345f;
  const auto format = Format::Create<ASF::FLOAT>(1, kSamplesPerSecond).take_value();
  auto buf = GenerateConstantAudio(format, 8, kConstValue);

  PowerChecker checker(4, 1, kConstValue);
  for (size_t i = 0; i <= 4; ++i) {
    EXPECT_TRUE(checker.Check(&buf.samples()[i], i, 4, true))
        << "samples [" << i << "] to [" << i + 3 << "]";
  }

  for (size_t i = 0; i < 8; ++i) {
    EXPECT_TRUE(checker.Check(&buf.samples()[i], i, 1, true)) << "sample[" << i << "]";
  }

  // Check a good signal, then inject a glitch and check that it is detected.
  buf.samples()[3] = 0.0f;

  for (size_t i = 0; i < 4; ++i) {
    EXPECT_FALSE(checker.Check(&buf.samples()[i], i, 4, false))
        << "samples [" << i << "] to [" << i + 3 << "]";
  }
  EXPECT_TRUE(checker.Check(&buf.samples()[4], 4, 4, true)) << "samples [4] to [7]";

  for (size_t i = 0; i < 8; ++i) {
    if (i == 3) {
      EXPECT_FALSE(checker.Check(&buf.samples()[i], i, 1, false)) << "sample[" << i << "]";
    } else {
      EXPECT_TRUE(checker.Check(&buf.samples()[i], i, 1, true)) << "sample[" << i << "]";
    }
  }
}

TEST(DropoutDetectors, PowerChecker_Sine) {
  constexpr int32_t kSamplesPerSecond = 48000;
  constexpr double kRelativeFreq = 1.0;
  const auto format = Format::Create<ASF::FLOAT>(1, kSamplesPerSecond).take_value();
  auto buf = GenerateCosineAudio(format, 8, kRelativeFreq);

  PowerChecker checker(4, 1, M_SQRT1_2);
  for (size_t i = 0; i <= 4; ++i) {
    EXPECT_TRUE(checker.Check(&buf.samples()[i], i, 4, true))
        << "samples [" << i << "] to [" << i + 3 << "]";
  }

  for (size_t i = 0; i < 8; ++i) {
    EXPECT_TRUE(checker.Check(&buf.samples()[i], i, 1, true)) << "sample[" << i << "]";
  }

  // Check a good signal, then inject a glitch and check that's detected.
  buf.samples()[3] = 0.0f;

  for (size_t i = 0; i < 4; ++i) {
    EXPECT_FALSE(checker.Check(&buf.samples()[i], i, 4, false))
        << "samples [" << i << "] to [" << i + 3 << "]";
  }
  EXPECT_TRUE(checker.Check(&buf.samples()[4], 4, 4, true)) << "samples [4] to [7]";
}

TEST(DropoutDetectors, PowerChecker_Reset) {
  constexpr float kConstValue = 0.12345f;
  constexpr float kBadValue = 0.01234f;
  PowerChecker checker(3, 1, kConstValue);

  // We haven't finished the next RMS window, so we won't fail yet.
  EXPECT_TRUE(checker.Check(&kBadValue, 0, 1, true));
  EXPECT_TRUE(checker.Check(&kBadValue, 1, 1, true));
  EXPECT_FALSE(checker.Check(&kBadValue, 2, 1, false));

  EXPECT_TRUE(checker.Check(&kBadValue, 3, 1, true));
  EXPECT_TRUE(checker.Check(&kBadValue, 4, 1, true));

  // Without Reset(), this Check() should fail.
  checker.Reset();
  EXPECT_TRUE(checker.Check(&kBadValue, 5, 1, true));
  EXPECT_TRUE(checker.Check(&kBadValue, 6, 1, true));

  // Because of position discontinuity, these Check()s will pass.
  EXPECT_TRUE(checker.Check(&kBadValue, 8, 1, true));
  EXPECT_TRUE(checker.Check(&kBadValue, 9, 1, true));

  EXPECT_FALSE(checker.Check(&kBadValue, 10, 1, false));
}

// These test cases must be carefully written, because SilenceChecker::kOnlyLogFailureOnEnd might be
// set, which means we don't log our ERROR message (even if Check returns false) until the silent
// range of frames ends (even if that particular Check returns true).
TEST(DropoutDetectors, SilenceChecker_Reset) {
  // Allow two consecutive silent frames, but not three.
  SilenceChecker checker(2, 1);

  float bad_vals[] = {0.0f, 0.0f, 0.0f, 0.5f};
  EXPECT_FALSE(checker.Check(bad_vals, 0, 3, false));

  // Ensure that the counter is NOT reset after the above failure (increments from 3 to 4).
  EXPECT_FALSE(checker.Check(bad_vals, 3, 4, false));

  // Ensure that the counter is reset (from 2 silent frames to 0) by position discontinuity.
  EXPECT_TRUE(checker.Check(bad_vals, 6, 2, true));
  EXPECT_TRUE(checker.Check(bad_vals, 0, 2, true));

  // Ensure that the counter is reset by a good value.
  auto non_silent_val = bad_vals[3];
  EXPECT_TRUE(checker.Check(&non_silent_val, 2, 1, true));
  EXPECT_TRUE(checker.Check(bad_vals, 3, 2, true));

  // Ensure that the counter is reset by an explicit Reset().
  checker.Reset(0, true);
  EXPECT_TRUE(checker.Check(bad_vals, 0, 2, true));
}

// All samples in a frame must be silent, to qualify as a silent frame for this checker.
TEST(DropoutDetectors, SilenceChecker_EntireFrame) {
  // Allow one silent frame but not two.
  SilenceChecker checker(1, 2);

  float source_data[12] = {
      0.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f,
  };
  EXPECT_TRUE(checker.Check(source_data, 0, 6, true));
  EXPECT_FALSE(checker.Check(source_data + 1, 1, 5, false));
}

TEST(DropoutDetectors, SilenceChecker_Sine) {
  constexpr int32_t kSamplesPerSecond = 48000;
  constexpr double kRelativeFreq = 1.0;
  const auto format = Format::Create<ASF::FLOAT>(1, kSamplesPerSecond).take_value();
  auto buf = GenerateCosineAudio(format, 8, kRelativeFreq);

  // Allow a silent frame, but not two consecutive ones.
  SilenceChecker checker(1, 1);

  EXPECT_TRUE(checker.Check(buf.samples().data(), 0, 8, true));

  // Now inject a glitch and ensure it is detected.
  // Exactly one wavelength of our cosine signal fits into the 8-sample buffer. This means that the
  // values at indices [2] and [6] will be zero. Thus, setting [5] to zero should cause a failure.
  buf.samples()[5] = 0.0f;
  EXPECT_FALSE(checker.Check(buf.samples().data(), 0, 8, false));
}

// Values as far from zero as +/-numeric_limits<float>::epsilon() are still considered silent.
TEST(DropoutDetectors, SilenceChecker_Epsilon) {
  SilenceChecker checker(1, 1);

  float bad_vals[] = {
      std::numeric_limits<float>::epsilon(), -1.0f * std::numeric_limits<float>::epsilon(),
      2.0f * std::numeric_limits<float>::epsilon(), -2.0f * std::numeric_limits<float>::epsilon()};
  EXPECT_TRUE(checker.Check(&bad_vals[0], 0, 1, true));
  EXPECT_FALSE(checker.Check(&bad_vals[1], 1, 2, false));

  EXPECT_TRUE(checker.Check(&bad_vals[2], 2, 2, true));
}

}  // namespace media::audio
