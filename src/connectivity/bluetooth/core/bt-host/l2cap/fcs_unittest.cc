// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found
// in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/fcs.h"

#include <gtest/gtest.h>

namespace bt::l2cap {
namespace {

constexpr const char kTestData[] = "🍜🥯🍕🥖🍞🍩";  // Carb-heavy dataset
const BufferView kTestBuffer = BufferView(kTestData, sizeof(kTestData) - 1);

TEST(FcsTest, EmptyBufferProducesInitialValue) {
  EXPECT_EQ(0, ComputeFcs(BufferView()).fcs);
  EXPECT_EQ(5, ComputeFcs(BufferView(), FrameCheckSequence{5}).fcs);
}

TEST(FcsTest, FcsOfSimpleValues) {
  // By inspection, the FCS has value zero if all inputs are 0.
  EXPECT_EQ(0, ComputeFcs(StaticByteBuffer(0).view()).fcs);

  // If only the "last" bit (i.e. MSb of the message) is set, then the FCS
  // should equal the generator polynomial because there's exactly one round of
  // feedback.
  EXPECT_EQ(0b1010'0000'0000'0001,
            ComputeFcs(StaticByteBuffer(0b1000'0000).view()).fcs);
}

TEST(FcsTest, Example1) {
  // Core Spec v5.0, Vol 3, Part A, Section 3.3.5, Example 1.
  const StaticByteBuffer kExample1Data(0x0E,
                                       0x00,
                                       0x40,
                                       0x00,
                                       0x02,
                                       0x00,
                                       0x00,
                                       0x01,
                                       0x02,
                                       0x03,
                                       0x04,
                                       0x05,
                                       0x06,
                                       0x07,
                                       0x08,
                                       0x09);
  EXPECT_EQ(0x6138, ComputeFcs(kExample1Data.view()).fcs);
}

TEST(FcsTest, Example2) {
  // Core Spec v5.0, Vol 3, Part A, Section 3.3.5, Example 2.
  const StaticByteBuffer kExample2Data(0x04, 0x00, 0x40, 0x00, 0x01, 0x01);
  EXPECT_EQ(0x14D4, ComputeFcs(kExample2Data.view()).fcs);
}

TEST(FcsTest, FcsOfSlicesSameAsFcsOfWhole) {
  const FrameCheckSequence whole_fcs = ComputeFcs(kTestBuffer);
  const auto slice0 = kTestBuffer.view(0, 4);
  const auto slice1 = kTestBuffer.view(slice0.size());
  const FrameCheckSequence sliced_fcs = ComputeFcs(slice1, ComputeFcs(slice0));
  EXPECT_EQ(whole_fcs.fcs, sliced_fcs.fcs);
}

}  // namespace
}  // namespace bt::l2cap
