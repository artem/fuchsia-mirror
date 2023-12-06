// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.cpu.ctrl/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fidl/cpp/wire/channel.h>

#include <zxtest/zxtest.h>

#include "performance-domain.h"

namespace {

constexpr cpuctrl::wire::CpuPerformanceStateInfo kTestPstates[] = {
    {.frequency_hz = 1000, .voltage_uv = 100}, {.frequency_hz = 800, .voltage_uv = 90},
    {.frequency_hz = 600, .voltage_uv = 80},   {.frequency_hz = 400, .voltage_uv = 70},
    {.frequency_hz = 200, .voltage_uv = 60},
};

constexpr uint32_t kInitialPstate = 0;

constexpr uint32_t kNumLogicalCores = 4;

constexpr uint64_t kLogicalCoreIds[kNumLogicalCores] = {1, 2, 3, 4};

class FakeCpuDevice : public fidl::testing::WireTestBase<cpuctrl::Device> {
 public:
  unsigned int PstateSetCount() const { return pstate_set_count_; }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ADD_FAILURE("unexpected call to %s", name.c_str());
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  void SetPerformanceState(SetPerformanceStateRequestView request,
                           SetPerformanceStateCompleter::Sync& completer) override;
  void GetCurrentPerformanceState(GetCurrentPerformanceStateCompleter::Sync& completer) override;
  void GetPerformanceStateInfo(GetPerformanceStateInfoRequestView request,
                               GetPerformanceStateInfoCompleter::Sync& completer) override;
  void GetNumLogicalCores(GetNumLogicalCoresCompleter::Sync& completer) override;
  void GetLogicalCoreId(GetLogicalCoreIdRequestView request,
                        GetLogicalCoreIdCompleter::Sync& completer) override;

  uint32_t current_pstate_ = kInitialPstate;
  unsigned int pstate_set_count_ = 0;
};

void FakeCpuDevice::GetPerformanceStateInfo(GetPerformanceStateInfoRequestView request,
                                            GetPerformanceStateInfoCompleter::Sync& completer) {
  if (request->state >= std::size(kTestPstates)) {
    completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
  } else {
    completer.ReplySuccess(kTestPstates[request->state]);
  }
}

void FakeCpuDevice::GetNumLogicalCores(GetNumLogicalCoresCompleter::Sync& completer) {
  completer.Reply(kNumLogicalCores);
}

void FakeCpuDevice::GetLogicalCoreId(GetLogicalCoreIdRequestView request,
                                     GetLogicalCoreIdCompleter::Sync& completer) {
  if (request->index >= std::size(kLogicalCoreIds)) {
    completer.Reply(UINT64_MAX);
  }
  completer.Reply(kLogicalCoreIds[request->index]);
}

void FakeCpuDevice::SetPerformanceState(SetPerformanceStateRequestView request,
                                        SetPerformanceStateCompleter::Sync& completer) {
  if (request->requested_state > std::size(kTestPstates)) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  pstate_set_count_++;
  current_pstate_ = request->requested_state;
  completer.ReplySuccess(request->requested_state);
}

void FakeCpuDevice::GetCurrentPerformanceState(
    GetCurrentPerformanceStateCompleter::Sync& completer) {
  completer.Reply(current_pstate_);
}

class TestCpuPerformanceDomain : public CpuPerformanceDomain {
 public:
  // Permit Explicit Construction
  TestCpuPerformanceDomain(fidl::ClientEnd<cpuctrl::Device> cpu_client)
      : CpuPerformanceDomain(std::move(cpu_client)) {}
};

class PerformanceDomainTest : public zxtest::Test {
 public:
  void SetUp() override;

  FakeCpuDevice& cpu() { return cpu_; }
  TestCpuPerformanceDomain& pd() { return pd_.value(); }

 private:
  FakeCpuDevice cpu_;
  std::optional<TestCpuPerformanceDomain> pd_;
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

void PerformanceDomainTest::SetUp() {
  zx::result cpu_endpoints = fidl::CreateEndpoints<fuchsia_hardware_cpu_ctrl::Device>();
  ASSERT_OK(cpu_endpoints);
  fidl::BindServer(loop_.dispatcher(), std::move(cpu_endpoints->server),
                   static_cast<fidl::WireServer<cpuctrl::Device>*>(&cpu_));

  pd_.emplace(std::move(cpu_endpoints->client));
  ASSERT_OK(loop_.StartThread("performance-domain-test-fidl-thread"));
}

// Trivial Tests.
TEST_F(PerformanceDomainTest, TestNumLogicalCores) {
  const auto [core_count_status, core_count] = pd().GetNumLogicalCores();

  EXPECT_OK(core_count_status);
  EXPECT_EQ(core_count, kNumLogicalCores);
}

TEST_F(PerformanceDomainTest, TestGetCurrentPerformanceState) {
  const auto [st, pstate, pstate_info] = pd().GetCurrentPerformanceState();
  EXPECT_OK(st);
  EXPECT_EQ(pstate, kInitialPstate);
  EXPECT_EQ(pstate_info.frequency_hz, kTestPstates[kInitialPstate].frequency_hz);
  EXPECT_EQ(pstate_info.voltage_uv, kTestPstates[kInitialPstate].voltage_uv);
}

TEST_F(PerformanceDomainTest, TestGetPerformanceStates) {
  const auto pstates = pd().GetPerformanceStates();

  ASSERT_EQ(pstates.size(), std::size(kTestPstates));

  for (size_t i = 0; i < pstates.size(); i++) {
    EXPECT_EQ(pstates[i].voltage_uv, kTestPstates[i].voltage_uv);
    EXPECT_EQ(pstates[i].frequency_hz, kTestPstates[i].frequency_hz);
  }
}

TEST_F(PerformanceDomainTest, TestSetPerformanceState) {
  // Just move to the next sequential pstate with wraparound.
  const uint32_t test_pstate = (kInitialPstate + 1) % std::size(kTestPstates);
  const uint32_t invalid_pstate = std::size(kTestPstates) + 1;
  zx_status_t st = pd().SetPerformanceState(test_pstate);

  EXPECT_OK(st);

  {
    const auto [res, new_pstate, info] = pd().GetCurrentPerformanceState();
    EXPECT_OK(res);
    EXPECT_EQ(new_pstate, test_pstate);
  }

  st = pd().SetPerformanceState(invalid_pstate);
  EXPECT_NOT_OK(st);

  {
    // Make sure the pstate hasn't changed.
    const auto [res, new_pstate, info] = pd().GetCurrentPerformanceState();
    EXPECT_OK(res);
    EXPECT_EQ(new_pstate, test_pstate);
  }

  // Make sure there was exactly one successful call to SetPerformanceState.
  EXPECT_EQ(cpu().PstateSetCount(), 1);
}

}  // namespace
