// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/credit_based_flow_control_tx_engine.h"

#include <gtest/gtest.h>
#include <pw_async/fake_dispatcher_fixture.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/fake_tx_channel.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/frame_headers.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"

namespace bt::l2cap::internal {
namespace {

using Engine = CreditBasedFlowControlTxEngine;

class CreditBasedFlowControlTxEngineTest : public ::testing::Test {
 protected:
  static constexpr ChannelId kTestChannelId = 170u;
  static constexpr auto kTestMtu = 256u;
  static constexpr auto kTestMps = 64u;
  static constexpr auto kInitialCredits = 1u;

  void SetUp() override {
    channel_.HandleSendFrame(
        [this](ByteBufferPtr pdu) { sent_frames().push_back(std::move(pdu)); });
  }

  Engine& engine() { return *engine_; }
  std::vector<ByteBufferPtr>& sent_frames() { return sent_frames_; }
  FakeTxChannel& channel() { return channel_; }

  void ProcessSdu(ByteBufferPtr sdu) {
    channel().QueueSdu(std::move(sdu));
    engine().NotifySduQueued();
  }

 private:
  std::unique_ptr<Engine> engine_ = std::make_unique<Engine>(
      kTestChannelId,
      kTestMtu,
      channel_,
      CreditBasedFlowControlMode::kLeCreditBasedFlowControl,
      kTestMps,
      kInitialCredits);

  std::vector<ByteBufferPtr> sent_frames_{};

  FakeTxChannel channel_{};
};

TEST_F(CreditBasedFlowControlTxEngineTest, SendBasicPayload) {
  StaticByteBuffer<4> basic{'t', 'e', 's', 't'};
  ProcessSdu(std::make_unique<DynamicByteBuffer>(basic));

  ASSERT_EQ(sent_frames().size(), 1u);
  auto& sent = sent_frames()[0];

  ASSERT_TRUE(sent);
  EXPECT_EQ(sent->size(), 10u);
  EXPECT_EQ(channel().queue_size(), 0u);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<10> expected{
      // PDU size field (LE u16)
      6, 0,
      // Channel field (LE u16)
      kTestChannelId, 0,
      // SDU size field (LE u16)
      4, 0,
      // Payload
      't', 'e', 's', 't',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent, expected));
}

TEST_F(CreditBasedFlowControlTxEngineTest, SendSegmentedPayload) {
  StaticByteBuffer<72> segmented{
      'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g',
      'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f',
      'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e',
      'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd',
      'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h',
  };

  // Make sure credits are available to send the entire payload.
  EXPECT_TRUE(engine().AddCredits(5));
  ProcessSdu(std::make_unique<DynamicByteBuffer>(segmented));
  EXPECT_EQ(channel().queue_size(), 0u);

  ASSERT_EQ(sent_frames().size(), 2u);
  auto& sent_first = sent_frames()[0];
  auto& sent_second = sent_frames()[1];

  ASSERT_TRUE(sent_first);
  EXPECT_EQ(sent_first->size(), kTestMps);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<kTestMps> expected_first{
      // PDU size field (LE u16)
      (kTestMps - 4), 0,
      // Channel field (LE u16)
      kTestChannelId, 0,
      // SDU size field (LE u16)
      72, 0,
      // Payload
      'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g',
      'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f',
      'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e',
      'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b',
  };
  // clang-format on

  ASSERT_TRUE(sent_second);
  EXPECT_EQ(sent_second->size(), 18u);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<18> expected_second{
      // PDU size field (LE u16)
      14, 0,
      // Channel field (LE u16)
      kTestChannelId, 0,
      // Payload
      'c', 'd', 'e', 'f', 'g', 'h', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_first, expected_first));
  EXPECT_TRUE(ContainersEqual(*sent_second, expected_second));
}

