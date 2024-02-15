// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/low_energy_interrogator.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/peer_cache.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/fake_l2cap.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/l2cap_defs.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/controller_test.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/mock_controller.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_packets.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/error.h"

namespace bt::gap {

constexpr hci_spec::ConnectionHandle kConnectionHandle = 0x0BAA;
constexpr uint64_t kLEFeaturesHasSca =
    0x0123456789a |
    static_cast<uint64_t>(
        hci_spec::LESupportedFeature::kSleepClockAccuracyUpdates);
constexpr uint64_t kLEFeaturesNoSca =
    0x0123456789a &
    ~static_cast<uint64_t>(
        hci_spec::LESupportedFeature::kSleepClockAccuracyUpdates);
constexpr auto kDefaultScaRange =
    pw::bluetooth::emboss::LESleepClockAccuracyRange::PPM_51_TO_75;
const DeviceAddress kTestDevAddr(DeviceAddress::Type::kLERandom, {1});

const auto kReadRemoteVersionInfoRsp =
    testing::CommandStatusPacket(hci_spec::kReadRemoteVersionInfo,
                                 pw::bluetooth::emboss::StatusCode::SUCCESS);
const auto kLEReadRemoteFeaturesRsp =
    testing::CommandStatusPacket(hci_spec::kLEReadRemoteFeatures,
                                 pw::bluetooth::emboss::StatusCode::SUCCESS);
const auto kLERequestPeerScaRsp = testing::CommandStatusPacket(
    hci_spec::kLERequestPeerSCA, pw::bluetooth::emboss::StatusCode::SUCCESS);

using TestingBase =
    bt::testing::FakeDispatcherControllerTest<bt::testing::MockController>;

class LowEnergyInterrogatorTest : public TestingBase {
 public:
  LowEnergyInterrogatorTest() = default;
  ~LowEnergyInterrogatorTest() override = default;

  void SetUp() override {
    TestingBase::SetUp();

    peer_cache_ = std::make_unique<PeerCache>(dispatcher());

    peer_ = peer_cache()->NewPeer(kTestDevAddr, /*connectable=*/true);
    ASSERT_TRUE(peer_->le());
    EXPECT_FALSE(peer_->version());
    EXPECT_FALSE(peer_->le()->features());
    EXPECT_FALSE(peer_->le()->sleep_clock_accuracy());

    CreateInterrogator(/*supports_sca=*/true);
  }

  void TearDown() override {
    RunUntilIdle();
    test_device()->Stop();
    interrogator_ = nullptr;
    peer_cache_ = nullptr;
    TestingBase::TearDown();
  }

 protected:
  void QueueSuccessfulInterrogation(hci_spec::ConnectionHandle conn,
                                    hci_spec::LESupportedFeatures features = {
                                        0}) const {
    const auto remote_version_complete_packet =
        testing::ReadRemoteVersionInfoCompletePacket(conn);

    EXPECT_CMD_PACKET_OUT(test_device(),
                          testing::ReadRemoteVersionInfoPacket(conn),
                          &kReadRemoteVersionInfoRsp,
                          &remote_version_complete_packet);

    const auto le_remote_features_complete_packet =
        testing::LEReadRemoteFeaturesCompletePacket(conn, features);
    EXPECT_CMD_PACKET_OUT(test_device(),
                          testing::LEReadRemoteFeaturesPacket(conn),
                          &kLEReadRemoteFeaturesRsp,
                          &le_remote_features_complete_packet);

    // Expect a SCA request, if supported by the peer and the controller
    if ((features.le_features &
         static_cast<uint64_t>(
             hci_spec::LESupportedFeature::kSleepClockAccuracyUpdates)) &&
        controller_supports_sca_) {
      const auto le_request_peer_sca_complete_packet =
          testing::LERequestPeerScaCompletePacket(conn, kDefaultScaRange);
      EXPECT_CMD_PACKET_OUT(test_device(),
                            testing::LERequestPeerScaPacket(conn),
                            &kLERequestPeerScaRsp,
                            &le_request_peer_sca_complete_packet);
    }
  }

  void CreateInterrogator(bool supports_sca) {
    controller_supports_sca_ = supports_sca;
    interrogator_ =
        std::make_unique<LowEnergyInterrogator>(peer_->GetWeakPtr(),
                                                kConnectionHandle,
                                                cmd_channel()->AsWeakPtr(),
                                                supports_sca);
  }

