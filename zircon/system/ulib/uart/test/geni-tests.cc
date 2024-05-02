// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/uart/geni.h>
#include <lib/uart/mock.h>
#include <lib/uart/uart.h>

#include <cstdint>

#include <zxtest/zxtest.h>

namespace {

using SimpleTestDriver =
    uart::KernelDriver<uart::geni::Driver, uart::mock::IoProvider, uart::UnsynchronizedPolicy>;
constexpr zbi_dcfg_simple_t kTestConfig = {};

// Helper for initializing the driver.
void Init(SimpleTestDriver& driver) {
  driver.io()
      .mock()
      // fifo_width = 32 bits, fifo_depth = 16, fifo_enabled=1
      .ExpectRead(uint32_t{0b0010'0000'0001'0000'0000'1000'0000'0000}, 0xe24)  // TX Hardware Params
      // fifo_width = 32 bits, fifo_depth = 16, fifo_enabled=1
      .ExpectRead(uint32_t{0b0010'0000'0001'0000'0000'1000'0000'0000}, 0xe28)  // RX Hardware Params

      .ExpectWrite(uint32_t{0b0000'0000'0100'0001}, 0x48)  // Enable clock< div=4
      .ExpectWrite(uint32_t{0b0000'0000'0100'0001}, 0x4c)  // Enable clock< div=4

      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0010}, 0x814)   // RX Watermark = 2
      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0100}, 0x80c);  // TX Watermark = 4

  driver.Init();
  driver.io().mock().VerifyAndClear();
}

TEST(GeniTests, HelloWorld) {
  SimpleTestDriver driver(kTestConfig);

  Init(driver);

  driver.io()
      .mock()
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x40)    // !busy
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x800)   // free
      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0100}, 0x270)  // len=4
      .ExpectWrite(uint32_t{0b0000'1000'0000'0000'0000'0000'0000'0000}, 0x600)  // start_tx
      .ExpectWrite(uint32_t{0x0A0D6968}, 0x700);                                // Write

  EXPECT_EQ(3, driver.Write("hi\n"));
}

TEST(GeniTests, HelloWorldBusy) {
  SimpleTestDriver driver(kTestConfig);

  Init(driver);

  driver.io()
      .mock()
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0001}, 0x40)    // busy
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0001}, 0x40)    // busy
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x40)    // !busy
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x800)   // free
      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0100}, 0x270)  // len=4
      .ExpectWrite(uint32_t{0b0000'1000'0000'0000'0000'0000'0000'0000}, 0x600)  // start_tx
      .ExpectWrite(uint32_t{0x0A0D6968}, 0x700);                                // Write

  EXPECT_EQ(3, driver.Write("hi\n"));
}

TEST(GeniTests, Read) {
  SimpleTestDriver driver(kTestConfig);

  Init(driver);

  driver.io()
      .mock()
      // RxFifoStatusReg != 0
      // RxFifoReg
      // partial, 1 byte, 1 word
      .ExpectRead(uint32_t{0b1001'0000'0000'0000'0000'0000'0000'0001}, 0x804)
      .ExpectRead(uint32_t{'q'}, 0x780)  // Read (data)
      .ExpectRead(uint32_t{0b1001'0000'0000'0000'0000'0000'0000'0001}, 0x804)
      .ExpectRead(uint32_t{'\r'}, 0x780);  // Read (data)

  EXPECT_EQ(uint8_t{'q'}, driver.Read());
  EXPECT_EQ(uint8_t{'\r'}, driver.Read());
}

TEST(GeniTests, InitInterrupt) {
  SimpleTestDriver driver(kTestConfig);

  Init(driver);

  driver.io()
      .mock()
      // Disable all interrupt conditions for both engines
      .ExpectWrite(uint32_t{0b1111'1111'1111'1111'1111'1111'1111'1111}, 0x620)
      .ExpectWrite(uint32_t{0b1111'1111'1111'1111'1111'1111'1111'1111}, 0x650)
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0011'0000}, 0x61c)   // main irq enable
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0011'0000}, 0x64c);  // main irq enable

  bool unmasked_irq = false;
  driver.InitInterrupt([&unmasked_irq]() { unmasked_irq = true; });
  EXPECT_TRUE(unmasked_irq);
}

void InitWithInterrupt(SimpleTestDriver& driver) {
  Init(driver);

  driver.io()
      .mock()
      .ExpectWrite(uint32_t{0b1111'1111'1111'1111'1111'1111'1111'1111}, 0x620)
      .ExpectWrite(uint32_t{0b1111'1111'1111'1111'1111'1111'1111'1111}, 0x650)
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0011'0000}, 0x61c)   // main irq enable
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0011'0000}, 0x64c);  // main irq enable

  driver.InitInterrupt([]() {});
  driver.io().mock().VerifyAndClear();
}

TEST(GeniTests, TxIrqOnly) {
  SimpleTestDriver driver(kTestConfig);

  InitWithInterrupt(driver);

  driver.io()
      .mock()
      // Set tx_fifo_watermark on main
      .ExpectRead(uint32_t{0b0100'0000'0000'0000'0000'0000'0000'0000}, 0x610)
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x640)
      // Clear set status
      .ExpectWrite(uint32_t{0b0100'0000'0000'0000'0000'0000'0000'0000}, 0x618)
      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x648)
      // Mask the tx fifo irq
      .ExpectWrite(uint32_t{0b0100'0000'0000'0000'0000'0000'0000'0000}, 0x620);

  int call_count = 0;
  driver.Interrupt(
      [&](auto& sync, auto& waiter, auto&& disable_tx_irq) {
        call_count++;
        disable_tx_irq();
      },
      [](auto& sync, auto&& reader, auto&& full) {
        FAIL("Unexpected call on |rx| irq callback.");
      });

  EXPECT_EQ(call_count, 1);
}
TEST(GeniTests, RxIrqEmptyFifo) {
  SimpleTestDriver driver(kTestConfig);

  Init(driver);

  // Now actual IRQ Handler expectations.
  driver.io()
      .mock()
      // irq status valid but empty fifo
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x610)
      .ExpectRead(uint32_t{0b0000'1000'0000'0000'0000'0000'0000'0000}, 0x640)
      // Clear what was read on both
      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x618)
      .ExpectWrite(uint32_t{0b0000'1000'0000'0000'0000'0000'0000'0000}, 0x648)
      // Read from the fifo status register - 0 bytes
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x804);

  // Empty Fifo bit is set, so it should just return.

  int call_count = 0;
  driver.Interrupt([](auto& sync, auto& waiter,
                      auto&& disable_tx_irq) { FAIL("Unexpected call on |tx| irq callback."); },
                   [&](auto& sync, auto&& reader, auto&& full) { call_count++; });

  driver.io().mock().VerifyAndClear();
  EXPECT_EQ(call_count, 0);
}

TEST(GeniTests, RxTimeoutIrqWithNonEmptyFifoAndNonFullQueue) {
  SimpleTestDriver driver(kTestConfig);

  InitWithInterrupt(driver);

  // Now actual IRQ Handler expectations.
  driver.io()
      .mock()
      // Check last and rx watermark.
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x610)
      .ExpectRead(uint32_t{0b0000'0100'0000'0000'0000'0000'0000'0000}, 0x640)
      // Clear what was read on both
      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x618)
      .ExpectWrite(uint32_t{0b0000'0100'0000'0000'0000'0000'0000'0000}, 0x648)
      // Read from the fifo status register - 1 words * word_width (4)
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0001}, 0x804)
      // Read from the fifo
      .ExpectRead(uint32_t{0b0100'0001'0100'0001'0100'0001'0100'0001}, 0x780);

  int call_count = 0;
  driver.Interrupt([](auto& sync, auto& waiter,
                      auto&& disable_tx_irq) { FAIL("Unexpected call on |tx| irq callback."); },
                   [&](auto& sync, auto&& reader, auto&& full) {
                     call_count++;
                     auto c = reader();
                     EXPECT_EQ('A', c);
                   });

  EXPECT_EQ(4, call_count);
}

TEST(GeniTests, RxIrqWithNonEmptyFifoAndFullQueue) {
  SimpleTestDriver driver(kTestConfig);

  InitWithInterrupt(driver);

  // Now actual IRQ Handler expectations.
  driver.io()
      .mock()
      // Check last and rx watermark.
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x610)
      .ExpectRead(uint32_t{0b0000'1100'0000'0000'0000'0000'0000'0000}, 0x640)
      // Clear what was read on both
      .ExpectWrite(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0000}, 0x618)
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0000'0000}, 0x648)
      // Read from the fifo status register - 1 words * word_width (4)
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0001}, 0x804)
      // Read fifo once before the call below stops it.
      .ExpectRead(uint32_t{0b0000'0000'0000'0000'0000'0000'0000'0100}, 0x780)
      // Now disable RX Interrupts
      // Disable on both engines
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0000'0000}, 0x620)
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0000'0000}, 0x650)
      // Clear from status
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0000'0000}, 0x618)
      .ExpectWrite(uint32_t{0b0000'1100'0000'0000'0000'0000'0000'0000}, 0x648);

  int call_count = 0;
  driver.Interrupt([](auto& sync, auto& waiter,
                      auto&& disable_tx_irq) { FAIL("Unexpected call on |tx| irq callback."); },
                   [&](auto& sync, auto&& reader, auto&& full) {
                     full();
                     call_count++;
                   });

  EXPECT_EQ(call_count, 1);
}

}  // namespace
