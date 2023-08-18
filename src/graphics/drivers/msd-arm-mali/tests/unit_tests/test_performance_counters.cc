// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <thread>

#include <gtest/gtest.h>

#include "driver_logger_harness.h"
#include "mock/mock_bus_mapper.h"
#include "mock/mock_mmio.h"
#include "src/graphics/drivers/msd-arm-mali/src/address_manager.h"
#include "src/graphics/drivers/msd-arm-mali/src/performance_counters.h"
#include "src/graphics/drivers/msd-arm-mali/tests/unit_tests/fake_connection_owner_base.h"

namespace {
class FakeOwner : public AddressManager::Owner {
 public:
  FakeOwner(mali::RegisterIo* regs) : register_io_(regs) {}

  mali::RegisterIo* register_io() override { return register_io_; }

 private:
  mali::RegisterIo* register_io_;
};

class TestConnectionOwner : public FakeConnectionOwnerBase {
 public:
  TestConnectionOwner(AddressManager* manager) : manager_(manager) {}

  void ScheduleAtom(std::shared_ptr<MsdArmAtom> atom) override {}
  void CancelAtoms(std::shared_ptr<MsdArmConnection> connection) override {}
  AddressSpaceObserver* GetAddressSpaceObserver() override { return manager_; }
  magma::PlatformBusMapper* GetBusMapper() override { return &bus_mapper_; }

 private:
  AddressManager* manager_;
  MockBusMapper bus_mapper_;
};

class TestCounterOwner : public PerformanceCounters::Owner {
 public:
  TestCounterOwner(mali::RegisterIo* regs)
      : register_io_(regs),
        address_manager_owner_(regs),
        address_manager_(&address_manager_owner_, 2),
        connection_owner_(&address_manager_) {}

  mali::RegisterIo* register_io() override { return register_io_; }
  AddressManager* address_manager() override { return &address_manager_; }
  MsdArmConnection::Owner* connection_owner() override { return &connection_owner_; }

 private:
  mali::RegisterIo* register_io_;

  FakeOwner address_manager_owner_;
  AddressManager address_manager_;
  TestConnectionOwner connection_owner_;
};

class TestManager : public PerformanceCountersManager {
 public:
  std::vector<uint64_t> EnabledPerfCountFlags() override {
    return enabled_ ? std::vector<uint64_t>{1} : std::vector<uint64_t>{};
  }

  void set_enabled(bool enabled) { enabled_ = enabled; }

 private:
  bool enabled_ = false;
};

}  // namespace

