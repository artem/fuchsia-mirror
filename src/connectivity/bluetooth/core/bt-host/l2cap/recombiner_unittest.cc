// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/recombiner.h"

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/pdu.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/packet.h"

namespace bt::l2cap {
namespace {

constexpr hci_spec::ConnectionHandle kTestHandle = 0x0001;
constexpr ChannelId kTestChannelId = 0xFFFF;

template <typename... T>
hci::ACLDataPacketPtr PacketFromBytes(T... data) {
  StaticByteBuffer bytes(std::forward<T>(data)...);
  BT_DEBUG_ASSERT(bytes.size() >= sizeof(hci_spec::ACLDataHeader));

  auto packet =
      hci::ACLDataPacket::New(bytes.size() - sizeof(hci_spec::ACLDataHeader));
  packet->mutable_view()->mutable_data().Write(bytes);
  packet->InitializeFromBuffer();

  return packet;
}

hci::ACLDataPacketPtr FirstFragment(
    std::string payload,
    std::optional<uint16_t> payload_size = std::nullopt,
    hci_spec::ACLPacketBoundaryFlag pbf =
        hci_spec::ACLPacketBoundaryFlag::kFirstFlushable) {
  uint16_t header_payload_size =
      payload_size.has_value() ? *payload_size : payload.size();
  auto packet =
      hci::ACLDataPacket::New(kTestHandle,
                              pbf,
                              hci_spec::ACLBroadcastFlag::kPointToPoint,
                              sizeof(BasicHeader) + payload.size());

  // L2CAP Header
  auto* header = packet->mutable_view()->mutable_payload<BasicHeader>();
  header->length = htole16(header_payload_size);
  header->channel_id = htole16(kTestChannelId);

  // L2CAP payload
  packet->mutable_view()->mutable_payload_data().Write(BufferView(payload),
                                                       sizeof(BasicHeader));
  return packet;
}

hci::ACLDataPacketPtr ContinuingFragment(std::string payload) {
  auto packet = hci::ACLDataPacket::New(
      kTestHandle,
      hci_spec::ACLPacketBoundaryFlag::kContinuingFragment,
      hci_spec::ACLBroadcastFlag::kPointToPoint,
      payload.size());
  packet->mutable_view()->mutable_payload_data().Write(BufferView(payload));
  return packet;
}

hci::ACLDataPacketPtr FirstFragmentWithShortL2capHeader() {
  return PacketFromBytes(
      // ACL data header (handle: 0x0001)
      0x01,
      0x00,
      0x03,
      0x00,

      // Incomplete basic L2CAP header (one byte short)
      0x00,
      0x00,
      0x03);
}

hci::ACLDataPacketPtr FirstFragmentWithTooLargePayload() {
  // Payload length (4 bytes) is larger than reported length (3 bytes).
  return FirstFragment("hello", {3});
}

void ValidatePdu(PDU pdu,
                 std::string expected_payload,
                 ChannelId expected_cid = kTestChannelId) {
  EXPECT_TRUE(pdu.is_valid());
  EXPECT_EQ(expected_payload.size(), pdu.length());
  EXPECT_EQ(expected_cid, pdu.channel_id());

  // Test that the contents of the PDU match the expected payload.
  auto sdu = std::make_unique<DynamicByteBuffer>(pdu.length());
  pdu.Copy(sdu.get());
  EXPECT_EQ(sdu->AsString(), expected_payload);

  // Validate that all individual fragments perfectly sum up to the expected
  // size.
  auto fragments = pdu.ReleaseFragments();
  size_t sum = 0;
  for (const auto& f : fragments) {
    sum += f->view().payload_size();
  }
  EXPECT_EQ(expected_payload.length() + sizeof(BasicHeader), sum);
}

#define VALIDATE_PDU(...)     \
  do {                        \
    SCOPED_TRACE("");         \
    ValidatePdu(__VA_ARGS__); \
  } while (false)

// The following test exercises a BT_DEBUG_ASSERT and thus only works in DEBUG
// builds.
#ifdef DEBUG
TEST(RecombinerTest, WrongHandle) {
  Recombiner recombiner(kTestHandle);
  auto packet = PacketFromBytes(0x02,
                                0x00,  // handle: 0x0002
                                0x00,
                                0x00  // length: 0
  );
  ASSERT_DEATH_IF_SUPPORTED(recombiner.ConsumeFragment(std::move(packet)),
                            ".*connection_handle.*");
}
#endif  // DEBUG

TEST(RecombinerTest, FirstFragmentTooShort) {
  Recombiner recombiner(kTestHandle);
  auto result = recombiner.ConsumeFragment(FirstFragmentWithShortL2capHeader());
  EXPECT_FALSE(result.pdu);
  EXPECT_TRUE(result.frames_dropped);
}

TEST(RecombinerTest, FirstFragmentTooLong) {
  Recombiner recombiner(kTestHandle);
  auto result = recombiner.ConsumeFragment(FirstFragmentWithTooLargePayload());
  EXPECT_FALSE(result.pdu);
  EXPECT_TRUE(result.frames_dropped);
}

TEST(RecombinerTest, ContinuingFragmentWhenNotRecombining) {
  Recombiner recombiner(kTestHandle);
  auto result = recombiner.ConsumeFragment(ContinuingFragment(""));
  EXPECT_FALSE(result.pdu);
  EXPECT_TRUE(result.frames_dropped);
}

TEST(RecombinerTest, CompleteEmptyFirstFragment) {
  Recombiner recombiner(kTestHandle);
  auto result = recombiner.ConsumeFragment(FirstFragment(""));
  EXPECT_FALSE(result.frames_dropped);
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "");
}

TEST(RecombinerTest, CompleteNonEmptyFirstFragment) {
  Recombiner recombiner(kTestHandle);
  auto result = recombiner.ConsumeFragment(FirstFragment("Test"));
  EXPECT_FALSE(result.frames_dropped);
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "Test");
}

