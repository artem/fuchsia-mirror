// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <optional>
#include <span>
#include <vector>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <soc/aml-common/aml-i2c.h>
#include <zxtest/zxtest.h>

#include "aml-i2c-regs.h"
#include "dfv2-driver.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"

namespace aml_i2c {

class FakeDfv2DriverController {
 public:
  struct Transfer {
    std::vector<uint8_t> write_data;
    std::vector<uint8_t> token_list;
    uint32_t target_addr;
    bool is_read;

    void ExpectTokenListEq(const std::vector<uint8_t>& expected) const {
      ASSERT_EQ(token_list.size(), expected.size());
      EXPECT_BYTES_EQ(token_list.data(), expected.data(), expected.size());
    }
  };

  explicit FakeDfv2DriverController(zx::unowned_interrupt irq)
      : mmio_(sizeof(reg_values_[0]), 8), irq_(std::move(irq)) {
    for (size_t i = 0; i < std::size(reg_values_); i++) {
      mmio_[i * sizeof(reg_values_[0])].SetReadCallback(ReadRegCallback(i));
      mmio_[i * sizeof(reg_values_[0])].SetWriteCallback(WriteRegCallback(i));
    }
    mmio_[kControlReg].SetWriteCallback([&](uint64_t value) { WriteControlReg(value); });
  }

  FakeDfv2DriverController(FakeDfv2DriverController&& other) = delete;
  FakeDfv2DriverController& operator=(FakeDfv2DriverController&& other) = delete;

  FakeDfv2DriverController(const FakeDfv2DriverController& other) = delete;
  FakeDfv2DriverController& operator=(const FakeDfv2DriverController& other) = delete;

  fdf::MmioBuffer GetMmioBuffer() { return mmio_.GetMmioBuffer(); }

  void SetReadData(cpp20::span<const uint8_t> read_data) { read_data_ = read_data; }

  std::vector<Transfer> GetTransfers() { return std::move(transfers_); }

  cpp20::span<uint32_t> mmio() { return {reg_values_, std::size(reg_values_)}; }

 private:
  std::function<uint64_t(void)> ReadRegCallback(size_t offset) {
    return [this, offset]() { return reg_values_[offset]; };
  }