class PerformanceCounterTest {
 public:
  static void TestStateChange() {
    auto mmio = std::make_unique<mali::RegisterIo>(MockMmio::Create(1024 * 1024));
    TestCounterOwner owner(mmio.get());
    PerformanceCounters perf_counters(&owner);
    TestManager manager;

    perf_counters.SetDeviceThreadId(std::this_thread::get_id());
    std::lock_guard lock(*perf_counters.device_thread_checker_);

    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kDisabled,
              perf_counters.counter_state_);
    EXPECT_FALSE(perf_counters.TriggerRead());

    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kDisabled,
              perf_counters.counter_state_);
    perf_counters.ReadCompleted();
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kDisabled,
              perf_counters.counter_state_);
    manager.set_enabled(true);
    perf_counters.AddManager(&manager);

    perf_counters.Update();
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kEnabled, perf_counters.counter_state_);

    perf_counters.ReadCompleted();
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kEnabled, perf_counters.counter_state_);

    EXPECT_TRUE(perf_counters.TriggerRead());
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kTriggered,
              perf_counters.counter_state_);

    manager.set_enabled(false);
    perf_counters.Update();

    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kTriggeredWillBeDisabled,
              perf_counters.counter_state_);

    perf_counters.ReadCompleted();
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kDisabled,
              perf_counters.counter_state_);
  }

  static void TestEnabled() {
    auto mmio = std::make_unique<mali::RegisterIo>(MockMmio::Create(1024 * 1024));
    TestCounterOwner owner(mmio.get());
    PerformanceCounters perf_counters(&owner);
    TestManager manager;
    perf_counters.SetDeviceThreadId(std::this_thread::get_id());
    std::lock_guard lock(*perf_counters.device_thread_checker_);

    perf_counters.AddManager(&manager);

    EXPECT_EQ(nullptr, owner.address_manager()->GetMappingForSlot(0).get());
    manager.set_enabled(true);
    perf_counters.Update();
    EXPECT_NE(nullptr, perf_counters.address_mapping_.get());
    EXPECT_EQ(perf_counters.address_mapping_, owner.address_manager()->GetMappingForSlot(0));

    EXPECT_TRUE(perf_counters.TriggerRead());
    registers::PerformanceCounterBase::Get().FromValue(4096 + 1024).WriteTo(mmio.get());
    struct TestClient : public PerformanceCounters::Client {
      void OnPerfCountDump(const std::vector<uint32_t>& dumped) override { dump_ = dumped; }
      void OnPerfCountersCanceled(size_t perf_counter_size) override { EXPECT_TRUE(false); }

      std::vector<uint32_t> dump_;
    };
    TestClient client;
    perf_counters.AddClient(&client);
    perf_counters.ReadCompleted();
    EXPECT_EQ(1024u / 4u, client.dump_.size());
    EXPECT_EQ(0u, client.dump_[0]);
    EXPECT_EQ(1u, registers::PerformanceCounterConfig::Get().ReadFrom(mmio.get()).mode());
    EXPECT_EQ(4096u, registers::PerformanceCounterBase::Get().ReadFrom(mmio.get()).reg_value());
  }

  static void TestForceDisable() {
    auto mmio = std::make_unique<mali::RegisterIo>(MockMmio::Create(1024 * 1024));
    TestCounterOwner owner(mmio.get());
    PerformanceCounters perf_counters(&owner);
    TestManager manager;
    GpuFeatures gpu_features;
    // 7 shader cores present, 1 through 7 (inclusive)
    gpu_features.shader_present = ((1 << 8) - 1) & ~1;
    gpu_features.mem_features.set_num_l2_slices_minus1(1);
    perf_counters.SetGpuFeatures(gpu_features);
    perf_counters.SetDeviceThreadId(std::this_thread::get_id());
    std::lock_guard lock(*perf_counters.device_thread_checker_);

    perf_counters.AddManager(&manager);

    EXPECT_EQ(nullptr, owner.address_manager()->GetMappingForSlot(0).get());
    manager.set_enabled(true);
    perf_counters.Update();
    EXPECT_NE(nullptr, perf_counters.address_mapping_.get());
    EXPECT_EQ(perf_counters.address_mapping_, owner.address_manager()->GetMappingForSlot(0));

    EXPECT_TRUE(perf_counters.TriggerRead());
    registers::PerformanceCounterBase::Get().FromValue(4096 + 1024).WriteTo(mmio.get());
    struct TestClient : public PerformanceCounters::Client {
      void OnPerfCountDump(const std::vector<uint32_t>& dumped) override { dump_ = dumped; }
      void OnPerfCountersCanceled(size_t perf_counter_size) override {
        // 1 JM + 1 tiler + 2 l2 slices + 8 shader cores.
        EXPECT_EQ(0x100u * (1 + 1 + 2 + 8), perf_counter_size);
        force_disable_count_++;
      }

      std::vector<uint32_t> dump_;
      uint32_t force_disable_count_ = 0;
    };
    TestClient client;
    perf_counters.AddClient(&client);
    perf_counters.ForceDisable();
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kDisabled,
              perf_counters.counter_state_);

    EXPECT_EQ(1u, client.force_disable_count_);
    // Could happen if the interrupt was delayed.
    perf_counters.ReadCompleted();
    EXPECT_EQ(2u, client.force_disable_count_);
    perf_counters.Update();
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kDisabled,
              perf_counters.counter_state_);
    perf_counters.RemoveForceDisable();
    perf_counters.Update();

    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kEnabled, perf_counters.counter_state_);
  }

  static void TestTriggerWhileDisabled() {
    auto mmio = std::make_unique<mali::RegisterIo>(MockMmio::Create(1024 * 1024));
    TestCounterOwner owner(mmio.get());
    PerformanceCounters perf_counters(&owner);
    TestManager manager;
    GpuFeatures gpu_features;
    // 7 shader cores present, 1 through 7 (inclusive)
    gpu_features.shader_present = ((1 << 8) - 1) & ~1;
    gpu_features.mem_features.set_num_l2_slices_minus1(1);
    perf_counters.SetGpuFeatures(gpu_features);
    perf_counters.SetDeviceThreadId(std::this_thread::get_id());
    std::lock_guard lock(*perf_counters.device_thread_checker_);

    perf_counters.AddManager(&manager);

    EXPECT_EQ(nullptr, owner.address_manager()->GetMappingForSlot(0).get());

    registers::PerformanceCounterBase::Get().FromValue(4096 + 1024).WriteTo(mmio.get());
    struct TestClient : public PerformanceCounters::Client {
      void OnPerfCountDump(const std::vector<uint32_t>& dumped) override { dump_ = dumped; }
      void OnPerfCountersCanceled(size_t perf_counter_size) override {
        // 1 JM + 1 tiler + 2 l2 slices + 8 shader cores.
        EXPECT_EQ(0x100u * (1 + 1 + 2 + 8), perf_counter_size);
        force_disable_count_++;
      }

      std::vector<uint32_t> dump_;
      uint32_t force_disable_count_ = 0;
    };
    TestClient client;
    perf_counters.AddClient(&client);
    perf_counters.ForceDisable();
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kDisabled,
              perf_counters.counter_state_);

    EXPECT_EQ(1u, client.force_disable_count_);
    manager.set_enabled(true);
    perf_counters.Update();
    EXPECT_FALSE(perf_counters.TriggerRead());
    EXPECT_EQ(2u, client.force_disable_count_);

    perf_counters.RemoveForceDisable();
    perf_counters.Update();

    EXPECT_EQ(2u, client.force_disable_count_);
    EXPECT_EQ(PerformanceCounters::PerformanceCounterState::kEnabled, perf_counters.counter_state_);
    EXPECT_NE(nullptr, perf_counters.address_mapping_.get());
    EXPECT_EQ(perf_counters.address_mapping_, owner.address_manager()->GetMappingForSlot(0));
  }
};

class PerfCounters : public testing::Test {
  void SetUp() override { logger_harness_ = DriverLoggerHarness::Create(); }
  std::unique_ptr<DriverLoggerHarness> logger_harness_;
};

TEST_F(PerfCounters, StateChange) { PerformanceCounterTest::TestStateChange(); }

TEST_F(PerfCounters, Enabled) { PerformanceCounterTest::TestEnabled(); }

TEST_F(PerfCounters, ForceDisable) { PerformanceCounterTest::TestForceDisable(); }

TEST_F(PerfCounters, TriggerWhileDisabled) { PerformanceCounterTest::TestTriggerWhileDisabled(); }
