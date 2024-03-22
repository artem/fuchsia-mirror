// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/cpu/drivers/aml-cpu/aml-cpu-v2.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire_test_base.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>
#include <sdk/lib/inspect/testing/cpp/inspect.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/lib/testing/predicates/status.h"

namespace amlogic_cpu {

using CpuCtrlClient = fidl::WireSyncClient<fuchsia_hardware_cpu_ctrl::Device>;

#define MHZ(x) ((x) * 1000000)

constexpr uint32_t kPdArmA53 = 1;

const std::vector<amlogic_cpu::perf_domain_t> kPerfDomains = {
    {.id = kPdArmA53, .core_count = 4, .relative_performance = 255, .name = "S905D2 ARM A53"},
};

const std::vector<amlogic_cpu::operating_point_t> kOperatingPointsMetadata = {
    {.freq_hz = 100'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 250'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 500'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 667'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'000'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'200'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'398'000'000, .volt_uv = 761'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'512'000'000, .volt_uv = 791'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'608'000'000, .volt_uv = 831'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'704'000'000, .volt_uv = 861'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'896'000'000, .volt_uv = 1'022'000, .pd_id = kPdArmA53},
};

const std::vector<amlogic_cpu::perf_domain_t> kTestPerfDomains = {
    {.id = kPdArmA53, .core_count = 1, .relative_performance = 0, .name = "testpd"},
};

const std::vector<operating_point_t> kTestOperatingPoints = {
    {.freq_hz = MHZ(10), .volt_uv = 1500, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(9), .volt_uv = 1350, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(8), .volt_uv = 1200, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(7), .volt_uv = 1050, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(6), .volt_uv = 900, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(5), .volt_uv = 750, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(4), .volt_uv = 600, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(3), .volt_uv = 450, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(2), .volt_uv = 300, .pd_id = kPdArmA53},
    {.freq_hz = MHZ(1), .volt_uv = 150, .pd_id = kPdArmA53},
};

class FakeMmio {
 public:
  FakeMmio() : mmio_(sizeof(uint32_t), kRegCount) {
    mmio_[kCpuVersionOffset].SetReadCallback([]() { return kCpuVersion; });
  }

  fdf::MmioBuffer mmio() { return mmio_.GetMmioBuffer(); }

 private:
  static constexpr size_t kCpuVersionOffset = 0x220;
  static constexpr size_t kRegCount = kCpuVersionOffset / sizeof(uint32_t) + 1;

  constexpr static uint64_t kCpuVersion = 0x28200b02;

  ddk_fake::FakeMmioRegRegion mmio_;
};

class TestClockDevice : public fidl::testing::WireTestBase<fuchsia_hardware_clock::Clock> {
 public:
  fuchsia_hardware_clock::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_clock::Service::InstanceHandler({
        .clock = binding_group_.CreateHandler(
            this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
    });
  }

  void Enable(EnableCompleter::Sync& completer) override {
    enabled_ = true;
    completer.ReplySuccess();
  }

  void Disable(DisableCompleter::Sync& completer) override {
    enabled_ = false;
    completer.ReplySuccess();
  }

  void IsEnabled(IsEnabledCompleter::Sync& completer) override { completer.ReplySuccess(enabled_); }

  void SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) override {
    rate_.emplace(request->hz);
    completer.ReplySuccess();
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fidl::ClientEnd<fuchsia_hardware_clock::Clock> Connect() {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_clock::Clock>();
    EXPECT_OK(endpoints.status_value());
    binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                              std::move(endpoints->server), this, fidl::kIgnoreBindingClosure);
    return std::move(endpoints->client);
  }

  bool enabled() const { return enabled_; }
  std::optional<uint32_t> rate() const { return rate_; }

 private:
  bool enabled_ = false;
  std::optional<uint32_t> rate_;
  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> binding_group_;
};

class TestPowerDevice : public fidl::WireServer<fuchsia_hardware_power::Device> {
 public:
  fuchsia_hardware_power::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_power::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(
            this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
    });
  }

  void ConnectRequest(fidl::ServerEnd<fuchsia_hardware_power::Device> request) {
    binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request),
                              this, fidl::kIgnoreBindingClosure);
  }

  fidl::ClientEnd<fuchsia_hardware_power::Device> Connect() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_power::Device>();
    EXPECT_OK(endpoints.status_value());
    ConnectRequest(std::move(endpoints->server));
    return std::move(endpoints->client);
  }

  void RegisterPowerDomain(RegisterPowerDomainRequestView request,
                           RegisterPowerDomainCompleter::Sync& completer) override {
    min_needed_voltage_ = request->min_needed_voltage;
    max_supported_voltage_ = request->max_supported_voltage;
    completer.ReplySuccess();
  }

  void UnregisterPowerDomain(UnregisterPowerDomainCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void GetPowerDomainStatus(GetPowerDomainStatusCompleter::Sync& completer) override {
    completer.ReplySuccess(fuchsia_hardware_power::wire::PowerDomainStatus::kEnabled);
  }
  void GetSupportedVoltageRange(GetSupportedVoltageRangeCompleter::Sync& completer) override {
    completer.ReplySuccess(min_voltage_, max_voltage_);
  }

  void RequestVoltage(RequestVoltageRequestView request,
                      RequestVoltageCompleter::Sync& completer) override {
    voltage_ = request->voltage;
    completer.ReplySuccess(voltage_);
  }

  void GetCurrentVoltage(GetCurrentVoltageRequestView request,
                         GetCurrentVoltageCompleter::Sync& completer) override {
    completer.ReplySuccess(voltage_);
  }

  void WritePmicCtrlReg(WritePmicCtrlRegRequestView request,
                        WritePmicCtrlRegCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void ReadPmicCtrlReg(ReadPmicCtrlRegRequestView request,
                       ReadPmicCtrlRegCompleter::Sync& completer) override {
    completer.ReplySuccess(1);
  }

  void SetSupportedVoltageRange(uint32_t min_voltage, uint32_t max_voltage) {
    min_voltage_ = min_voltage;
    max_voltage_ = max_voltage;
  }

  void SetVoltage(uint32_t voltage) { voltage_ = voltage; }

  uint32_t voltage() const { return voltage_; }
  uint32_t min_needed_voltage() const { return min_needed_voltage_; }
  uint32_t max_supported_voltage() const { return max_supported_voltage_; }

 private:
  uint32_t voltage_ = 0;
  uint32_t min_voltage_ = 0;
  uint32_t max_voltage_ = 0;
  uint32_t min_needed_voltage_ = 0;
  uint32_t max_supported_voltage_ = 0;
  fidl::ServerBindingGroup<fuchsia_hardware_power::Device> binding_group_;
};

class AmlCpuEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) {
    auto dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    device_server_.Init("pdev", "root");
    EXPECT_EQ(ZX_OK, device_server_.Serve(dispatcher, &to_driver_vfs));