  std::function<void(uint64_t)> WriteRegCallback(size_t offset) {
    return [this, offset](uint64_t value) {
      EXPECT_EQ(value & 0xffff'ffff, value);
      reg_values_[offset] = static_cast<uint32_t>(value);
    };
  }

  void WriteControlReg(uint64_t value) {
    EXPECT_EQ(value & 0xffff'ffff, value);
    if (value & 1) {
      // Start flag -- process the token list (saving the target address and/or data if needed),
      // then trigger the interrupt.
      ProcessTokenList();
      irq_->trigger(0, zx::clock::get_monotonic());
    }
    reg_values_[kControlReg / sizeof(uint32_t)] = static_cast<uint32_t>(value);
  }

  void ProcessTokenList() {
    uint64_t token_list = Get64BitReg(kTokenList0Reg);
    uint64_t write_data = Get64BitReg(kWriteData0Reg);
    uint64_t read_data = 0;
    size_t data_count = 0;

    for (uint64_t token = token_list & 0xf;; token_list >>= 4, token = token_list & 0xf) {
      // Skip most token validation as test cases can check against the expected token sequence.
      switch (static_cast<TokenList::Token>(token)) {
        case TokenList::Token::kEnd:
        case TokenList::Token::kStop:
          break;
        case TokenList::Token::kStart: {
          const uint32_t target_addr = reg_values_[kTargetAddrReg / sizeof(uint32_t)];
          EXPECT_EQ(target_addr & 1, 0);
          transfers_.push_back({.target_addr = (target_addr >> 1) & 0x7f});
          break;
        }
        case TokenList::Token::kTargetAddrWr:
          ASSERT_FALSE(transfers_.empty());
          transfers_.back().is_read = false;
          break;
        case TokenList::Token::kTargetAddrRd:
          ASSERT_FALSE(transfers_.empty());
          transfers_.back().is_read = true;
          break;
        case TokenList::Token::kData:
        case TokenList::Token::kDataLast:
          ASSERT_FALSE(transfers_.empty());
          if (transfers_.back().is_read) {
            ASSERT_FALSE(read_data_.empty());
            read_data |= static_cast<uint64_t>(read_data_[0]) << (8 * data_count++);
            read_data_ = read_data_.subspan(1);
          } else {
            transfers_.back().write_data.push_back(write_data & 0xff);
            write_data >>= 8;
          }
          break;
        default:
          ASSERT_TRUE(false, "Invalid token %lu", token_list & 0xf);
          break;
      }

      ASSERT_FALSE(transfers_.empty());
      transfers_.back().token_list.push_back(token_list & 0xf);

      if (static_cast<TokenList::Token>(token) == TokenList::Token::kEnd) {
        break;
      }
    }

    EXPECT_EQ(token_list, 0);  // There should be no tokens after the end token.

    reg_values_[kReadData0Reg / sizeof(uint32_t)] = read_data & 0xffff'ffff;
    reg_values_[kReadData1Reg / sizeof(uint32_t)] = read_data >> 32;
  }

  uint64_t Get64BitReg(size_t offset) const {
    return reg_values_[offset / sizeof(uint32_t)] |
           (static_cast<uint64_t>(reg_values_[(offset / sizeof(uint32_t)) + 1]) << 32);
  }

  ddk_fake::FakeMmioRegRegion mmio_;
  uint32_t reg_values_[8]{};
  zx::unowned_interrupt irq_;
  std::vector<Transfer> transfers_;
  cpp20::span<const uint8_t> read_data_;
};

class Environment {
 public:
  fdf_testing::TestNode::CreateStartArgsResult Init(fake_pdev::FakePDevFidl::Config pdev_config,
                                                    std::optional<aml_i2c_delay_values> metadata) {
    zx::result start_args_result = test_node_.CreateStartArgsAndServe();
    ZX_ASSERT(start_args_result.is_ok());

    zx::result init_result =
        test_environment_.Initialize(std::move(start_args_result->incoming_directory_server));
    ZX_ASSERT(init_result.is_ok());

    pdev_server_.SetConfig(std::move(pdev_config));

    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    std::string instance_name = "pdev";

    zx::result add_service_result =
        test_environment_.incoming_directory()
            .AddService<fuchsia_hardware_platform_device::Service>(
                pdev_server_.GetInstanceHandler(dispatcher), instance_name);
    ZX_ASSERT(add_service_result.is_ok());

    compat_server_.Init("default", "topo");
    if (metadata.has_value()) {
      compat_server_.AddMetadata(DEVICE_METADATA_PRIVATE, &metadata.value(),
                                 sizeof(metadata.value()));
    }
    zx_status_t status = compat_server_.Serve(dispatcher, &test_environment_.incoming_directory());
    ZX_ASSERT(status == ZX_OK);

    return std::move(start_args_result.value());
  }

 private:
  fdf_testing::TestNode test_node_{"root"};
  fake_pdev::FakePDevFidl pdev_server_;
  fdf_testing::TestEnvironment test_environment_;
  compat::DeviceServer compat_server_;
  std::optional<fdf::WireSyncClient<fuchsia_hardware_i2cimpl::Device>> i2c_;
};

class AmlI2cDfv2Test : public zxtest::Test {
 public:
  // Convenience definitions that don't require casting.
  enum Token : uint8_t {
    kEnd,
    kStart,
    kTargetAddrWr,
    kTargetAddrRd,
    kData,
    kDataLast,
    kStop,
  };

  void TearDown() override {
    ASSERT_OK(runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<Dfv2Driver>::PrepareStop)));
  }

  void InitDriver(std::optional<aml_i2c_delay_values> metadata = std::nullopt,
                  uint32_t mmio_count = 1, uint32_t irq_count = 1,
                  bool start_should_succeed = true) {
    zx::result pdev_config = InitController(mmio_count, irq_count);
    ASSERT_OK(pdev_config.status_value());
    auto init_result =
        env_.SyncCall(&Environment::Init, std::move(pdev_config.value()), std::move(metadata));

    // Start driver.
    auto result = runtime_.RunToCompletion(dut_.SyncCall(
        &fdf_testing::DriverUnderTest<Dfv2Driver>::Start, std::move(init_result.start_args)));
    if (start_should_succeed) {
      ASSERT_OK(result.status_value());
      dut_.SyncCall([](fdf_testing::DriverUnderTest<Dfv2Driver>* dut) {
        (*dut)->SetTimeout(zx::duration(ZX_TIME_INFINITE));
      });
    }

    ConnectToI2cImpl(init_result.outgoing_directory_client);
  }

  // `InitDriver` must be called before using this method.
  FakeDfv2DriverController& controller() {
    EXPECT_TRUE(controller_.has_value());
    return controller_.value();
  }

  cpp20::span<uint32_t> mmio() { return controller().mmio(); }

