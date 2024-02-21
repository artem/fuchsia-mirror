// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/inspect/cpp/component.h>

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  auto context = sys::ComponentContext::CreateAndServeOutgoingDirectory();
  // This is a test for a deprecated thing (sys::ComponentInspector). We keep the test
  // to not lose coverage of the deprecated type, but silence the build warning.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
  auto inspector = std::make_unique<sys::ComponentInspector>(context.get());
#pragma clang diagnostic pop

  inspector->root().CreateInt("val1", 1, inspector->inspector());
  inspector->root().CreateInt("val2", 2, inspector->inspector());
  inspector->root().CreateInt("val3", 3, inspector->inspector());
  inspector->root().CreateLazyNode(
      "child",
      [] {
        inspect::Inspector insp;
        insp.GetRoot().CreateInt("val", 0, &insp);
        return fpromise::make_ok_promise(std::move(insp));
      },
      inspector->inspector());
  inspector->root().CreateLazyValues(
      "values",
      [] {
        inspect::Inspector insp;
        insp.GetRoot().CreateInt("val4", 4, &insp);
        return fpromise::make_ok_promise(std::move(insp));
      },
      inspector->inspector());

  inspector->Health().Ok();

  loop.Run();
  return 0;
}
