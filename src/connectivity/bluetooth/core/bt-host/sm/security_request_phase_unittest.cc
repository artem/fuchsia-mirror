// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/security_request_phase.h"

#include <memory>

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/macros.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/connection.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/fake_channel_test.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/fake_phase_listener.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/packet.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/smp.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/types.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/util.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"

namespace bt::sm {
namespace {
struct SecurityRequestOptions {
  SecurityLevel requested_level = SecurityLevel::kEncrypted;
  BondableMode bondable = BondableMode::Bondable;
};

class SecurityRequestPhaseTest : public l2cap::testing::FakeChannelTest {
 public:
  SecurityRequestPhaseTest() = default;
  ~SecurityRequestPhaseTest() override = default;

 protected:
  void SetUp() override { NewSecurityRequestPhase(); }

  void TearDown() override { security_request_phase_ = nullptr; }

  pw::async::HeapDispatcher& heap_dispatcher() { return heap_dispatcher_; }

  void NewSecurityRequestPhase(
      SecurityRequestOptions opts = SecurityRequestOptions(),
      bt::LinkType ll_type = bt::LinkType::kLE) {
    l2cap::ChannelId cid = ll_type == bt::LinkType::kLE ? l2cap::kLESMPChannelId
                                                        : l2cap::kSMPChannelId;
    ChannelOptions options(cid);
    options.link_type = ll_type;

    fake_chan_ = CreateFakeChannel(options);
    sm_chan_ = std::make_unique<PairingChannel>(fake_chan_->GetWeakPtr());
    fake_listener_ = std::make_unique<FakeListener>();
    security_request_phase_ = std::make_unique<SecurityRequestPhase>(
        sm_chan_->GetWeakPtr(),
        fake_listener_->as_weak_ptr(),
        opts.requested_level,
        opts.bondable,
        [this](PairingRequestParams preq) { last_pairing_req_ = preq; });
  }

  l2cap::testing::FakeChannel* fake_chan() const { return fake_chan_.get(); }
  SecurityRequestPhase* security_request_phase() {
    return security_request_phase_.get();
  }

  std::optional<PairingRequestParams> last_pairing_req() {
    return last_pairing_req_;
  }

 private:
  std::unique_ptr<l2cap::testing::FakeChannel> fake_chan_;
  std::unique_ptr<PairingChannel> sm_chan_;
  std::unique_ptr<FakeListener> fake_listener_;
  std::unique_ptr<SecurityRequestPhase> security_request_phase_;

  std::optional<PairingRequestParams> last_pairing_req_;