  fdf::Arena arena_{'TEST'};
  fdf::WireSyncClient<fuchsia_hardware_i2cimpl::Device> i2c_;

 private:
  static constexpr size_t kMmioSize = sizeof(uint32_t) * 8;

  void ConnectToI2cImpl(fidl::ClientEnd<fuchsia_io::Directory>& outgoing_directory_client) {
    auto svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(ZX_OK, svc_endpoints.status_value());

    zx_status_t status = fdio_open_at(outgoing_directory_client.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints->server.TakeChannel().release());
    ASSERT_EQ(ZX_OK, status);

    auto connect_result =
        fdf::internal::DriverTransportConnect<fuchsia_hardware_i2cimpl::Service::Device>(
            svc_endpoints->client, component::kDefaultInstance);
    ASSERT_TRUE(connect_result.is_ok());
    i2c_.Bind(std::move(connect_result.value()));
    ASSERT_TRUE(i2c_.is_valid());
  }

  zx::result<fake_pdev::FakePDevFidl::Config> InitController(uint32_t mmio_count,
                                                             uint32_t irq_count) {
    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    zx_status_t status =
        zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]);
    if (status != ZX_OK) {
      return zx::error(status);
    }
    controller_.emplace(config.irqs[0].borrow());
    config.mmios[0] = controller().GetMmioBuffer();

    config.device_info = {
        .mmio_count = mmio_count,
        .irq_count = irq_count,
    };
    return zx::ok(std::move(config));
  }

  std::optional<FakeDfv2DriverController> controller_;
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<Environment> env_{env_dispatcher_->async_dispatcher(),
                                                        std::in_place};
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<aml_i2c::Dfv2Driver>> dut_{
      driver_dispatcher_->async_dispatcher(), std::in_place};
};

TEST_F(AmlI2cDfv2Test, SmallWrite) {
  InitDriver();

  constexpr uint8_t kWriteData[]{0x45, 0xd9, 0x65, 0xbc, 0x31, 0x26, 0xd7, 0xe5};

  fidl::VectorView<uint8_t> write_buffer{arena_, kWriteData};
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x13,
       fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
           fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&write_buffer)),
       true}};

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  EXPECT_EQ(transact_result->value()->read.count(), 0);

  const std::vector transfers = controller().GetTransfers();
  ASSERT_EQ(transfers.size(), 1);

  EXPECT_EQ(transfers[0].target_addr, 0x13);
  ASSERT_EQ(transfers[0].write_data.size(), std::size(kWriteData));
  EXPECT_BYTES_EQ(transfers[0].write_data.data(), kWriteData, sizeof(kWriteData));
  transfers[0].ExpectTokenListEq({
      kStart,
      kTargetAddrWr,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kStop,
      kEnd,
  });
}

TEST_F(AmlI2cDfv2Test, BigWrite) {
  InitDriver();

  constexpr uint8_t kWriteData[]{0xb9, 0x17, 0x32, 0xba, 0x8e, 0xf7, 0x19, 0xf2, 0x78, 0xbf,
                                 0xcb, 0xd3, 0xdc, 0xad, 0xbd, 0x78, 0x1b, 0xa8, 0xef, 0x1a};

  fidl::VectorView<uint8_t> write_buffer{arena_, kWriteData};
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x5f,
       fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
           fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&write_buffer)),
       true}};

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  EXPECT_EQ(transact_result->value()->read.count(), 0);

  const std::vector transfers = controller().GetTransfers();
  ASSERT_EQ(transfers.size(), 1);

  EXPECT_EQ(transfers[0].target_addr, 0x5f);
  ASSERT_EQ(transfers[0].write_data.size(), std::size(kWriteData));
  EXPECT_BYTES_EQ(transfers[0].write_data.data(), kWriteData, sizeof(kWriteData));
  transfers[0].ExpectTokenListEq({
      // First transfer
      kStart,
      kTargetAddrWr,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kEnd,

      // Second transfer
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kEnd,

      // Third transfer
      kData,
      kData,
      kData,
      kData,
      kStop,
      kEnd,
  });
}

