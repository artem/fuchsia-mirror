// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/basic_mode_tx_engine.h"

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"

namespace bt::l2cap::internal {
namespace {

constexpr ChannelId kTestChannelId = 0x0001;

TEST(BasicModeTxEngineTest, QueueSduTransmitsMinimalSizedSdu) {
  ByteBufferPtr last_pdu;
  size_t n_pdus = 0;
  auto tx_callback = [&](auto pdu) {
    ++n_pdus;
    last_pdu = std::move(pdu);
  };

  constexpr size_t kMtu = 10;
  const StaticByteBuffer payload(1);
  BasicModeTxEngine(kTestChannelId, kMtu, tx_callback)
      .QueueSdu(std::make_unique<DynamicByteBuffer>(payload));
  EXPECT_EQ(1u, n_pdus);
  ASSERT_TRUE(last_pdu);
  EXPECT_TRUE(ContainersEqual(payload, *last_pdu));
}

TEST(BasicModeTxEngineTest, QueueSduTransmitsMaximalSizedSdu) {
  ByteBufferPtr last_pdu;
  size_t n_pdus = 0;
  auto tx_callback = [&](auto pdu) {
    ++n_pdus;
    last_pdu = std::move(pdu);
  };

  constexpr size_t kMtu = 1;
  const StaticByteBuffer payload(1);
  BasicModeTxEngine(kTestChannelId, kMtu, tx_callback)
      .QueueSdu(std::make_unique<DynamicByteBuffer>(payload));
  EXPECT_EQ(1u, n_pdus);
  ASSERT_TRUE(last_pdu);
  EXPECT_TRUE(ContainersEqual(payload, *last_pdu));
}

TEST(BasicModeTxEngineTest, QueueSduDropsOversizedSdu) {
  size_t n_pdus = 0;
  auto tx_callback = [&](auto pdu) { ++n_pdus; };

  constexpr size_t kMtu = 1;
  BasicModeTxEngine(kTestChannelId, kMtu, tx_callback)
      .QueueSdu(std::make_unique<DynamicByteBuffer>(StaticByteBuffer(1, 2)));
  EXPECT_EQ(0u, n_pdus);
}

TEST(BasicModeTxEngineTest, QueueSduSurvivesZeroByteSdu) {
  constexpr size_t kMtu = 1;
  BasicModeTxEngine(kTestChannelId, kMtu, [](auto pdu) {
  }).QueueSdu(std::make_unique<DynamicByteBuffer>());
}

}  // namespace
}  // namespace bt::l2cap::internal