    std::map<uint32_t, fake_pdev::Mmio> mmios;
    mmios[0] = mmio_.mmio();

    pdev_server_.SetConfig(fake_pdev::FakePDevFidl::Config{
        .mmios = std::move(mmios),
        .device_info =
            pdev_device_info_t{
                .pid = PDEV_PID_ASTRO,
            },
    });
    EXPECT_OK(to_driver_vfs
                  .AddService<fuchsia_hardware_platform_device::Service>(
                      pdev_server_.GetInstanceHandler(dispatcher), "pdev")
                  .status_value());

    power_server_.SetVoltage(0);
    power_server_.SetSupportedVoltageRange(0, 0);
    EXPECT_OK(to_driver_vfs
                  .AddService<fuchsia_hardware_power::Service>(power_server_.GetInstanceHandler(),
                                                               "power-01")
                  .status_value());

    EXPECT_OK(to_driver_vfs
                  .AddService<fuchsia_hardware_clock::Service>(
                      clock_pll_div16_server_.GetInstanceHandler(), "clock-pll-div16-01")
                  .status_value());

    EXPECT_OK(to_driver_vfs
                  .AddService<fuchsia_hardware_clock::Service>(
                      clock_cpu_div16_server_.GetInstanceHandler(), "clock-cpu-div16-01")
                  .status_value());