TEST_F(AmlI2cDfv2Test, SmallRead) {
  InitDriver();

  constexpr uint8_t kExpectedReadData[]{0xf0, 0xdb, 0xdf, 0x6b, 0xb9, 0x3e, 0xa6, 0xfa};
  controller().SetReadData({kExpectedReadData, std::size(kExpectedReadData)});

  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x41, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(sizeof(kExpectedReadData)),
       true}};

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  const auto& read = transact_result->value()->read;
  ASSERT_EQ(read.count(), 1);
  EXPECT_EQ(read[0].data.count(), sizeof(kExpectedReadData));
  EXPECT_BYTES_EQ(read[0].data.data(), kExpectedReadData, sizeof(kExpectedReadData));

  const std::vector transfers = controller().GetTransfers();
  ASSERT_EQ(transfers.size(), 1);

  EXPECT_EQ(transfers[0].target_addr, 0x41);
  transfers[0].ExpectTokenListEq({
      kStart,
      kTargetAddrRd,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kDataLast,
      kStop,
      kEnd,
  });
}

TEST_F(AmlI2cDfv2Test, BigRead) {
  InitDriver();

  constexpr uint8_t kExpectedReadData[]{0xb9, 0x17, 0x32, 0xba, 0x8e, 0xf7, 0x19, 0xf2, 0x78, 0xbf,
                                        0xcb, 0xd3, 0xdc, 0xad, 0xbd, 0x78, 0x1b, 0xa8, 0xef, 0x1a};
  controller().SetReadData({kExpectedReadData, std::size(kExpectedReadData)});

  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x29, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(sizeof(kExpectedReadData)),
       true}};

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  const auto& read = transact_result->value()->read;
  ASSERT_EQ(read.count(), 1);
  EXPECT_EQ(read[0].data.count(), sizeof(kExpectedReadData));
  EXPECT_BYTES_EQ(read[0].data.data(), kExpectedReadData, sizeof(kExpectedReadData));

  const std::vector transfers = controller().GetTransfers();
  ASSERT_EQ(transfers.size(), 1);

  EXPECT_EQ(transfers[0].target_addr, 0x29);
  transfers[0].ExpectTokenListEq({
      // First transfer
      kStart,
      kTargetAddrRd,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kEnd,

      // Second transfer
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kEnd,

      // Third transfer
      kData,
      kData,
      kData,
      kDataLast,
      kStop,
      kEnd,
  });
}

TEST_F(AmlI2cDfv2Test, EmptyRead) {
  InitDriver();

  controller().SetReadData({});

  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x41, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(0), true}};

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  const auto& read = transact_result->value()->read;
  ASSERT_EQ(read.count(), 1);
  EXPECT_TRUE(read[0].data.empty());

  const std::vector transfers = controller().GetTransfers();
  ASSERT_TRUE(transfers.empty());
}

TEST_F(AmlI2cDfv2Test, NoStopFlag) {
  InitDriver();

  fidl::VectorView<uint8_t> buffer{arena_, 4};
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x00,
       fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
           fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&buffer)),
       false}};

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());

  const std::vector transfers = controller().GetTransfers();
  ASSERT_EQ(transfers.size(), 1);

  transfers[0].ExpectTokenListEq({kStart, kTargetAddrWr, kData, kData, kData, kData, kEnd});
}

TEST_F(AmlI2cDfv2Test, TransferError) {
  InitDriver();

  uint8_t buffer[4];
  controller().SetReadData({buffer, std::size(buffer)});
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x00, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(4), false}};

  mmio()[kControlReg / sizeof(uint32_t)] = 1 << 3;

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  EXPECT_TRUE(transact_result->is_error());
}

