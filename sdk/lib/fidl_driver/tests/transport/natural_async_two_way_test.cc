// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/test.transport/cpp/driver/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/internal.h>
#include <lib/fit/defer.h>
#include <zircon/errors.h>

#include <memory>

#include <zxtest/zxtest.h>

#include "sdk/lib/fidl_driver/tests/transport/scoped_fake_driver.h"
#include "sdk/lib/fidl_driver/tests/transport/server_on_unbound_helper.h"

namespace {

constexpr uint32_t kRequestPayload = 1234;
constexpr uint32_t kResponsePayload = 5678;

// TODO(fxbug.dev/96101) Convert server to natural types.
struct TestServer : public fdf::WireServer<test_transport::TwoWayTest> {
  void TwoWay(TwoWayRequestView request, fdf::Arena& in_request_arena,
              TwoWayCompleter::Sync& completer) override {
    ASSERT_EQ(kRequestPayload, request->payload);

    auto response_arena = fdf::Arena::Create(0, "");
    completer.buffer(*response_arena).Reply(kResponsePayload);
  }
};

TEST(DriverTransport, NaturalTwoWayAsync) {
  fidl_driver_testing::ScopedFakeDriver driver;

  auto dispatcher = fdf::Dispatcher::Create(0);
  ASSERT_OK(dispatcher.status_value());

  auto channels = fdf::ChannelPair::Create(0);
  ASSERT_OK(channels.status_value());

  fdf::ServerEnd<test_transport::TwoWayTest> server_end(std::move(channels->end0));
  fdf::ClientEnd<test_transport::TwoWayTest> client_end(std::move(channels->end1));

  auto server = std::make_shared<TestServer>();
  fdf::BindServer(dispatcher->get(), std::move(server_end), server,
                  fidl_driver_testing::FailTestOnServerError<::test_transport::TwoWayTest>());

  fdf::Client<test_transport::TwoWayTest> client;
  sync_completion_t called;
  auto bind_and_run_on_dispatcher_thread = [&] {
    client.Bind(std::move(client_end), dispatcher->get());

    client->TwoWay(kRequestPayload, [&](fdf::Result<::test_transport::TwoWayTest::TwoWay>& result) {
      ASSERT_TRUE(result.is_ok());
      ASSERT_EQ(kResponsePayload, result->payload());
      sync_completion_signal(&called);
    });
  };
  async::PostTask(dispatcher->async_dispatcher(), bind_and_run_on_dispatcher_thread);
  ASSERT_OK(sync_completion_wait(&called, ZX_TIME_INFINITE));

  sync_completion_t destroyed;
  auto destroy_on_dispatcher_thread = [client = std::move(client), &destroyed] {
    sync_completion_signal(&destroyed);
  };
  async::PostTask(dispatcher->async_dispatcher(), std::move(destroy_on_dispatcher_thread));
  ASSERT_OK(sync_completion_wait(&destroyed, ZX_TIME_INFINITE));
}

TEST(DriverTransport, NaturalTwoWayAsyncShared) {
  fidl_driver_testing::ScopedFakeDriver driver;

  auto dispatcher = fdf::Dispatcher::Create(FDF_DISPATCHER_OPTION_UNSYNCHRONIZED);
  ASSERT_OK(dispatcher.status_value());

  auto channels = fdf::ChannelPair::Create(0);
  ASSERT_OK(channels.status_value());

  fdf::ServerEnd<test_transport::TwoWayTest> server_end(std::move(channels->end0));
  fdf::ClientEnd<test_transport::TwoWayTest> client_end(std::move(channels->end1));

  auto server = std::make_shared<TestServer>();
  fdf::BindServer(dispatcher->get(), std::move(server_end), server,
                  fidl_driver_testing::FailTestOnServerError<::test_transport::TwoWayTest>());

  fdf::SharedClient<test_transport::TwoWayTest> client;
  client.Bind(std::move(client_end), dispatcher->get());

  sync_completion_t done;
  client->TwoWay(kRequestPayload, [&](fdf::Result<::test_transport::TwoWayTest::TwoWay>& result) {
    ASSERT_TRUE(result.is_ok());
    ASSERT_EQ(kResponsePayload, result->payload());
    sync_completion_signal(&done);
  });

  ASSERT_OK(sync_completion_wait(&done, ZX_TIME_INFINITE));
}

}  // namespace