  void DestroyInterrogator() { interrogator_.reset(); }

  Peer* peer() const { return peer_; }

  PeerCache* peer_cache() const { return peer_cache_.get(); }

  LowEnergyInterrogator* interrogator() const { return interrogator_.get(); }

 private:
  Peer* peer_ = nullptr;
  std::unique_ptr<PeerCache> peer_cache_;
  std::unique_ptr<LowEnergyInterrogator> interrogator_;
  bool controller_supports_sca_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(LowEnergyInterrogatorTest);
};

using GAP_LowEnergyInterrogatorTest = LowEnergyInterrogatorTest;

TEST_F(LowEnergyInterrogatorTest, SuccessfulInterrogation) {
  // As of Core Spec v5.4, the Feature Set mask has 44 bits (5.5 bytes) in use.
  const hci_spec::LESupportedFeatures kFeatures{kLEFeaturesHasSca};
  QueueSuccessfulInterrogation(kConnectionHandle, kFeatures);

  std::optional<hci::Result<>> status;
  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });
  RunUntilIdle();

  ASSERT_TRUE(status.has_value());
  EXPECT_EQ(fit::ok(), *status);

  EXPECT_TRUE(peer()->version());
  ASSERT_TRUE(peer()->le()->features());
  EXPECT_EQ(kFeatures.le_features, peer()->le()->features()->le_features);
  ASSERT_TRUE(peer()->le()->sleep_clock_accuracy());
  EXPECT_EQ(*(peer()->le()->sleep_clock_accuracy()), kDefaultScaRange);
}

TEST_F(LowEnergyInterrogatorTest,
       SuccessfulInterrogationPeerAlreadyHasLEFeatures) {
  // As of Core Spec v5.4, the Feature Set mask has 44 bits (5.5 bytes) in use.
  const hci_spec::LESupportedFeatures kFeatures{kLEFeaturesHasSca};

  const auto remote_version_complete_packet =
      testing::ReadRemoteVersionInfoCompletePacket(kConnectionHandle);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::ReadRemoteVersionInfoPacket(kConnectionHandle),
                        &kReadRemoteVersionInfoRsp,
                        &remote_version_complete_packet);

  // We should still query peer's SCA
  constexpr auto kScaRange =
      pw::bluetooth::emboss::LESleepClockAccuracyRange::PPM_0_TO_20;
  const auto le_request_peer_sca_complete_packet =
      testing::LERequestPeerScaCompletePacket(kConnectionHandle, kScaRange);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::LERequestPeerScaPacket(kConnectionHandle),
                        &kLERequestPeerScaRsp,
                        &le_request_peer_sca_complete_packet);

  peer()->MutLe().SetFeatures(kFeatures);

  std::optional<hci::Result<>> status;
  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });
  RunUntilIdle();
  ASSERT_TRUE(status.has_value());
  EXPECT_EQ(fit::ok(), *status);
  ASSERT_TRUE(peer()->le()->features());
  EXPECT_EQ(kFeatures.le_features, peer()->le()->features()->le_features);
  ASSERT_TRUE(peer()->le()->sleep_clock_accuracy());
  EXPECT_EQ(*(peer()->le()->sleep_clock_accuracy()), kScaRange);
}

TEST_F(LowEnergyInterrogatorTest, SuccessfulReinterrogation) {
  const hci_spec::LESupportedFeatures kFeatures{kLEFeaturesHasSca};
  QueueSuccessfulInterrogation(kConnectionHandle, kFeatures);

  std::optional<hci::Result<>> status;
  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });
  RunUntilIdle();

  ASSERT_TRUE(status.has_value());
  EXPECT_EQ(fit::ok(), *status);
  status = std::nullopt;

  // Remote version should always be read, even if already known.
  const auto remote_version_complete_packet =
      testing::ReadRemoteVersionInfoCompletePacket(kConnectionHandle);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::ReadRemoteVersionInfoPacket(kConnectionHandle),
                        &kReadRemoteVersionInfoRsp,
                        &remote_version_complete_packet);

  // SCA should be read at each connection event.
  constexpr auto kScaRange =
      pw::bluetooth::emboss::LESleepClockAccuracyRange::PPM_251_TO_500;
  const auto le_request_peer_sca_complete_packet =
      testing::LERequestPeerScaCompletePacket(kConnectionHandle, kScaRange);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::LERequestPeerScaPacket(kConnectionHandle),
                        &kLERequestPeerScaRsp,
                        &le_request_peer_sca_complete_packet);

  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });

  RunUntilIdle();
  ASSERT_TRUE(status.has_value());
  EXPECT_EQ(fit::ok(), *status);
  ASSERT_TRUE(peer()->le()->sleep_clock_accuracy());
  EXPECT_EQ(*(peer()->le()->sleep_clock_accuracy()), kScaRange);
}

