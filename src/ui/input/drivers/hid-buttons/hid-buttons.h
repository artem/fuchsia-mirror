// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_HID_BUTTONS_HID_BUTTONS_H_
#define SRC_UI_INPUT_DRIVERS_HID_BUTTONS_HID_BUTTONS_H_

#include <fidl/fuchsia.buttons/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/input_report_reader/reader.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/port.h>
#include <lib/zx/timer.h>

#include <list>
#include <map>
#include <optional>
#include <set>

#include <ddk/metadata/buttons.h>
#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/array.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted.h>

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

class HidButtonsDevice;
using DeviceType =
    ddk::Device<HidButtonsDevice, ddk::Messageable<fuchsia_input_report::InputDevice>::Mixin,
                ddk::Unbindable>;
class ButtonsNotifyInterface;

using Buttons = fuchsia_buttons::Buttons;
using ButtonType = fuchsia_buttons::wire::ButtonType;

class HidButtonsDevice : public DeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_INPUTREPORT> {
 public:
  struct Gpio {
    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> client;
    zx::interrupt irq;
    buttons_gpio_config_t config;
  };

  explicit HidButtonsDevice(zx_device_t* device, async_dispatcher_t* dispatcher)
      : DeviceType(device), dispatcher_(dispatcher) {
    metrics_root_ = inspector_.GetRoot().CreateChild("hid-input-report-touch");
    average_latency_usecs_ = metrics_root_.CreateUint("average_latency_usecs", 0);
    max_latency_usecs_ = metrics_root_.CreateUint("max_latency_usecs", 0);
    total_report_count_ = metrics_root_.CreateUint("total_report_count", 0);
    last_event_timestamp_ = metrics_root_.CreateUint("last_event_timestamp", 0);
  }
  virtual ~HidButtonsDevice() = default;

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

  // FIDL Interface Functions.
  bool GetState(ButtonType type);
  zx_status_t RegisterNotify(uint8_t types, ButtonsNotifyInterface* notify);

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  zx_status_t Bind(fbl::Array<Gpio> gpios, fbl::Array<buttons_button_config_t> buttons);
  virtual void ClosingChannel(ButtonsNotifyInterface* notify);
  virtual void Notify(uint32_t button_index);

 protected:
  // Protected for unit testing.
  void ShutDown();

  zx::port port_;

  fbl::Mutex channels_lock_;
  // A map of ButtonTypes to the interfaces that have to be notified when they are pressed.
  std::map<ButtonType, std::set<ButtonsNotifyInterface*>> registered_notifiers_
      TA_GUARDED(channels_lock_);
  // A map of ButtonType values to an index into the buttons_ array.
  std::map<ButtonType, uint32_t> button_map_;

  std::list<ButtonsNotifyInterface> interfaces_ TA_GUARDED(channels_lock_);  // owns the channels

 private:
  friend class HidButtonsDeviceTest;
  static constexpr size_t kFeatureAndDescriptorBufferSize = 512;

  int Thread();
  uint8_t ReconfigurePolarity(uint32_t idx, uint64_t int_port);
  zx_status_t ConfigureInterrupt(uint32_t idx, uint64_t int_port);
  bool MatrixScan(uint32_t row, uint32_t col, zx_duration_t delay);

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
  zx::result<ButtonsInputReport> GetInputReport();

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
  ButtonsInputReport last_report_;

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

class ButtonsNotifyInterface : public fidl::WireServer<Buttons> {
 public:
  explicit ButtonsNotifyInterface(HidButtonsDevice* peripheral) : device_(peripheral) {}
  ~ButtonsNotifyInterface() override = default;

  const fidl::ServerBindingRef<Buttons>& binding() { return *binding_; }

  // Methods required by the FIDL interface
  void GetState(GetStateRequestView request, GetStateCompleter::Sync& completer) override {
    completer.Reply(device_->GetState(request->type));
  }
  void RegisterNotify(RegisterNotifyRequestView request,
                      RegisterNotifyCompleter::Sync& completer) override {
    completer.Reply(zx::make_result(device_->RegisterNotify(request->types, this)));
  }

 private:
  HidButtonsDevice* device_;
  std::optional<fidl::ServerBindingRef<Buttons>> binding_;
};

}  // namespace buttons

#endif  // SRC_UI_INPUT_DRIVERS_HID_BUTTONS_HID_BUTTONS_H_