TEST_F(CreditBasedFlowControlTxEngineTest, NoSendWithoutCreditsBasic) {
  StaticByteBuffer<5> first{'f', 'i', 'r', 's', 't'};
  StaticByteBuffer<6> second{'s', 'e', 'c', 'o', 'n', 'd'};

  ProcessSdu(std::make_unique<DynamicByteBuffer>(first));

  ASSERT_EQ(sent_frames().size(), 1u);
  auto& sent_first = sent_frames()[0];

  ASSERT_TRUE(sent_first);
  EXPECT_EQ(sent_first->size(), 11u);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<11> expected_first{
      // PDU size field (LE u16)
      7, 0,
      // Channel field (LE u16)
      kTestChannelId, 0,
      // SDU size field (LE u16)
      5, 0,
      // Payload
      'f', 'i', 'r', 's', 't',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_first, expected_first));
  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 0u);
  EXPECT_EQ(channel().queue_size(), 0u);

  ProcessSdu(std::make_unique<DynamicByteBuffer>(second));

  // Ensure the second send did not occur yet, as credits are exhausted.
  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 0u);
  EXPECT_EQ(sent_frames().size(), 1u);
  EXPECT_EQ(channel().queue_size(), 1u);

  engine().AddCredits(1);

  // Now confirm the send did occur.
  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 0u);
  EXPECT_EQ(channel().queue_size(), 0u);
  ASSERT_EQ(sent_frames().size(), 2u);
  auto& sent_second = sent_frames()[1];
  ASSERT_TRUE(sent_second);
  EXPECT_EQ(sent_second->size(), 12u);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<12> expected_second{
      // PDU size field (LE u16)
      8, 0,
      // Channel field (LE u16)
      170, 0,
      // SDU size field (LE u16)
      6, 0,
      // Payload
      's', 'e', 'c', 'o', 'n', 'd',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_second, expected_second));
}

TEST_F(CreditBasedFlowControlTxEngineTest, NoSendWithoutCreditsSegmented) {
  StaticByteBuffer<150> segmented{
      'L', 'o', 'r', 'e', 'm', ' ', 'i', 'p', 's', 'u', 'm', ' ', 'd', 'o',
      'l', 'o', 'r', ' ', 's', 'i', 't', ' ', 'a', 'm', 'e', 't', ',', ' ',
      'c', 'o', 'n', 's', 'e', 'c', 't', 'e', 't', 'u', 'r', ' ', 'a', 'd',
      'i', 'p', 'i', 's', 'c', 'i', 'n', 'g', ' ', 'e', 'l', 'i', 't', '.',
      ' ', 'S', 'e', 'd', ' ', 'e', 't', ' ', 'v', 'e', 'h', 'i', 'c', 'u',
      'l', 'a', ' ', 'e', 'n', 'i', 'm', '.', ' ', 'U', 't', ' ', 's', 'i',
      't', ' ', 'a', 'm', 'e', 't', ' ', 'm', 'a', 'g', 'n', 'a', ' ', 'm',
      'a', 'u', 'r', 'i', 's', '.', ' ', 'U', 't', ' ', 's', 'e', 'd', ' ',
      't', 'u', 'r', 'p', 'i', 's', ' ', 'n', 'i', 'b', 'h', '.', ' ', 'V',
      'e', 's', 't', 'i', 'b', 'u', 'l', 'u', 'm', ' ', 's', 'e', 'd', ' ',
      't', 'o', 'r', 't', 'o', 'r', ' ', 'i', 'd', '.'};

  ProcessSdu(std::make_unique<DynamicByteBuffer>(segmented));
  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 2u);
  EXPECT_EQ(channel().queue_size(), 0u);

  ASSERT_EQ(sent_frames().size(), 1u);
  auto& sent_first = sent_frames()[0];
  ASSERT_TRUE(sent_first);
  EXPECT_EQ(sent_first->size(), kTestMps);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<kTestMps> expected_first{
      // PDU size field (LE u16)
      (kTestMps - 4), 0,
      // Channel field (LE u16)
      170, 0,
      // SDU size field (LE u16)
      150, 0,
      // Payload
      'L', 'o', 'r', 'e', 'm', ' ', 'i', 'p', 's', 'u', 'm', ' ', 'd', 'o', 'l',
      'o', 'r', ' ', 's', 'i', 't', ' ', 'a', 'm', 'e', 't', ',', ' ', 'c', 'o',
      'n', 's', 'e', 'c', 't', 'e', 't', 'u', 'r', ' ', 'a', 'd', 'i', 'p', 'i',
      's', 'c', 'i', 'n', 'g', ' ', 'e', 'l', 'i', 't', '.', ' ', 'S',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_first, expected_first));

  engine().AddCredits(1);

  ASSERT_EQ(sent_frames().size(), 2u);
  auto& sent_second = sent_frames()[1];
  ASSERT_TRUE(sent_second);
  EXPECT_EQ(sent_second->size(), kTestMps);
  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 1u);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<kTestMps> expected_second{
      // PDU size field (LE u16)
      (kTestMps - 4), 0,
      // Channel field (LE u16)
      170, 0,
      // Payload
      'e', 'd', ' ', 'e', 't', ' ', 'v', 'e', 'h', 'i', 'c', 'u', 'l', 'a', ' ',
      'e', 'n', 'i', 'm', '.', ' ', 'U', 't', ' ', 's', 'i', 't', ' ', 'a', 'm',
      'e', 't', ' ', 'm', 'a', 'g', 'n', 'a', ' ', 'm', 'a', 'u', 'r', 'i', 's',
      '.', ' ', 'U', 't', ' ', 's', 'e', 'd', ' ', 't', 'u', 'r', 'p', 'i', 's',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_second, expected_second));

  engine().AddCredits(10);

  ASSERT_EQ(sent_frames().size(), 3u);
  auto& sent_third = sent_frames()[2];
  ASSERT_TRUE(sent_third);
  EXPECT_EQ(sent_third->size(), 36u);
  EXPECT_EQ(engine().credits(), 9);
  EXPECT_EQ(engine().segments_count(), 0u);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<36> expected_third{
      // PDU size field (LE u16)
      32, 0,
      // Channel field (LE u16)
      170, 0,
      // Payload
      ' ', 'n', 'i', 'b', 'h', '.', ' ', 'V', 'e', 's', 't', 'i', 'b', 'u', 'l',
      'u', 'm', ' ', 's', 'e', 'd', ' ', 't', 'o', 'r', 't', 'o', 'r', ' ', 'i',
      'd', '.'
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_third, expected_third));
}