  pw::async::HeapDispatcher heap_dispatcher_{dispatcher()};

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(SecurityRequestPhaseTest);
};

using SMP_SecurityRequestPhaseDeathTest = SecurityRequestPhaseTest;

TEST_F(SecurityRequestPhaseTest, MakeEncryptedBondableSecurityRequest) {
  NewSecurityRequestPhase(
      SecurityRequestOptions{.requested_level = SecurityLevel::kEncrypted,
                             .bondable = BondableMode::Bondable});
  StaticByteBuffer kExpectedReq(kSecurityRequest, AuthReq::kBondingFlag);
  (void)heap_dispatcher().Post(
      [this](pw::async::Context /*ctx*/, pw::Status status) {
        if (status.ok()) {
          security_request_phase()->Start();
        }
      });
  ASSERT_TRUE(Expect(kExpectedReq));
  EXPECT_EQ(SecurityLevel::kEncrypted,
            security_request_phase()->pending_security_request());
}

TEST_F(SecurityRequestPhaseTest, MakeAuthenticatedNonBondableSecurityRequest) {
  NewSecurityRequestPhase(
      SecurityRequestOptions{.requested_level = SecurityLevel::kAuthenticated,
                             .bondable = BondableMode::NonBondable});
  StaticByteBuffer kExpectedReq(kSecurityRequest, AuthReq::kMITM);
  (void)heap_dispatcher().Post(
      [this](pw::async::Context /*ctx*/, pw::Status status) {
        if (status.ok()) {
          security_request_phase()->Start();
        }
      });
  ASSERT_TRUE(Expect(kExpectedReq));
  EXPECT_EQ(SecurityLevel::kAuthenticated,
            security_request_phase()->pending_security_request());
}

TEST_F(SecurityRequestPhaseTest,
       MakeSecureAuthenticatedBondableSecurityRequest) {
  NewSecurityRequestPhase(SecurityRequestOptions{
      .requested_level = SecurityLevel::kSecureAuthenticated});
  StaticByteBuffer kExpectedReq(
      kSecurityRequest, AuthReq::kBondingFlag | AuthReq::kMITM | AuthReq::kSC);

  (void)heap_dispatcher().Post(
      [this](pw::async::Context /*ctx*/, pw::Status status) {
        if (status.ok()) {
          security_request_phase()->Start();
        }
      });
  ASSERT_TRUE(Expect(kExpectedReq));
  EXPECT_EQ(SecurityLevel::kSecureAuthenticated,
            security_request_phase()->pending_security_request());
}

TEST_F(SecurityRequestPhaseTest, HandlesChannelClosedGracefully) {
  fake_chan()->Close();
  RunUntilIdle();
}

TEST_F(SecurityRequestPhaseTest, PairingRequestAsResponderPassedThrough) {
  StaticByteBuffer<util::PacketSize<PairingRequestParams>()> preq_packet;
  PacketWriter writer(kPairingRequest, &preq_packet);
  PairingRequestParams generic_preq{.io_capability = IOCapability::kDisplayOnly,
                                    .oob_data_flag = OOBDataFlag::kNotPresent,
                                    .auth_req = AuthReq::kBondingFlag,
                                    .max_encryption_key_size = 0,
                                    .initiator_key_dist_gen = 0,
                                    .responder_key_dist_gen = 0};
  *writer.mutable_payload<PairingRequestParams>() = generic_preq;
  ASSERT_FALSE(last_pairing_req().has_value());
  fake_chan()->Receive(preq_packet);
  RunUntilIdle();
  ASSERT_TRUE(last_pairing_req().has_value());
  PairingRequestParams last_preq = last_pairing_req().value();
  ASSERT_EQ(0, memcmp(&last_preq, &generic_preq, sizeof(PairingRequestParams)));
}

TEST_F(SecurityRequestPhaseTest, InboundSecurityRequestFails) {
  StaticByteBuffer<util::PacketSize<PairingResponseParams>()> pres_packet;
  PacketWriter writer(kPairingResponse, &pres_packet);
  *writer.mutable_payload<PairingResponseParams>() = PairingResponseParams();

  bool message_sent = false;
  fake_chan()->SetSendCallback(
      [&message_sent](ByteBufferPtr sdu) {
        ValidPacketReader reader = ValidPacketReader::ParseSdu(sdu).value();
        ASSERT_EQ(reader.code(), kPairingFailed);
        message_sent = true;
      },
      dispatcher());

  fake_chan()->Receive(pres_packet);
  RunUntilIdle();
  ASSERT_FALSE(last_pairing_req().has_value());
  ASSERT_TRUE(message_sent);
}

TEST_F(SecurityRequestPhaseTest, DropsInvalidPacket) {
  StaticByteBuffer bad_packet(0xFF);  // 0xFF is not a valid SMP header code

  bool message_sent = false;
  fake_chan()->SetSendCallback(
      [&message_sent](ByteBufferPtr sdu) {
        ValidPacketReader reader = ValidPacketReader::ParseSdu(sdu).value();
        ASSERT_EQ(reader.code(), kPairingFailed);
        message_sent = true;
      },
      dispatcher());

  fake_chan()->Receive(bad_packet);
  RunUntilIdle();
  ASSERT_FALSE(last_pairing_req().has_value());
  ASSERT_TRUE(message_sent);
}

}  // namespace
}  // namespace bt::sm
