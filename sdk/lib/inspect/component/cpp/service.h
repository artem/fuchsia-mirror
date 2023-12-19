// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_INSPECT_COMPONENT_CPP_SERVICE_H_
#define LIB_INSPECT_COMPONENT_CPP_SERVICE_H_

#include <fidl/fuchsia.inspect/cpp/wire.h>
#include <lib/async/cpp/executor.h>
#include <lib/inspect/component/cpp/tree_handler_settings.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/stdcompat/optional.h>

#include <functional>
#include <memory>

namespace inspect {

/// TreeServer is an implementation of the fuchsia.inspect.Tree protocol.
///
/// Generally, it is not necessary to use this directly. See `inspect::ComponentInspector`.
///
/// This class can be used directly if the client wishes to manage protocol registration
/// details manually.
///
/// For an example of usage, see the constructor for `inspect::ComponentInspector`.
class TreeServer final : public fidl::WireServer<fuchsia_inspect::Tree> {
 public:
  /// Starts a new server. The implementation deletes itself during teardown after an unbind.
  static void StartSelfManagedServer(Inspector inspector, TreeHandlerSettings settings,
                                     async_dispatcher_t* dispatcher,
                                     fidl::ServerEnd<fuchsia_inspect::Tree>&& request);

  /// Get the VMO handle for the Inspector handled by this server.
  void GetContent(GetContentCompleter::Sync& completer) override;

  /// Start a server for handling the lazy child whose name is passed.
  void OpenChild(OpenChildRequestView request, OpenChildCompleter::Sync& completer) override;

  /// Start a server that furnishes the names of this Tree's children.
  ///
  /// The names provided by the server this method starts are valid values to be passed to
  /// `OpenChild`.
  void ListChildNames(ListChildNamesRequestView request,
                      ListChildNamesCompleter::Sync& completer) override;

 private:
  TreeServer(Inspector inspector, TreeHandlerSettings settings, async_dispatcher_t* disp)
      : executor_(disp), settings_(settings), inspector_(std::move(inspector)) {}
  TreeServer(const TreeServer&) = delete;
  TreeServer(TreeServer&&) = delete;
  TreeServer& operator=(TreeServer&&) = delete;
  TreeServer& operator=(const TreeServer&) = delete;

  async::Executor executor_;
  TreeHandlerSettings settings_;
  Inspector inspector_;
  cpp17::optional<fidl::ServerBindingRef<fuchsia_inspect::Tree>> binding_;
};

}  // namespace inspect

#endif  // LIB_INSPECT_COMPONENT_CPP_SERVICE_H_
