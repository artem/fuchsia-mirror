// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.feedback/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/syslog/cpp/macros.h>

#include <new>

// 512MB structure.
struct BigStruct {
  int y[128 * 1024 * 1024];
};

extern "C" int cpp_out_of_mem() {
  int rv = 0;
  for (int ix = 0; ix < 1000; ++ix) {
    auto big = new BigStruct;
    rv += (big->y > &rv) ? 0 : 1;
  }
  return rv;
}

extern "C" int llcpp_channel_overflow() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  // The protocol used doesn't matter, the server endpoint of the channel isn't sent to the
  // component implementing the protocol and stays in the crasher process.
  auto endpoints = fidl::CreateEndpoints<fuchsia_feedback::CrashReporter>();
  fidl::WireClient<fuchsia_feedback::CrashReporter> client(std::move(endpoints->client),
                                                           loop.dispatcher());
  while (true) {
    client->FileReport(fuchsia_feedback::wire::CrashReport())
        .ThenExactlyOnce(
            [](fidl::WireUnownedResult<fuchsia_feedback::CrashReporter::FileReport> &) {});
  }

  loop.Run();

  return 0;
}

extern "C" int cpp_log_fatal() {
  FX_LOGS(FATAL) << "Logging fatal";
  return 0;
}
