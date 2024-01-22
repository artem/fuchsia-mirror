// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/boot-shim/testing/devicetree-test-fixture.h>
#include <lib/fit/defer.h>
#include <lib/uart/amlogic.h>
#include <lib/zbitl/image.h>

namespace {
using boot_shim::testing::LoadDtb;
using boot_shim::testing::LoadedDtb;

class ChosenNodeMatcherTest
    : public boot_shim::testing::TestMixin<boot_shim::testing::ArmDevicetreeTest,
                                           boot_shim::testing::RiscvDevicetreeTest> {
 public:
  static void SetUpTestSuite() {
    Mixin::SetUpTestSuite();
    auto loaded_dtb = LoadDtb("chosen.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    chosen_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("chosen_unknown_intc.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    chosen_unknown_intc_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("chosen_with_reg_offset.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    chosen_with_reg_offset_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("chosen_with_translation.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    chosen_with_translation_dtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    chosen_dtb_ = std::nullopt;
    chosen_unknown_intc_dtb_ = std::nullopt;
    chosen_with_reg_offset_dtb_ = std::nullopt;
    chosen_with_translation_dtb_ = std::nullopt;
    Mixin::TearDownTestSuite();
  }

  devicetree::Devicetree chosen() { return chosen_dtb_->fdt(); }
  devicetree::Devicetree chosen_with_reg_offset() { return chosen_with_reg_offset_dtb_->fdt(); }
  devicetree::Devicetree chosen_unknown_intc() { return chosen_unknown_intc_dtb_->fdt(); }
  devicetree::Devicetree chosen_with_translation() { return chosen_with_translation_dtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> chosen_dtb_;
  static std::optional<LoadedDtb> chosen_with_reg_offset_dtb_;
  static std::optional<LoadedDtb> chosen_unknown_intc_dtb_;
  static std::optional<LoadedDtb> chosen_with_translation_dtb_;
};

std::optional<LoadedDtb> ChosenNodeMatcherTest::chosen_dtb_ = std::nullopt;
std::optional<LoadedDtb> ChosenNodeMatcherTest::chosen_with_reg_offset_dtb_ = std::nullopt;
std::optional<LoadedDtb> ChosenNodeMatcherTest::chosen_unknown_intc_dtb_ = std::nullopt;
std::optional<LoadedDtb> ChosenNodeMatcherTest::chosen_with_translation_dtb_ = std::nullopt;

struct ExpectedChosen {
  uintptr_t ramdisk_start;
  uintptr_t ramdisk_end;
  std::string_view cmdline;
  std::string_view uart_config_name;
  zbi_dcfg_simple_t uart_config;
  std::string_view uart_absolute_path;
};

using AllUartDrivers =
    std::variant<uart::null::Driver, uart::ns8250::Dw8250Driver, uart::pl011::Driver,
                 uart::ns8250::Mmio8Driver, uart::ns8250::Mmio32Driver, uart::amlogic::Driver>;

template <typename ChosenItemType>
void CheckChosenMatcher(const ChosenItemType& matcher, const ExpectedChosen& expected) {
  std::vector<std::unique_ptr<devicetree::Node>> nodes_in_path;
  size_t current = 0;
  while (current < expected.uart_absolute_path.length()) {
    size_t next = current;
    if (next = expected.uart_absolute_path.substr(current).find('/');
        next == std::string_view::npos) {
      next = expected.uart_absolute_path.length() - current;
    }
    nodes_in_path.push_back(
        std::make_unique<devicetree::Node>(expected.uart_absolute_path.substr(current, next)));
    current += next + 1;
  }

  devicetree::NodePath expected_uart_path;
  auto cleanup = fit::defer([&]() {
    while (!expected_uart_path.is_empty()) {
      expected_uart_path.pop_back();
    }
  });
  for (auto& node : nodes_in_path) {
    expected_uart_path.push_back(node.get());
  }

  // Cmdline Check.
  EXPECT_EQ(matcher.cmdline(), expected.cmdline);

  // Ramdisk captured correctly.
  auto ramdisk = matcher.zbi();
  uintptr_t ramdisk_start = reinterpret_cast<uintptr_t>(ramdisk.data());
  EXPECT_EQ(ramdisk_start, expected.ramdisk_start);
  EXPECT_EQ(ramdisk.size(), expected.ramdisk_end - expected.ramdisk_start);

  ASSERT_TRUE(matcher.stdout_path());
  EXPECT_EQ(*matcher.stdout_path(), expected_uart_path);

  // Uart.
  ASSERT_TRUE(matcher.uart());
  uart::internal::Visit(
      [expected](const auto& uart) {
        using config_t = std::decay_t<decltype(uart.config())>;
        if constexpr (std::is_same_v<config_t, zbi_dcfg_simple_t>) {
          EXPECT_EQ(uart.config_name(), expected.uart_config_name, "Actual name %s\n",
                    uart.config_name().data());
          EXPECT_EQ(uart.config().mmio_phys, expected.uart_config.mmio_phys);
          // The bootstrap phase does not decode interrupt.
          EXPECT_EQ(uart.config().irq, expected.uart_config.irq);
        } else {
          FAIL("Unexpected driver: %s", fbl::TypeInfo<decltype(uart)>::Name());
        }
      },
      *matcher.uart());
}

TEST_F(ChosenNodeMatcherTest, Chosen) {
  auto fdt = chosen();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher,
                     {
                         .ramdisk_start = 0x48000000,
                         .ramdisk_end = 0x58000000,
                         .cmdline = "-foo=bar -bar=baz",
                         .uart_config_name = uart::pl011::Driver::config_name(),
                         .uart_config =
                             {
                                 .mmio_phys = 0x9000000,
                                 .irq = 33,
                             },
                         .uart_absolute_path = "/some-interrupt-controller/pl011uart@9000000",
                     });
}

TEST_F(ChosenNodeMatcherTest, ChosenWithRegOffset) {
  auto fdt = chosen_with_reg_offset();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher,
                     {
                         .ramdisk_start = 0x48000000,
                         .ramdisk_end = 0x58000000,
                         .cmdline = "-foo=bar -bar=baz",
                         .uart_config_name = uart::pl011::Driver::config_name(),
                         .uart_config =
                             {
                                 .mmio_phys = 0x9000123,
                                 .irq = 33,
                             },
                         .uart_absolute_path = "/some-interrupt-controller/pl011uart@9000000",
                     });
}

TEST_F(ChosenNodeMatcherTest, ChosenWithAddressTranslation) {
  auto fdt = chosen_with_translation();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher,
                     {
                         .ramdisk_start = 0x48000000,
                         .ramdisk_end = 0x58000000,
                         .cmdline = "-foo=bar -bar=baz",
                         .uart_config_name = uart::pl011::Driver::config_name(),
                         .uart_config =
                             {
                                 .mmio_phys = 0x9030000,
                                 .irq = 33,
                             },
                         .uart_absolute_path = "/some-interrupt-controller@0/pl011uart@9000000",
                     });
}

TEST_F(ChosenNodeMatcherTest, ChosenWithUnknownInterruptController) {
  auto fdt = chosen_unknown_intc();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));
  CheckChosenMatcher(chosen_matcher,
                     {
                         .ramdisk_start = 0x48000000,
                         .ramdisk_end = 0x58000000,
                         .cmdline = "-foo=bar -bar=baz",
                         .uart_config_name = uart::pl011::Driver::config_name(),
                         .uart_config =
                             {
                                 .mmio_phys = 0x9000000,
                                 .irq = 0,
                             },
                         .uart_absolute_path = "/some-interrupt-controller/pl011uart@9000000",
                     });
}

