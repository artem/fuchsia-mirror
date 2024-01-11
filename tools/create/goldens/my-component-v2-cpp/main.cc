// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/syslog/cpp/macros.h>

#include "tools/create/goldens/my-component-v2-cpp/my_component_v2_cpp.h"

int main(int argc, const char** argv) {
  // Create the main async event loop.
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  // Create an instance of the application state.
  my_component_v2_cpp::App app(loop.dispatcher());

  auto component_context = sys::ComponentContext::CreateAndServeOutgoingDirectory();

  // Initialize inspect
  inspect::ComponentInspector inspector(loop.dispatcher(), inspect::PublishOptions{});
  inspector.Health().StartingUp();

  // Serve a protocol using:
  // component_context->outgoing()->AddPublicService<MyProtocol>(..);

  inspector.Health().Ok();
  FX_LOGS(DEBUG) << "Initialized.";

  // Run the loop until it is shutdown.
  loop.Run();
  return 0;
}
