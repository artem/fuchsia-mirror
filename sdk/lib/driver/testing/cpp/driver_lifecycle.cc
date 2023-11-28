// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/driver_lifecycle.h>

#if __Fuchsia_API_level__ >= 15

namespace fdf_testing {

DriverUnderTestBase::DriverUnderTestBase(DriverRegistration driver_registration_symbol)
    : driver_dispatcher_(fdf::Dispatcher::GetCurrent()->get()),
      checker_(fdf_dispatcher_get_async_dispatcher(driver_dispatcher_)),
      driver_registration_symbol_(driver_registration_symbol) {
  zx::result endpoints = fdf::CreateEndpoints<fuchsia_driver_framework::Driver>();
  ZX_ASSERT_MSG(endpoints.is_ok(), "Failed to create fdf::Driver endpoints: %s",
                endpoints.status_string());
  void* token = driver_registration_symbol_.v1.initialize(endpoints->server.TakeHandle().release());
  driver_client_.Bind(std::move(endpoints->client), driver_dispatcher_, this);
  token_.emplace(token);
}

DriverUnderTestBase::~DriverUnderTestBase() {
  if (token_.has_value()) {
    driver_registration_symbol_.v1.destroy(token_.value());
  }
}

void DriverUnderTestBase::on_fidl_error(fidl::UnbindInfo error) {
  std::lock_guard guard(checker_);
  if (stop_completer_.has_value()) {
    stop_completer_.value().complete_ok(zx::ok());
    stop_completer_.reset();
  }
}

void DriverUnderTestBase::handle_unknown_event(
    fidl::UnknownEventMetadata<fuchsia_driver_framework::Driver> metadata) {}

DriverRuntime::AsyncTask<zx::result<>> DriverUnderTestBase::Start(fdf::DriverStartArgs start_args) {
  std::lock_guard guard(checker_);
  fdf::Arena arena('STRT');
  fpromise::bridge<zx::result<>> bridge;
  driver_client_.buffer(arena)
      ->Start(fidl::ToWire(arena, std::move(start_args)))
      .Then([completer = bridge.completer.bind()](
                fdf::WireUnownedResult<fuchsia_driver_framework::Driver::Start>& result) mutable {
        if (!result.ok()) {
          completer(zx::make_result(result.error().status()));
          return;
        }

        if (result.value().is_error()) {
          completer(result->take_error());
          return;
        }

        completer(zx::ok());
      });

  return DriverRuntime::AsyncTask<zx::result<>>(bridge.consumer.promise());
}

DriverRuntime::AsyncTask<zx::result<>> DriverUnderTestBase::PrepareStop() {
  std::lock_guard guard(checker_);
  fpromise::bridge<zx::result<>> bridge;
  stop_completer_.emplace(std::move(bridge.completer));
  fdf::Arena arena('STOP');
  fidl::OneWayStatus status = driver_client_.buffer(arena)->Stop();
  ZX_ASSERT_MSG(status.ok(), "Failed to send Stop request.");
  return DriverRuntime::AsyncTask<zx::result<>>(bridge.consumer.promise());
}

zx::result<> DriverUnderTestBase::Stop() {
  ZX_ASSERT(token_.has_value());
  driver_registration_symbol_.v1.destroy(token_.value());
  token_.reset();
  return zx::ok();
}

}  // namespace fdf_testing

#endif