    EXPECT_OK(to_driver_vfs
                  .AddService<fuchsia_hardware_clock::Service>(
                      clock_cpu_scaler_server_.GetInstanceHandler(), "clock-cpu-scaler-01")
                  .status_value());
    return zx::ok();
  }

  compat::DeviceServer device_server_;
  FakeMmio mmio_;
  fake_pdev::FakePDevFidl pdev_server_;
  TestPowerDevice power_server_;
  TestClockDevice clock_pll_div16_server_;
  TestClockDevice clock_cpu_div16_server_;
  TestClockDevice clock_cpu_scaler_server_;
};

class AmlCpuBindingConfiguration final {
 public:
  static constexpr bool kDriverOnForeground = false;
  static constexpr bool kAutoStartDriver = false;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = AmlCpuV2;
  using EnvironmentType = AmlCpuEnvironment;
};

class AmlCpuTest : public fdf_testing::DriverTestFixture<AmlCpuBindingConfiguration> {
 public:
  void StartWithMetadata(const std::vector<perf_domain_t>& perf_domains,
                         const std::vector<operating_point_t>& op_points) {
    // Notes on AmlCpu Initialization:
    //  + Should enable the CPU and PLL clocks.
    //  + Should initially assume that the device is in it's opp.
    //  + Should configure the device to it's highest opp.
    const operating_point_t& slowest = op_points.front();
    const operating_point_t& fastest = op_points.back();

    RunInEnvironmentTypeContext([slowest, fastest](AmlCpuEnvironment& env) {
      // The DUT should initialize.
      env.power_server_.SetSupportedVoltageRange(slowest.volt_uv, fastest.volt_uv);

      // The DUT scales up to the fastest available opp.
      env.power_server_.SetVoltage(fastest.volt_uv);
    });

    RunInEnvironmentTypeContext([&perf_domains, &op_points](AmlCpuEnvironment& env) {
      env.device_server_.AddMetadata(DEVICE_METADATA_AML_PERF_DOMAINS, perf_domains.data(),
                                     perf_domains.size() * sizeof(perf_domain_t));
      env.device_server_.AddMetadata(DEVICE_METADATA_AML_OP_POINTS, op_points.data(),
                                     op_points.size() * sizeof(operating_point_t));
    });
    ASSERT_OK(StartDriver().status_value());

    RunInEnvironmentTypeContext([slowest, fastest](AmlCpuEnvironment& env) {
      ASSERT_EQ(env.power_server_.min_needed_voltage(), slowest.volt_uv);
      ASSERT_EQ(env.power_server_.max_supported_voltage(), fastest.volt_uv);
    });

    RunInNodeContext([&perf_domains](fdf_testing::TestNode& node) {
      ASSERT_EQ(node.children().size(), perf_domains.size());
    });
  }

  void ConnectToCpuCtrl(const perf_domain_t& perf_domain) {
    auto device = ConnectThroughDevfs<fuchsia_hardware_cpu_ctrl::Device>(perf_domain.name);
    EXPECT_OK(device.status_value());
    cpu_ctrl_.Bind(std::move(device.value()));
  }

  CpuCtrlClient cpu_ctrl_;
};

TEST_F(AmlCpuTest, TrivialBinding) { StartWithMetadata(kPerfDomains, kOperatingPointsMetadata); }