TEST_F(ChosenNodeMatcherTest, CrosvmArm) {
  constexpr std::string_view kCmdline =
      "panic=-1 kernel.experimental.serial_migration=true console.shell=true "
      "zircon.autorun.boot=/boot/bin/devicetree-extract";

  auto fdt = crosvm_arm();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher,
                     {
                         .ramdisk_start = 0x81000000,
                         .ramdisk_end = 0x82bd4e28,
                         .cmdline = kCmdline,
                         .uart_config_name = uart::ns8250::Mmio8Driver::config_name(),
                         .uart_config =
                             {
                                 .mmio_phys = 0x3F8,
                                 .irq = 32,
                             },
                         .uart_absolute_path = "/U6_16550A@3f8",
                     });
}

TEST_F(ChosenNodeMatcherTest, QemuArm) {
  constexpr std::string_view kQemuCmdline =
      "TERM=xterm-256color "
      "kernel.entropy-mixin=cd93b8955fc588b1bcde0d691a694b926d53faeca61c386635739b24df717363 "
      "kernel.halt-on-panic=true ";
  constexpr uint32_t kQemuRamdiskStart = 0x48000000;
  constexpr uint32_t kQemuRamdiskEnd = 0x499e8458;

  auto fdt = qemu_arm_gic3();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher, {
                                         .ramdisk_start = kQemuRamdiskStart,
                                         .ramdisk_end = kQemuRamdiskEnd,
                                         .cmdline = kQemuCmdline,
                                         .uart_config_name = uart::pl011::Driver::config_name(),
                                         .uart_config =
                                             {
                                                 .mmio_phys = uart::pl011::kQemuConfig.mmio_phys,
                                                 .irq = 33,
                                             },
                                         .uart_absolute_path = "/pl011@9000000",
                                     });
}

