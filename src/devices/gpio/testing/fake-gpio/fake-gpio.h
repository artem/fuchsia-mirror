// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_TESTING_FAKE_GPIO_FAKE_GPIO_H_
#define SRC_DEVICES_GPIO_TESTING_FAKE_GPIO_FAKE_GPIO_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire_test_base.h>
#include <lib/zx/interrupt.h>

#include <queue>
#include <vector>

namespace fake_gpio {

// Contains information specific to when a GPIO has been configured for
// output.
struct WriteSubState {
  // Value that the GPIO has been set to output.
  uint8_t value;

  bool operator==(const WriteSubState& other) const;
};

// Contains information specific to when a GPIO has been configured for input.
struct ReadSubState {
  fuchsia_hardware_gpio::GpioFlags flags;

  bool operator==(const ReadSubState& other) const;
};

// Contains information specific to when a GPIO has been configured to perform
// an alternative function.
struct AltFunctionSubState {
  uint64_t function;

  bool operator==(const AltFunctionSubState& other) const;
};

using SubState = std::variant<WriteSubState, ReadSubState, AltFunctionSubState>;

template <typename T>
bool operator==(const T& sub_state1, const SubState& sub_state2) {
  const T* alternative = std::get_if<T>(&sub_state2);
  return alternative != nullptr && sub_state1 == *alternative;
}

template <typename T>
bool operator==(const SubState& sub_state1, const T& sub_state2) {
  return sub_state2 == sub_state1;
}

struct State {
  fuchsia_hardware_gpio::GpioPolarity polarity;
  SubState sub_state;
};

class FakeGpio;

using ReadCallback = std::function<zx::result<uint8_t>(FakeGpio&)>;

using WriteCallback = std::function<zx_status_t(FakeGpio&)>;

class FakeGpio : public fidl::testing::WireTestBase<fuchsia_hardware_gpio::Gpio> {
 public:
  FakeGpio();

  // fidl::testing::WireTestBase<fuchsia_hardware_gpu::Gpio>
  void GetInterrupt(GetInterruptRequestView request,
                    GetInterruptCompleter::Sync& completer) override;
  void SetAltFunction(SetAltFunctionRequestView request,
                      SetAltFunctionCompleter::Sync& completer) override;
  void ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) override;
  void ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) override;
  void Write(WriteRequestView request, WriteCompleter::Sync& completer) override;
  void Read(ReadCompleter::Sync& completer) override;
  void ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) override;
  void SetPolarity(SetPolarityRequestView request, SetPolarityCompleter::Sync& completer) override;
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  // Return the function set by `SetAltFunction`. Will fail if the current
  // state isn't `AltFunction.`
  uint64_t GetAltFunction() const;

  // Return the value being written by the gpio. Will fail if the current state
  // isn't `Write`.
  uint8_t GetWriteValue() const;

  // Return the read flags set by `ConfigIn`. Will fail if the current state
  // isn't `Read`.
  fuchsia_hardware_gpio::GpioFlags GetReadFlags() const;

  // Return the most recent polarity set by `SetPolarity`. Will fail if there
  // is not a current state.
  fuchsia_hardware_gpio::GpioPolarity GetPolarity() const;

  // Set the interrupt used for GetInterrupt requests to `interrupt`.
  void SetInterrupt(zx::result<zx::interrupt> interrupt);

  // Add `callback` to the queue of callbacks used to handle `Read` requests.
  void PushReadCallback(ReadCallback callback);

  // Add a callback that will return `response` to the queue of callbacks used
  // to handle `Read` requests.
  void PushReadResponse(zx::result<uint8_t> response);

  // Set the default response for `Read` requests if the callback queue for
  // `Read` requests is empty. Set to none for no default response.
  void SetDefaultReadResponse(std::optional<zx::result<uint8_t>> response);

  // Set the callback used for responding to Write requests to `write_callback`.
  void SetWriteCallback(WriteCallback write_callback);

  // Set the current state to `state`.
  void SetCurrentState(State state);

  // Return the states the gpio has been set to in chronological order.
  std::vector<State> GetStateLog();

  // Serve the gpio FIDL protocol on the current dispatcher and return a client
  // end that can communicate with the server.
  fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> Connect();

  // Returns a handler that binds incoming gpio service connections to this
  // server implementation and the dispatcher of the caller.
  fuchsia_hardware_gpio::Service::InstanceHandler CreateInstanceHandler();

 private:
  // Returns the polarity of the current state. Returns high if there isn't a
  // current state.
  fuchsia_hardware_gpio::GpioPolarity GetCurrentPolarity();

  // Contains the states that the gpio has been set to in chronological order.
  std::vector<State> state_log_;

  // Default response for `Read` requests if `read_callbacks_` is empty.
  std::optional<zx::result<uint8_t>> default_read_response_;

  // Queue of callbacks that provide values to respond to `Read` requests with.
  std::queue<ReadCallback> read_callbacks_;

  // Callback that provides the value to respond to `Write` requests with.
  WriteCallback write_callback_;

  // Interrupt used for GetInterrupt requests.
  zx::result<zx::interrupt> interrupt_;

  fidl::ServerBindingGroup<fuchsia_hardware_gpio::Gpio> bindings_;
};

}  // namespace fake_gpio

#endif  // SRC_DEVICES_GPIO_TESTING_FAKE_GPIO_FAKE_GPIO_H_