TEST_F(AmlCpuTest, UnorderedOperatingPoints) {
  // AML CPU's bind hook expects that all operating points are strictly
  // ordered and it should handle the situation where there are duplicate
  // frequencies.
  const std::vector<amlogic_cpu::operating_point_t> kOperatingPointsMetadata = {
      {.freq_hz = MHZ(1), .volt_uv = 200'000, .pd_id = kPdArmA53},
      {.freq_hz = MHZ(1), .volt_uv = 100'000, .pd_id = kPdArmA53},
      {.freq_hz = MHZ(1), .volt_uv = 300'000, .pd_id = kPdArmA53},
  };

  StartWithMetadata(kPerfDomains, kOperatingPointsMetadata);

  ConnectToCpuCtrl(kPerfDomains[0]);

  auto out = cpu_ctrl_->SetCurrentOperatingPoint(0);
  EXPECT_OK(out.status());
  EXPECT_EQ(out->value()->out_opp, 0ul);

  RunInEnvironmentTypeContext([](AmlCpuEnvironment& env) {
    uint32_t voltage = env.power_server_.voltage();
    EXPECT_EQ(voltage, 300'000u);
  });
}

TEST_F(AmlCpuTest, TestGetOperatingPointInfo) {
  StartWithMetadata(kTestPerfDomains, kTestOperatingPoints);
  ConnectToCpuCtrl(kTestPerfDomains[0]);

  auto opp_size = kTestOperatingPoints.size();

  // Make sure that we can get information about all the supported opps.
  for (size_t i = 0; i < opp_size; i++) {
    const uint32_t opp = static_cast<uint32_t>(i);
    auto oppInfo = cpu_ctrl_->GetOperatingPointInfo(opp);

    // First, make sure there were no transport errors.
    ASSERT_OK(oppInfo.status());

    // Then make sure that the driver accepted the call.
    ASSERT_FALSE(oppInfo->is_error());

    // Then make sure that we're getting the expected frequency and voltage values.
    EXPECT_EQ(oppInfo->value()->info.frequency_hz, kTestOperatingPoints[i].freq_hz);
    EXPECT_EQ(oppInfo->value()->info.voltage_uv, kTestOperatingPoints[i].volt_uv);
  }

  // Make sure that we can't get any information about opps that don't
  // exist.
  for (size_t i = opp_size; i < opp_size + 10; i++) {
    const uint32_t opp = static_cast<uint32_t>(i);
    auto oppInfo = cpu_ctrl_->GetOperatingPointInfo(opp);

    // Even if it's an unsupported opp, we still expect the transport to
    // deliver the message successfully.
    ASSERT_OK(oppInfo.status());

    // Make sure that the driver returns an error, however.
    EXPECT_TRUE(oppInfo->is_error());
  }
}

TEST_F(AmlCpuTest, TestSetCurrentOperatingPoint) {
  StartWithMetadata(kTestPerfDomains, kTestOperatingPoints);
  ConnectToCpuCtrl(kTestPerfDomains[0]);

  // Scale to the lowest opp.
  const uint32_t min_opp_index = static_cast<uint32_t>(kTestOperatingPoints.size() - 1);
  const operating_point_t& min_opp = kTestOperatingPoints[min_opp_index];

  RunInEnvironmentTypeContext(
      [&min_opp](AmlCpuEnvironment& env) { env.power_server_.SetVoltage(min_opp.volt_uv); });

  auto min_result = cpu_ctrl_->SetCurrentOperatingPoint(min_opp_index);
  EXPECT_OK(min_result.status());
  EXPECT_TRUE(min_result->is_ok());
  EXPECT_EQ(min_result->value()->out_opp, min_opp_index);
  RunInEnvironmentTypeContext([&min_opp](AmlCpuEnvironment& env) {
    auto rate = env.clock_cpu_scaler_server_.rate();
    ASSERT_TRUE(rate.has_value());
    ASSERT_EQ(rate.value(), min_opp.freq_hz);
  });

  // Check that we get the same value back from GetCurrentOperatingPoint
  auto min_result_again = cpu_ctrl_->GetCurrentOperatingPoint();
  EXPECT_OK(min_result_again.status());
  EXPECT_EQ(min_result_again.value().out_opp, min_opp_index);

  // Scale to the highest opp.
  const uint32_t max_opp_index = 0;
  const operating_point_t& max_opp = kTestOperatingPoints[max_opp_index];

  RunInEnvironmentTypeContext(
      [&max_opp](AmlCpuEnvironment& env) { env.power_server_.SetVoltage(max_opp.volt_uv); });

  auto max_result = cpu_ctrl_->SetCurrentOperatingPoint(max_opp_index);
  EXPECT_OK(max_result.status());
  EXPECT_TRUE(max_result->is_ok());
  EXPECT_EQ(max_result->value()->out_opp, max_opp_index);
  RunInEnvironmentTypeContext([&max_opp](AmlCpuEnvironment& env) {
    auto rate = env.clock_cpu_scaler_server_.rate();
    ASSERT_TRUE(rate.has_value());
    ASSERT_EQ(rate.value(), max_opp.freq_hz);
  });

  // Check that we get the same value back from GetCurrentOperatingPoint
  auto max_result_again = cpu_ctrl_->GetCurrentOperatingPoint();
  EXPECT_OK(max_result_again.status());
  EXPECT_EQ(max_result_again.value().out_opp, max_opp_index);

  // Set to the opp that we're already at and make sure that it's a no-op.
  auto same_result = cpu_ctrl_->SetCurrentOperatingPoint(max_opp_index);
  EXPECT_OK(same_result.status());
  EXPECT_TRUE(same_result->is_ok());
  EXPECT_EQ(same_result->value()->out_opp, max_opp_index);

  // Check that we get the same value back from GetCurrentOperatingPoint
  auto same_result_again = cpu_ctrl_->GetCurrentOperatingPoint();
  EXPECT_OK(same_result_again.status());
  EXPECT_EQ(same_result_again.value().out_opp, max_opp_index);
}

TEST_F(AmlCpuTest, TestCpuInfo) {
  using namespace inspect::testing;

  StartWithMetadata(kTestPerfDomains, kTestOperatingPoints);

  RunInDriverContext([](AmlCpuV2& driver) {
    auto& dut = driver.performance_domains().front();

    auto hierarchy = inspect::ReadFromVmo(dut->inspect_vmo());
    EXPECT_TRUE(hierarchy.is_ok());
    auto* cpu_info = hierarchy.value().GetByPath({"cpu_info_service"});
    EXPECT_TRUE(cpu_info);

    // cpu_major_revision : 40
    EXPECT_THAT(*cpu_info, NodeMatches(AllOf(PropertyList(::testing::UnorderedElementsAre(
                               UintIs("cpu_major_revision", 40), UintIs("cpu_minor_revision", 11),
                               UintIs("cpu_package_id", 2))))));
  });
}

TEST_F(AmlCpuTest, TestGetOperatingPointCount) {
  StartWithMetadata(kTestPerfDomains, kTestOperatingPoints);
  ConnectToCpuCtrl(kTestPerfDomains[0]);

  auto resp = cpu_ctrl_->GetOperatingPointCount();

  ASSERT_OK(resp.status());

  EXPECT_EQ(resp.value()->count, kTestOperatingPoints.size());
}

TEST_F(AmlCpuTest, TestGetLogicalCoreCount) {
  StartWithMetadata(kTestPerfDomains, kTestOperatingPoints);
  ConnectToCpuCtrl(kTestPerfDomains[0]);

  auto coreCountResp = cpu_ctrl_->GetNumLogicalCores();

  ASSERT_OK(coreCountResp.status());

  EXPECT_EQ(coreCountResp.value().count, kTestPerfDomains[0].core_count);
}
}  // namespace amlogic_cpu
