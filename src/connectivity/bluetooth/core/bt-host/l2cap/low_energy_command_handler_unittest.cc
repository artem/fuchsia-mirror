// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/l2cap/low_energy_command_handler.h"

#include <pw_async/fake_dispatcher_fixture.h>
#include <pw_async_fuchsia/dispatcher.h>

#include "src/connectivity/bluetooth/core/bt-host/l2cap/fake_signaling_channel.h"
#include "src/connectivity/bluetooth/core/bt-host/testing/test_helpers.h"

namespace bt::l2cap::internal {

class LowEnergyCommandHandlerTest : public pw::async::test::FakeDispatcherFixture {
 public:
  LowEnergyCommandHandlerTest() = default;
  ~LowEnergyCommandHandlerTest() override = default;
  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(LowEnergyCommandHandlerTest);

 protected:
  // TestLoopFixture overrides
  void SetUp() override {
    signaling_channel_ = std::make_unique<testing::FakeSignalingChannel>(dispatcher());
    command_handler_ = std::make_unique<LowEnergyCommandHandler>(
        fake_sig(), fit::bind_member<&LowEnergyCommandHandlerTest::OnRequestFail>(this));
    request_fail_callback_ = nullptr;
    failed_requests_ = 0;
  }

  void TearDown() override {
    request_fail_callback_ = nullptr;
    signaling_channel_ = nullptr;
    command_handler_ = nullptr;
  }

  testing::FakeSignalingChannel* fake_sig() const { return signaling_channel_.get(); }
  LowEnergyCommandHandler* cmd_handler() const { return command_handler_.get(); }
  size_t failed_requests() const { return failed_requests_; }

  void set_request_fail_callback(fit::closure request_fail_callback) {
    BT_ASSERT(!request_fail_callback_);
    request_fail_callback_ = std::move(request_fail_callback);
  }

 private:
  void OnRequestFail() {
    failed_requests_++;
    if (request_fail_callback_) {
      request_fail_callback_();
    }
  }

  std::unique_ptr<testing::FakeSignalingChannel> signaling_channel_;
  std::unique_ptr<LowEnergyCommandHandler> command_handler_;
  fit::closure request_fail_callback_;
  size_t failed_requests_;
};

TEST_F(LowEnergyCommandHandlerTest, OutboundConnParamUpdateReqRspOk) {
  constexpr uint16_t kIntervalMin = 0;
  constexpr uint16_t kIntervalMax = 1;
  constexpr uint16_t kPeripheralLatency = 2;
  constexpr uint16_t kTimeoutMult = 3;
  StaticByteBuffer param_update_req(
      // Interval Min
      LowerBits(kIntervalMin), UpperBits(kIntervalMin),
      // Interval Max
      LowerBits(kIntervalMax), UpperBits(kIntervalMax),
      // Peripheral Latency
      LowerBits(kPeripheralLatency), UpperBits(kPeripheralLatency),
      // Timeout Multiplier
      LowerBits(kTimeoutMult), UpperBits(kTimeoutMult));

  StaticByteBuffer param_update_rsp(
      LowerBits(static_cast<uint16_t>(ConnectionParameterUpdateResult::kRejected)),
      UpperBits(static_cast<uint16_t>(ConnectionParameterUpdateResult::kRejected)));

  bool cb_called = false;
  LowEnergyCommandHandler::ConnectionParameterUpdateResponseCallback rsp_cb =
      [&](const LowEnergyCommandHandler::ConnectionParameterUpdateResponse& rsp) {
        cb_called = true;
        EXPECT_EQ(SignalingChannel::Status::kSuccess, rsp.status());
        EXPECT_EQ(ConnectionParameterUpdateResult::kRejected, rsp.result());
      };

  EXPECT_OUTBOUND_REQ(*fake_sig(), kConnectionParameterUpdateRequest, param_update_req.view(),
                      {SignalingChannel::Status::kSuccess, param_update_rsp.view()});

  EXPECT_TRUE(cmd_handler()->SendConnectionParameterUpdateRequest(
      kIntervalMin, kIntervalMax, kPeripheralLatency, kTimeoutMult, std::move(rsp_cb)));
  RunUntilIdle();
  EXPECT_TRUE(cb_called);
}

TEST_F(LowEnergyCommandHandlerTest, InboundConnParamsUpdateReqRspOk) {
  constexpr uint16_t kIntervalMin = 0;
  constexpr uint16_t kIntervalMax = 1;
  constexpr uint16_t kPeripheralLatency = 2;
  constexpr uint16_t kTimeoutMult = 3;
  StaticByteBuffer param_update_req(
      // Interval Min
      LowerBits(kIntervalMin), UpperBits(kIntervalMin),
      // Interval Max
      LowerBits(kIntervalMax), UpperBits(kIntervalMax),
      // Peripheral Latency
      LowerBits(kPeripheralLatency), UpperBits(kPeripheralLatency),
      // Timeout Multiplier
      LowerBits(kTimeoutMult), UpperBits(kTimeoutMult));

  auto param_update_rsp = StaticByteBuffer(
      LowerBits(static_cast<uint16_t>(ConnectionParameterUpdateResult::kRejected)),
      UpperBits(static_cast<uint16_t>(ConnectionParameterUpdateResult::kRejected)));

  LowEnergyCommandHandler::ConnectionParameterUpdateRequestCallback cb =
      [&](uint16_t interval_min, uint16_t interval_max, uint16_t peripheral_latency,
          uint16_t timeout_multiplier,
          LowEnergyCommandHandler::ConnectionParameterUpdateResponder* responder) {
        EXPECT_EQ(kIntervalMin, interval_min);
        EXPECT_EQ(kIntervalMax, interval_max);
        EXPECT_EQ(kPeripheralLatency, peripheral_latency);
        EXPECT_EQ(kTimeoutMult, timeout_multiplier);
        responder->Send(ConnectionParameterUpdateResult::kRejected);
      };

  cmd_handler()->ServeConnectionParameterUpdateRequest(std::move(cb));

  RETURN_IF_FATAL(fake_sig()->ReceiveExpect(kConnectionParameterUpdateRequest, param_update_req,
                                            param_update_rsp));
}

TEST_F(LowEnergyCommandHandlerTest, InboundConnParamsUpdateReqNotEnoughBytes) {
  constexpr uint16_t kIntervalMin = 0;

  // Request is missing interval max, peripheral latency, and timeout multiplier fields.
  auto param_update_req = StaticByteBuffer(
      // Interval Min
      LowerBits(kIntervalMin), UpperBits(kIntervalMin));

  bool cb_called = false;
  auto cb = [&](uint16_t interval_min, uint16_t interval_max, uint16_t peripheral_latency,
                uint16_t timeout_multiplier, auto responder) { cb_called = true; };

  cmd_handler()->ServeConnectionParameterUpdateRequest(std::move(cb));

  RETURN_IF_FATAL(fake_sig()->ReceiveExpectRejectNotUnderstood(kConnectionParameterUpdateRequest,
                                                               param_update_req));
  EXPECT_FALSE(cb_called);
}

}  // namespace bt::l2cap::internal