TEST(RecombinerTest, TwoPartRecombination) {
  Recombiner recombiner(kTestHandle);
  auto result = recombiner.ConsumeFragment(FirstFragment("der", {4}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);

  result = recombiner.ConsumeFragment(ContinuingFragment("p"));
  EXPECT_FALSE(result.frames_dropped);
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "derp");
}

TEST(RecombinerTest, ThreePartRecombination) {
  Recombiner recombiner(kTestHandle);
  auto result = recombiner.ConsumeFragment(FirstFragment("d", {4}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);

  result = recombiner.ConsumeFragment(ContinuingFragment("er"));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);

  result = recombiner.ConsumeFragment(ContinuingFragment("p"));
  EXPECT_FALSE(result.frames_dropped);
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "derp");
}

TEST(RecombinerTest, RecombinationDroppedDueToCompleteFirstPacket) {
  Recombiner recombiner(kTestHandle);

  // Write a partial first fragment that initiates a recombination (complete
  // frame length is 2 but payload contains 1 byte).
  auto result = recombiner.ConsumeFragment(FirstFragment("a", {2}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);  // No complete PDU yet.

  // Write a new complete first fragment. The previous (still recombining) frame
  // should get dropped and the new frame should get delivered. This should
  // report an error for the dropped PDU even though it also returns a valid
  // PDU.
  result = recombiner.ConsumeFragment(FirstFragment("derp"));
  EXPECT_TRUE(result.frames_dropped);

  // We should have a complete PDU that doesn't contain the dropped segment
  // ("a").
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "derp");
}

TEST(RecombinerTest, RecombinationDroppedDueToPartialFirstPacket) {
  Recombiner recombiner(kTestHandle);

  // Write a partial first fragment that initiates a recombination (complete
  // frame length is 2 but payload contains 1 byte).
  auto result = recombiner.ConsumeFragment(FirstFragment("a", {2}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);  // No complete PDU yet.

  // Write a new partial first fragment. The previous (still recombining) frame
  // should get dropped and the new frame should be buffered for recombination.
  result = recombiner.ConsumeFragment(FirstFragment("de", {4}));
  EXPECT_TRUE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);  // No complete PDU yet.

  // Complete the new sequence. This should not contain the dropped segment
  // ("a")
  result = recombiner.ConsumeFragment(ContinuingFragment("rp"));
  EXPECT_FALSE(result.frames_dropped);
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "derp");
}