TEST_F(CreditBasedFlowControlTxEngineTest, DoesNotAcceptSduWhilePdusQueued) {
  StaticByteBuffer<150> segmented{
      'L', 'o', 'r', 'e', 'm', ' ', 'i', 'p', 's', 'u', 'm', ' ', 'd', 'o',
      'l', 'o', 'r', ' ', 's', 'i', 't', ' ', 'a', 'm', 'e', 't', ',', ' ',
      'c', 'o', 'n', 's', 'e', 'c', 't', 'e', 't', 'u', 'r', ' ', 'a', 'd',
      'i', 'p', 'i', 's', 'c', 'i', 'n', 'g', ' ', 'e', 'l', 'i', 't', '.',
      ' ', 'S', 'e', 'd', ' ', 'e', 't', ' ', 'v', 'e', 'h', 'i', 'c', 'u',
      'l', 'a', ' ', 'e', 'n', 'i', 'm', '.', ' ', 'U', 't', ' ', 's', 'i',
      't', ' ', 'a', 'm', 'e', 't', ' ', 'm', 'a', 'g', 'n', 'a', ' ', 'm',
      'a', 'u', 'r', 'i', 's', '.', ' ', 'U', 't', ' ', 's', 'e', 'd', ' ',
      't', 'u', 'r', 'p', 'i', 's', ' ', 'n', 'i', 'b', 'h', '.', ' ', 'V',
      'e', 's', 't', 'i', 'b', 'u', 'l', 'u', 'm', ' ', 's', 'e', 'd', ' ',
      't', 'o', 'r', 't', 'o', 'r', ' ', 'i', 'd', '.'};

  ProcessSdu(std::make_unique<DynamicByteBuffer>(segmented));
  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 2u);
  EXPECT_EQ(channel().queue_size(), 0u);

  ASSERT_EQ(sent_frames().size(), 1u);
  auto& sent_first = sent_frames()[0];
  ASSERT_TRUE(sent_first);
  EXPECT_EQ(sent_first->size(), kTestMps);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<kTestMps> expected_first{
      // PDU size field (LE u16)
      (kTestMps - 4), 0,
      // Channel field (LE u16)
      170, 0,
      // SDU size field (LE u16)
      150, 0,
      // Payload
      'L', 'o', 'r', 'e', 'm', ' ', 'i', 'p', 's', 'u', 'm', ' ', 'd', 'o', 'l',
      'o', 'r', ' ', 's', 'i', 't', ' ', 'a', 'm', 'e', 't', ',', ' ', 'c', 'o',
      'n', 's', 'e', 'c', 't', 'e', 't', 'u', 'r', ' ', 'a', 'd', 'i', 'p', 'i',
      's', 'c', 'i', 'n', 'g', ' ', 'e', 'l', 'i', 't', '.', ' ', 'S',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_first, expected_first));

  StaticByteBuffer<8> next_sdu{'n', 'e', 'x', 't', '_', 's', 'd', 'u'};
  ProcessSdu(std::make_unique<DynamicByteBuffer>(next_sdu));

  EXPECT_EQ(sent_frames().size(), 1u);
  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 2u);
  EXPECT_EQ(channel().queue_size(), 1u);

  engine().AddCredits(3);

  ASSERT_EQ(sent_frames().size(), 4u);
  auto& sent_second = sent_frames()[1];
  auto& sent_third = sent_frames()[2];
  auto& sent_fourth = sent_frames()[3];

  ASSERT_TRUE(sent_second);
  EXPECT_EQ(sent_second->size(), kTestMps);

  ASSERT_TRUE(sent_third);
  EXPECT_EQ(sent_third->size(), 36u);

  ASSERT_TRUE(sent_fourth);
  EXPECT_EQ(sent_fourth->size(), 14u);

  EXPECT_EQ(engine().credits(), 0);
  EXPECT_EQ(engine().segments_count(), 0u);
  EXPECT_EQ(channel().queue_size(), 0u);

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<kTestMps> expected_second{
      // PDU size field (LE u16)
      (kTestMps - 4), 0,
      // Channel field (LE u16)
      170, 0,
      // Payload
      'e', 'd', ' ', 'e', 't', ' ', 'v', 'e', 'h', 'i', 'c', 'u', 'l', 'a', ' ',
      'e', 'n', 'i', 'm', '.', ' ', 'U', 't', ' ', 's', 'i', 't', ' ', 'a', 'm',
      'e', 't', ' ', 'm', 'a', 'g', 'n', 'a', ' ', 'm', 'a', 'u', 'r', 'i', 's',
      '.', ' ', 'U', 't', ' ', 's', 'e', 'd', ' ', 't', 'u', 'r', 'p', 'i', 's',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_second, expected_second));

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<36> expected_third{
      // PDU size field (LE u16)
      32, 0,
      // Channel field (LE u16)
      170, 0,
      // Payload
      ' ', 'n', 'i', 'b', 'h', '.', ' ', 'V', 'e', 's', 't', 'i', 'b', 'u', 'l',
      'u', 'm', ' ', 's', 'e', 'd', ' ', 't', 'o', 'r', 't', 'o', 'r', ' ', 'i',
      'd', '.'
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_third, expected_third));

  // clang-format off: Formatter wants each value on a separate line.
  StaticByteBuffer<14> expected_fourth{
      // PDU size field (LE u16)
      10, 0,
      // Channel field (LE u16)
      170, 0,
      // SDU size field (LE u16)
      8, 0,
      // Payload
      'n', 'e', 'x', 't', '_', 's', 'd', 'u',
  };
  // clang-format on

  EXPECT_TRUE(ContainersEqual(*sent_fourth, expected_fourth));
}