TEST_F(AmlI2cDfv2Test, ManyTransactions) {
  InitDriver();

  const uint32_t kReadCount1 = 20;
  constexpr uint32_t kReadCount2 = 4;
  constexpr uint8_t kExpectedReadData[]{0x85, 0xb0, 0xd0, 0x1c, 0xc6, 0x8a, 0x35, 0xfc,
                                        0xcf, 0xca, 0x95, 0x01, 0x61, 0x42, 0x60, 0x8c,
                                        0xa6, 0x01, 0xd6, 0x2e, 0x38, 0x20, 0x09, 0xfa};
  controller().SetReadData({kExpectedReadData, std::size(kExpectedReadData)});

  constexpr uint8_t kExpectedWriteData[]{0x39, 0xf0, 0xf9, 0x17, 0xad, 0x51, 0xdc, 0x30, 0xe5};

  fidl::VectorView<uint8_t> write_buffer_1{arena_, cpp20::span(kExpectedWriteData, 1)};
  fidl::VectorView<uint8_t> write_buffer_2{
      arena_, cpp20::span(kExpectedWriteData + 1, sizeof(kExpectedWriteData) - 1)};

  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> ops = {
      {0x1c,
       fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
           fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&write_buffer_1)),
       false},
      {0x2d, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(kReadCount1), true},
      {0x3e,
       fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
           fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&write_buffer_2)),
       true},
      {0x4f, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(kReadCount2), false},
  };

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, ops});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  const auto& read = transact_result->value()->read;
  ASSERT_EQ(read.count(), 2);
  EXPECT_EQ(read[0].data.count(), kReadCount1);
  EXPECT_BYTES_EQ(read[0].data.data(), kExpectedReadData, kReadCount1);
  EXPECT_EQ(read[1].data.count(), kReadCount2);
  EXPECT_BYTES_EQ(read[1].data.data(), kExpectedReadData + kReadCount1, kReadCount2);

  const std::vector transfers = controller().GetTransfers();
  ASSERT_EQ(transfers.size(), 4);

  EXPECT_EQ(transfers[0].target_addr, 0x1c);
  ASSERT_EQ(transfers[0].write_data.size(), 1);
  EXPECT_EQ(transfers[0].write_data[0], kExpectedWriteData[0]);
  transfers[0].ExpectTokenListEq({kStart, kTargetAddrWr, kData, kEnd});

  EXPECT_EQ(transfers[1].target_addr, 0x2d);
  transfers[1].ExpectTokenListEq({
      // First transfer
      kStart,
      kTargetAddrRd,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kEnd,

      // Second transfer
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kEnd,

      // Third transfer
      kData,
      kData,
      kData,
      kDataLast,
      kStop,
      kEnd,
  });

  EXPECT_EQ(transfers[2].target_addr, 0x3e);
  ASSERT_EQ(transfers[2].write_data.size(), write_buffer_2.count());
  EXPECT_BYTES_EQ(transfers[2].write_data.data(), kExpectedWriteData + 1, write_buffer_2.count());
  transfers[2].ExpectTokenListEq({
      kStart,
      kTargetAddrWr,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kData,
      kStop,
      kEnd,
  });

  EXPECT_EQ(transfers[3].target_addr, 0x4f);
  transfers[3].ExpectTokenListEq({
      Token::kStart,
      Token::kTargetAddrRd,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kDataLast,
      Token::kEnd,
  });
}

TEST_F(AmlI2cDfv2Test, WriteTransactionTooBig) {
  InitDriver();

  fidl::VectorView<uint8_t> buffer{arena_, 512};
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x00,
       fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
           fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&buffer)),
       true}};

  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  EXPECT_EQ(transact_result->value()->read.count(), 0);

  fidl::VectorView<uint8_t> buffer2{arena_, 513};
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op2 = {
      {0x00,
       fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
           fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&buffer2)),
       true}};

  auto transact_result2 = i2c_.buffer(arena_)->Transact({arena_, op2});
  ASSERT_OK(transact_result.status());
  EXPECT_TRUE(transact_result2->is_error());
}

TEST_F(AmlI2cDfv2Test, ReadTransactionTooBig) {
  InitDriver();

  constexpr uint8_t kReadData[512] = {0};
  controller().SetReadData({kReadData, std::size(kReadData)});

  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op = {
      {0x00, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(512), true}};
  auto transact_result = i2c_.buffer(arena_)->Transact({arena_, op});
  ASSERT_OK(transact_result.status());
  ASSERT_FALSE(transact_result->is_error());
  EXPECT_EQ(transact_result->value()->read.count(), 1);

  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> op2 = {
      {0x00, fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(513), true}};
  auto transact_result2 = i2c_.buffer(arena_)->Transact({arena_, op2});
  ASSERT_OK(transact_result.status());
  EXPECT_TRUE(transact_result2->is_error());
}

TEST_F(AmlI2cDfv2Test, Metadata) {
  constexpr aml_i2c_delay_values kMetadata{.quarter_clock_delay = 0x3cd, .clock_low_delay = 0xf12};

  InitDriver(kMetadata);

  EXPECT_EQ(mmio()[kControlReg / sizeof(uint32_t)], 0x3cd << 12);
  EXPECT_EQ(mmio()[kTargetAddrReg / sizeof(uint32_t)], (0xf12 << 16) | (1 << 28));
}

TEST_F(AmlI2cDfv2Test, NoMetadata) {
  InitDriver(std::nullopt);

  EXPECT_EQ(mmio()[kControlReg / sizeof(uint32_t)], 0);
  EXPECT_EQ(mmio()[kTargetAddrReg / sizeof(uint32_t)], 0);
}

TEST_F(AmlI2cDfv2Test, MmioIrqCountInvalid) { InitDriver(std::nullopt, 2, 2, false); }

}  // namespace aml_i2c
