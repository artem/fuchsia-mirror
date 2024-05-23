// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.tracing.controller/cpp/fidl.h>
#include <fidl/fuchsia.tracing/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/observer.h>
#include <lib/zx/socket.h>
#include <unistd.h>

#include <thread>

#include <perftest/perftest.h>

#include "zircon/system/public/zircon/syscalls.h"

namespace {

void Drain(const zx::unowned_socket& socket) {
  char buffer[4096];
  size_t actual = 0;
  while (zx_status_t result =
             socket->read(0, buffer, sizeof(buffer), &actual) != ZX_ERR_PEER_CLOSED) {
    if (result == ZX_ERR_SHOULD_WAIT) {
      zx_signals_t signals;
      zx_status_t wait_result =
          socket->wait_one(ZX_SOCKET_READABLE, zx::time::infinite(), &signals);
      if (wait_result != ZX_OK) {
        break;
      }
    }
  }
}

template <uint32_t buffer_size>
bool ConfigureMany(perftest::RepeatState* state) {
  state->DeclareStep("setup");
  state->DeclareStep("init_tracing");
  state->DeclareStep("start_tracing");
  state->DeclareStep("stop_tracing");
  state->DeclareStep("terminate_tracing");
  state->DeclareStep("cleanup");

  zx::result client_end = component::Connect<fuchsia_tracing_controller::Controller>();
  if (client_end.is_error()) {
    return false;
  }

  auto client = fidl::SyncClient{std::move(*client_end)};
  // Wait for the tracee to connect if needed.
  for (;;) {
    auto res = client->GetProviders();
    FX_CHECK(res.is_ok());
    if (res.value().providers().size() == 20) {
      break;
    }
    usleep(100000);
  }

  while (state->KeepRunning()) {
    zx::socket in_socket;
    zx::socket outgoing_socket;
    FX_CHECK(zx::socket::create(0u, &in_socket, &outgoing_socket) == ZX_OK);
    std::thread drainer{[socket = in_socket.borrow()]() { Drain(socket); }};

    state->NextStep();
    FX_CHECK(
        client
            ->InitializeTracing({{{
                                     .categories = std::vector<std::string>{"none"},
                                     .buffer_size_megabytes_hint = uint32_t{buffer_size},
                                     .buffering_mode = fuchsia_tracing::BufferingMode::kStreaming,
                                 }},
                                 std::move(outgoing_socket)})
            .is_ok());
    state->NextStep();

    FX_CHECK(client->StartTracing({}).is_ok());
    state->NextStep();

    FX_CHECK(client->StopTracing({{{.write_results = {false}}}}).is_ok());
    state->NextStep();

    FX_CHECK(client->TerminateTracing({{{.write_results = {false}}}}).is_ok());
    state->NextStep();

    drainer.join();
  }

  return true;
}

void RegisterTests() {
  perftest::RegisterTest("Configure/20Providers/8MBBuffers", ConfigureMany<8>);
  perftest::RegisterTest("Configure/20Providers/32MBBuffers", ConfigureMany<32>);
  perftest::RegisterTest("Configure/20Providers/64MBBuffers", ConfigureMany<64>);
}

PERFTEST_CTOR(RegisterTests)
}  // namespace

int main(int argc, char** argv) {
  const char* test_suite = "fuchsia.trace_system";
  return perftest::PerfTestMain(argc, argv, test_suite);
}