TEST_F(CreditBasedFlowControlTxEngineTest, DoesNotAcceptOversizedSdu) {
  StaticByteBuffer<kTestMtu + 1> oversized{};
  ProcessSdu(std::make_unique<DynamicByteBuffer>(oversized));

  EXPECT_EQ(engine().credits(), 1);
  EXPECT_EQ(engine().segments_count(), 0u);
  EXPECT_EQ(channel().queue_size(), 0u);
  ASSERT_EQ(sent_frames().size(), 0u);
}

TEST_F(CreditBasedFlowControlTxEngineTest, AddCreditsOverCap) {
  EXPECT_FALSE(engine().AddCredits(65535));
  EXPECT_EQ(engine().credits(), 1u);
  EXPECT_TRUE(engine().AddCredits(3000));
  EXPECT_EQ(engine().credits(), 3001u);
  EXPECT_TRUE(engine().AddCredits(50000));
  EXPECT_EQ(engine().credits(), 53001u);
  EXPECT_FALSE(engine().AddCredits(12535));
  EXPECT_EQ(engine().credits(), 53001u);
  EXPECT_FALSE(engine().AddCredits(65535));
  EXPECT_EQ(engine().credits(), 53001u);
  EXPECT_TRUE(engine().AddCredits(12534));
  EXPECT_EQ(engine().credits(), 65535u);
  EXPECT_FALSE(engine().AddCredits(1));
  EXPECT_EQ(engine().credits(), 65535u);
  EXPECT_FALSE(engine().AddCredits(42));
  EXPECT_EQ(engine().credits(), 65535u);
  EXPECT_FALSE(engine().AddCredits(99));
  EXPECT_EQ(engine().credits(), 65535u);
  EXPECT_FALSE(engine().AddCredits(32768));
  EXPECT_EQ(engine().credits(), 65535u);
  EXPECT_FALSE(engine().AddCredits(32767));
  EXPECT_EQ(engine().credits(), 65535u);
  EXPECT_FALSE(engine().AddCredits(65535));
  EXPECT_EQ(engine().credits(), 65535u);
}

}  // namespace
}  // namespace bt::l2cap::internal
