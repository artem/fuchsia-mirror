// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_BUTTONS_BUTTONS_DEVICE_H_
#define SRC_UI_INPUT_DRIVERS_BUTTONS_BUTTONS_DEVICE_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/input_report_reader/reader.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/port.h>
#include <lib/zx/timer.h>
#include <zircon/threads.h>

#include <ddk/metadata/buttons.h>
#include <fbl/array.h>

namespace buttons {

// zx_port_packet::key.
constexpr uint64_t kPortKeyShutDown = 0x01;
// Start of up to kNumberOfRequiredGpios port types used for interrupts.
constexpr uint64_t kPortKeyInterruptStart = 0x10;
// Timer start
constexpr uint64_t kPortKeyTimerStart = 0x100;
// Poll timer
constexpr uint64_t kPortKeyPollTimer = 0x1000;
// Debounce threshold.
constexpr uint64_t kDebounceThresholdNs = 50'000'000;

class ButtonsDevice : public fidl::WireServer<fuchsia_input_report::InputDevice> {
 public:
  struct Gpio {
    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> client;
    zx::interrupt irq;
    buttons_gpio_config_t config;
  };

  explicit ButtonsDevice(async_dispatcher_t* dispatcher,
                         fbl::Array<buttons_button_config_t> buttons, fbl::Array<Gpio> gpios);
  void Notify(size_t button_index);
  void ShutDown();

  // fuchsia_input_report::InputDevice required methods
  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override;
  void GetDescriptor(GetDescriptorCompleter::Sync& completer) override;
  void SendOutputReport(SendOutputReportRequestView request,
                        SendOutputReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetFeatureReport(GetFeatureReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void SetFeatureReport(SetFeatureReportRequestView request,
                        SetFeatureReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetInputReport(GetInputReportRequestView request,
                      GetInputReportCompleter::Sync& completer) override;

 private:
  zx::port port_;
  friend class ButtonsDeviceTest;
  static constexpr size_t kFeatureAndDescriptorBufferSize = 512;

  int Thread();
  zx_status_t Init();
  zx::result<uint8_t> ReconfigurePolarity(uint32_t idx, uint64_t int_port);
  zx_status_t ConfigureInterrupt(uint32_t idx, uint64_t int_port);
  zx::result<bool> MatrixScan(uint32_t row, uint32_t col, zx_duration_t delay);

  struct ButtonsInputReport {
    zx::time event_time = zx::time(ZX_TIME_INFINITE_PAST);
    std::array<bool, BUTTONS_ID_MAX> buttons = {};

    void ToFidlInputReport(
        fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
        fidl::AnyArena& allocator);

    bool operator==(const ButtonsInputReport& other) const { return buttons == other.buttons; }
    bool operator!=(const ButtonsInputReport& other) const { return !(*this == other); }

    void set(uint32_t button_id, bool pressed) {
      if (button_id >= buttons.size()) {
        return;
      }
      buttons[button_id] = pressed;
    }
    bool empty() const {
      return std::all_of(buttons.cbegin(), buttons.cend(), [](bool i) { return !i; });
    }
  };
  zx::result<ButtonsInputReport> GetInputReportInternal();

  async_dispatcher_t* dispatcher_;

  thrd_t thread_;
  libsync::Completion thread_started_;
  input_report_reader::InputReportReaderManager<ButtonsInputReport> readers_;
  fbl::Array<buttons_button_config_t> buttons_;
  fbl::Array<Gpio> gpios_;

  struct debounce_state {
    bool enqueued;
    zx::timer timer;
    bool value;
    zx::time timestamp = zx::time::infinite_past();
  };
  fbl::Array<debounce_state> debounce_states_;
  // last_report_ saved to de-duplicate reports
  std::optional<ButtonsInputReport> last_report_ = std::nullopt;

  zx::duration poll_period_{zx::duration::infinite()};
  zx::timer poll_timer_;

  inspect::Inspector inspector_;
  inspect::Node metrics_root_;
  // Note that because this driver handles both polling and IRQ reports, latency is only measured
  // for IRQ reports because it is not meaningful for polling.
  inspect::UintProperty average_latency_usecs_;
  inspect::UintProperty max_latency_usecs_;
  // However, total_report_count_ and last_event_timestamp_ will reflect both polling and IRQ
  // reports.
  inspect::UintProperty total_report_count_;
  inspect::UintProperty last_event_timestamp_;

  uint64_t report_count_ = 0;
  zx::duration total_latency_ = {};
  zx::duration max_latency_ = {};
};

}  // namespace buttons

#endif  // SRC_UI_INPUT_DRIVERS_BUTTONS_BUTTONS_DEVICE_H_