TEST_F(LowEnergyInterrogatorTest, LEReadRemoteFeaturesErrorStatus) {
  const auto remote_version_complete_packet =
      testing::ReadRemoteVersionInfoCompletePacket(kConnectionHandle);
  const auto le_read_remote_features_error_status_packet =
      testing::CommandStatusPacket(
          hci_spec::kLEReadRemoteFeatures,
          pw::bluetooth::emboss::StatusCode::UNKNOWN_COMMAND);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::ReadRemoteVersionInfoPacket(kConnectionHandle),
                        &kReadRemoteVersionInfoRsp,
                        &remote_version_complete_packet);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::LEReadRemoteFeaturesPacket(kConnectionHandle),
                        &le_read_remote_features_error_status_packet);

  std::optional<hci::Result<>> status;
  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });
  RunUntilIdle();
  ASSERT_TRUE(status.has_value());
  EXPECT_FALSE(status->is_ok());
  EXPECT_FALSE(peer()->le()->features().has_value());

  // When previous operations fail, we shouldn't try to read SCA.
  EXPECT_FALSE(peer()->le()->sleep_clock_accuracy());
}

TEST_F(LowEnergyInterrogatorTest, ReadRemoteVersionErrorStatus) {
  const auto remote_version_error_status_packet = testing::CommandStatusPacket(
      hci_spec::kReadRemoteVersionInfo,
      pw::bluetooth::emboss::StatusCode::UNKNOWN_COMMAND);
  const auto le_remote_features_complete_packet =
      testing::LEReadRemoteFeaturesCompletePacket(kConnectionHandle,
                                                  /*features=*/{0});
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::ReadRemoteVersionInfoPacket(kConnectionHandle),
                        &remote_version_error_status_packet);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::LEReadRemoteFeaturesPacket(kConnectionHandle),
                        &kLEReadRemoteFeaturesRsp,
                        &le_remote_features_complete_packet);

  std::optional<hci::Result<>> status;
  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });
  RunUntilIdle();
  ASSERT_TRUE(status.has_value());
  EXPECT_FALSE(status->is_ok());
  EXPECT_FALSE(peer()->version());

  // When previous operations fail, we shouldn't try to read SCA.
  EXPECT_FALSE(peer()->le()->sleep_clock_accuracy());
}

TEST_F(LowEnergyInterrogatorTest,
       ReadLERemoteFeaturesCallbackHandlesCanceledInterrogation) {
  const auto remote_version_complete_packet =
      testing::ReadRemoteVersionInfoCompletePacket(kConnectionHandle);
  const auto le_remote_features_complete_packet =
      testing::LEReadRemoteFeaturesCompletePacket(
          kConnectionHandle, hci_spec::LESupportedFeatures{0});

  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::ReadRemoteVersionInfoPacket(kConnectionHandle),
                        &kReadRemoteVersionInfoRsp,
                        &remote_version_complete_packet);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::LEReadRemoteFeaturesPacket(kConnectionHandle),
                        &kLEReadRemoteFeaturesRsp);

  std::optional<hci::Result<>> result;
  interrogator()->Start(
      [&result](hci::Result<> cb_result) { result = cb_result; });
  RunUntilIdle();
  EXPECT_FALSE(result.has_value());

  interrogator()->Cancel();
  RunUntilIdle();
  ASSERT_TRUE(result.has_value());
  EXPECT_TRUE(result->is_error());
  EXPECT_EQ(result.value(), ToResult(HostError::kCanceled));
  result.reset();

  test_device()->SendCommandChannelPacket(le_remote_features_complete_packet);
  RunUntilIdle();
  EXPECT_FALSE(result.has_value());
  // The read remote features handler should not update the features of a
  // canceled interrogation.
  EXPECT_FALSE(peer()->le()->features().has_value());
  EXPECT_FALSE(peer()->le()->sleep_clock_accuracy());
}

