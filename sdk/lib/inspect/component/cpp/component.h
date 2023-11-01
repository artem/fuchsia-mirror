// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_INSPECT_COMPONENT_CPP_COMPONENT_H_
#define LIB_INSPECT_COMPONENT_CPP_COMPONENT_H_

#include <fidl/fuchsia.inspect/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/inspect/component/cpp/tree_handler_settings.h>
#include <lib/inspect/cpp/health.h>
#include <lib/inspect/cpp/inspect.h>

#include <string>

namespace inspect {
#if __Fuchsia_API_level__ >= 16
// Options for a published `ComponentInspector`.
//
// The default constructor is acceptable for many components, and will cause a default
// Inspector to be published via `fuchsia.inspect.InspectSink`, connected via the component's
// default namespace, using default `TreeHandlerSettings`.
struct PublishOptions final {
  // Optionally specify an existing `Inspector`.
  Inspector inspector = {};

  // Specify how `fuchsia.inspect.Tree` should behave.
  TreeHandlerSettings tree_handler_settings = {};

  // Provide a name to appear in the tree's Inspect Metadata. By default, it will be empty.
  std::optional<std::string> tree_name = {};

  // Provide a fidl::ClientEnd, useful if `fuchsia.inspect.InspectSink` is not in the root
  // namespace or is renamed.
  std::optional<fidl::ClientEnd<fuchsia_inspect::InspectSink>> client_end = {};
};
#endif

// ComponentInspector is an instance of an Inspector that
// serves its Inspect data via the fuchsia.inspect.Tree protocol.
//
// Example:
//
// ```
// #include <lib/async-loop/cpp/loop.h>
// #include <lib/async-loop/default.h>
// #include <lib/inspect/component/cpp/component.h>
//
// int main() {
//   using inspect::ComponentInspector;
//
//   async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
//   auto* dispatcher = loop.dispatcher();
//   auto inspector = ComponentInspector(dispatcher, {});
//
//   inspector.root().RecordInt("val1", 1);
//
//   inspector.Health().Ok();
//
//   loop.Run();
//   return 0;
// }
// ```
class ComponentInspector final {
 public:
#if __Fuchsia_API_level__ >= 16
  // Construct a `ComponentInspector` with the provided `PublishOptions`.
  // This `ComponentInspector` will be published via `fuchsia.inspect.InspectSink`.
  ComponentInspector(async_dispatcher_t* dispatcher, PublishOptions opts);
#else
  // Construct a ComponentInspector and host it on the given outgoing directory.
  //
  // Note that it is the caller's responsibility to ensure the outgoing directory is served.
  ComponentInspector(component::OutgoingDirectory& outgoing_directory,
                     async_dispatcher_t* dispatcher, Inspector inspector = {},
                     TreeHandlerSettings settings = {});
#endif  // __Fuchsia_API_level__

  ComponentInspector(ComponentInspector&&) = default;

  // Get the Inspector's root node.
  Node& root() { return inspector_.GetRoot(); }

  // Get the wrapped Inspector.
  Inspector& inspector() { return inspector_; }

  // Gets the NodeHealth for this component.
  // This method is not thread safe.
  NodeHealth& Health();

 private:
  ComponentInspector() = delete;
  ComponentInspector(const ComponentInspector&) = delete;

  Inspector inspector_;
  std::unique_ptr<NodeHealth> component_health_;
};
}  // namespace inspect

#endif  // LIB_INSPECT_COMPONENT_CPP_COMPONENT_H_