TEST_F(ChosenNodeMatcherTest, QemuRiscv) {
  constexpr std::string_view kQemuCmdline =
      "BOOT_IMAGE=/vmlinuz-5.19.0-1012-generic root=/dev/mapper/ubuntu--vg-ubuntu--lv ro";
  constexpr uint32_t kQemuRamdiskStart = 0xD646A000;
  constexpr uint32_t kQemuRamdiskEnd = 0xDAFEFDB6;

  auto fdt = qemu_riscv();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher,
                     {
                         .ramdisk_start = kQemuRamdiskStart,
                         .ramdisk_end = kQemuRamdiskEnd,
                         .cmdline = kQemuCmdline,
                         .uart_config_name = uart::ns8250::Mmio8Driver::config_name(),
                         .uart_config =
                             {
                                 .mmio_phys = 0x10000000,
                                 .irq = 10,
                             },
                         .uart_absolute_path = "/soc/serial@10000000",
                     });
}

TEST_F(ChosenNodeMatcherTest, VisionFive2) {
  constexpr std::string_view kCmdline =
      "root=/dev/mmcblk1p4 rw console=tty0 console=ttyS0,115200 earlycon rootwait "
      "stmmaceth=chain_mode:1 selinux=0";

  auto fdt = vision_five_2();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher,
                     {
                         .ramdisk_start = 0x48100000,
                         .ramdisk_end = 0x48fb3df5,
                         .cmdline = kCmdline,
                         .uart_config_name = uart::ns8250::Dw8250Driver::config_name(),
                         .uart_config =
                             {
                                 .mmio_phys = 0x10000000,
                                 .irq = 32,
                             },
                         .uart_absolute_path = "/soc/serial@10000000",
                     });
}

TEST_F(ChosenNodeMatcherTest, KhadasVim3) {
  constexpr std::string_view kCmdline =
      " androidboot.verifiedbootstate=orange androidboot.dtbo_idx=3   "
      " androidboot.serialno=06ECB1E62CB2  no_console_suspend console=ttyAML0,115200 earlycon"
      " printk.devkmsg=on androidboot.boot_devices=soc/ffe07000.mmc init=/init"
      " firmware_class.path=/vendor/firmware androidboot.hardware=yukawa"
      " androidboot.selinux=permissive";

  auto fdt = khadas_vim3();
  boot_shim::DevicetreeChosenNodeMatcher<AllUartDrivers> chosen_matcher("test", stdout);

  ASSERT_TRUE(devicetree::Match(fdt, chosen_matcher));

  CheckChosenMatcher(chosen_matcher, {
                                         .ramdisk_start = 0x7fe4d000,
                                         .ramdisk_end = 0x7ffff5d7,
                                         .cmdline = kCmdline,
                                         .uart_config_name = uart::amlogic::Driver::config_name(),
                                         .uart_config =
                                             {
                                                 .mmio_phys = 0xff803000,
                                                 .irq = 225,
                                             },
                                         .uart_absolute_path = "/soc/bus@ff800000/serial@3000",
                                     });
}

}  // namespace