TEST_F(LowEnergyInterrogatorTest,
       ReadRemoteVersionCallbackHandlesCanceledInterrogation) {
  const auto remote_version_complete_packet =
      testing::ReadRemoteVersionInfoCompletePacket(kConnectionHandle);
  const auto le_remote_features_complete_packet =
      testing::LEReadRemoteFeaturesCompletePacket(
          kConnectionHandle, hci_spec::LESupportedFeatures{0});

  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::ReadRemoteVersionInfoPacket(kConnectionHandle),
                        &kReadRemoteVersionInfoRsp);
  EXPECT_CMD_PACKET_OUT(test_device(),
                        testing::LEReadRemoteFeaturesPacket(kConnectionHandle),
                        &kLEReadRemoteFeaturesRsp,
                        &le_remote_features_complete_packet);

  std::optional<hci::Result<>> result;
  interrogator()->Start(
      [&result](hci::Result<> cb_result) { result = cb_result; });
  RunUntilIdle();
  EXPECT_FALSE(result.has_value());

  interrogator()->Cancel();
  RunUntilIdle();
  ASSERT_TRUE(result.has_value());
  EXPECT_TRUE(result->is_error());
  EXPECT_EQ(result.value(), ToResult(HostError::kCanceled));
  result.reset();

  test_device()->SendCommandChannelPacket(remote_version_complete_packet);
  RunUntilIdle();
  EXPECT_FALSE(result.has_value());
  // The read remote version handler should not update the version after a
  // canceled interrogation.
  EXPECT_FALSE(peer()->version());
  EXPECT_FALSE(peer()->le()->sleep_clock_accuracy());
}

TEST_F(LowEnergyInterrogatorTest, ScaUpdateNotSupportedOnController) {
  CreateInterrogator(/*supports_sca=*/false);

  const hci_spec::LESupportedFeatures kFeatures{kLEFeaturesHasSca};
  QueueSuccessfulInterrogation(kConnectionHandle, kFeatures);

  std::optional<hci::Result<>> status;
  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });
  RunUntilIdle();

  ASSERT_TRUE(status.has_value());
  EXPECT_EQ(fit::ok(), *status);

  EXPECT_TRUE(peer()->version());
  ASSERT_TRUE(peer()->le()->features());
  EXPECT_EQ(kFeatures.le_features, peer()->le()->features()->le_features);
  ASSERT_FALSE(peer()->le()->sleep_clock_accuracy());
}

TEST_F(LowEnergyInterrogatorTest, ScaUpdateNotSupportedOnPeer) {
  // Disable peer support for SCA updates.
  const hci_spec::LESupportedFeatures kFeatures{kLEFeaturesNoSca};
  QueueSuccessfulInterrogation(kConnectionHandle, kFeatures);

  std::optional<hci::Result<>> status;
  interrogator()->Start(
      [&status](hci::Result<> cb_status) { status = cb_status; });
  RunUntilIdle();

  ASSERT_TRUE(status.has_value());
  EXPECT_EQ(fit::ok(), *status);

  EXPECT_TRUE(peer()->version());
  ASSERT_TRUE(peer()->le()->features());
  EXPECT_EQ(kFeatures.le_features, peer()->le()->features()->le_features);
  ASSERT_FALSE(peer()->le()->sleep_clock_accuracy());
}

TEST_F(LowEnergyInterrogatorTest, DestroyInterrogatorInCompleteCallback) {
  // As of Core Spec v5.4, the Feature Set mask has 44 bits (5.5 bytes) in use.
  const hci_spec::LESupportedFeatures kFeatures{kLEFeaturesHasSca};
  QueueSuccessfulInterrogation(kConnectionHandle, kFeatures);

  std::optional<hci::Result<>> status;
  interrogator()->Start([this, &status](hci::Result<> cb_status) {
    status = cb_status;
    DestroyInterrogator();
  });
  RunUntilIdle();
  ASSERT_TRUE(status.has_value());
  EXPECT_TRUE(status->is_ok());
  ASSERT_TRUE(peer()->le()->features());
  EXPECT_EQ(kFeatures.le_features, peer()->le()->features()->le_features);
  ASSERT_TRUE(peer()->le()->sleep_clock_accuracy());
  EXPECT_EQ(*(peer()->le()->sleep_clock_accuracy()), kDefaultScaRange);
}

}  // namespace bt::gap