TEST(RecombinerTest, RecombinationDroppedDueToMalformedFirstPacket) {
  Recombiner recombiner(kTestHandle);

  // Write a partial first fragment that initiates a recombination (complete
  // frame length is 2 but payload contains 1 byte).
  auto result = recombiner.ConsumeFragment(FirstFragment("a", {2}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);  // No complete PDU yet.

  // Write a new partial first fragment. The previous (still recombining) frame
  // should get dropped. The new fragment should also get dropped since it's
  // malformed.
  result = recombiner.ConsumeFragment(FirstFragmentWithShortL2capHeader());
  EXPECT_TRUE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);  // No complete PDU yet.

  // Complete a new sequence. This should not contain the dropped segments.
  result = recombiner.ConsumeFragment(FirstFragment("derp"));
  EXPECT_FALSE(result.frames_dropped);
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "derp");
}

TEST(RecombinerTest, RecombinationDroppedDueToTooLargeContinuingFrame) {
  Recombiner recombiner(kTestHandle);

  // Write a partial first fragment that initiates a recombination (complete
  // frame length is 2 but payload contains 1 byte).
  auto result = recombiner.ConsumeFragment(FirstFragment("a", {2}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);  // No complete PDU yet.

  // Write a continuing fragment that makes the complete frame larger than 2
  // bytes. The previous (still recombining) frame should get dropped alongside
  // the new fragment.
  result = recombiner.ConsumeFragment(ContinuingFragment("bc"));
  EXPECT_TRUE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);  // No complete PDU.

  // The next frame should not include the two dropped fragments.
  result = recombiner.ConsumeFragment(FirstFragment("derp"));
  EXPECT_FALSE(result.frames_dropped);
  ASSERT_TRUE(result.pdu);
  VALIDATE_PDU(std::move(*result.pdu), "derp");
}

TEST(RecombinerTest, RecombinationDroppedForFrameWithMaxSize) {
  constexpr size_t kFrameSize = std::numeric_limits<uint16_t>::max();
  constexpr size_t kRxSize = kFrameSize + 1;

  Recombiner recombiner(kTestHandle);
  const auto result =
      recombiner.ConsumeFragment(FirstFragment("", {kFrameSize}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);

  // Split the rest of the frame into multiple fragments (this is because
  // Fuchsia's bt-hci layer currently requires ACL data payloads to be no larger
  // than 1024 bytes).
  //
  // Loop until the frame is 1 byte larger than expected.
  bool completed = false;
  for (size_t acc = 0; acc < kRxSize;) {
    const size_t remainder = kRxSize - acc;
    const size_t size = std::min(hci_spec::kMaxACLPayloadSize, remainder);
    acc += size;

    const auto result =
        recombiner.ConsumeFragment(ContinuingFragment(std::string(size, 'd')));
    if (acc == kRxSize) {
      completed = true;
      EXPECT_TRUE(result.frames_dropped) << "last fragment should get dropped!";
      EXPECT_FALSE(result.pdu);
    } else {
      EXPECT_FALSE(result.frames_dropped);
      EXPECT_FALSE(result.pdu);
    }
  }
  EXPECT_TRUE(completed);
}

TEST(RecombinerTest, RecombinationSucceedsForFrameWithMaxSize) {
  constexpr size_t kFrameSize = std::numeric_limits<uint16_t>::max();

  Recombiner recombiner(kTestHandle);
  const auto result =
      recombiner.ConsumeFragment(FirstFragment("", {kFrameSize}));
  EXPECT_FALSE(result.frames_dropped);
  EXPECT_FALSE(result.pdu);

  // Split the rest of the frame into multiple fragments (this is because
  // Fuchsia's bt-hci layer currently requires ACL data payloads to be no larger
  // than 1024 bytes).
  //
  // Loop until the frame is 1 byte larger than expected.
  bool completed = false;
  for (size_t acc = 0; acc < kFrameSize;) {
    const size_t remainder = kFrameSize - acc;
    const size_t size = std::min(hci_spec::kMaxACLPayloadSize, remainder);
    acc += size;

    auto result =
        recombiner.ConsumeFragment(ContinuingFragment(std::string(size, 'd')));
    if (acc == kFrameSize) {
      completed = true;
      EXPECT_FALSE(result.frames_dropped)
          << "last fragment should not cause a drop!";
      ASSERT_TRUE(result.pdu) << "last fragment should result in PDU!";
      VALIDATE_PDU(std::move(*result.pdu), std::string(kFrameSize, 'd'));
    } else {
      EXPECT_FALSE(result.frames_dropped);
      EXPECT_FALSE(result.pdu);
    }
  }
  EXPECT_TRUE(completed);
}

}  // namespace
}  // namespace bt::l2cap
