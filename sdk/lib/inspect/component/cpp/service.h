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
#include <variant>

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
  /// Starts a new server. The implementation deletes itself during teardown after an unbind or
  /// else runs until component shutdown.
  ///
  /// `data` is the Inspect data served over the connection.
  ///
  /// The `Inspector` variant results in full-featured Inspect with lazy nodes
  /// and values.
  ///
  /// The `zx::vmo` variant will not serve lazy nodes/values. It will only serve the Inspect
  /// data in itself. The VMO may contain lazy nodes/values, but they will be ignored when
  /// snapshotting and parsing the data.
  static void StartSelfManagedServer(std::variant<Inspector, zx::vmo> data,
                                     TreeHandlerSettings settings, async_dispatcher_t* dispatcher,
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
  TreeServer(std::variant<Inspector, zx::vmo> data, TreeHandlerSettings settings,
             async_dispatcher_t* disp)
      : executor_(disp), settings_(settings), data_(std::move(data)) {}
  TreeServer(const TreeServer&) = delete;
  TreeServer(TreeServer&&) = delete;
  TreeServer& operator=(TreeServer&&) = delete;
  TreeServer& operator=(const TreeServer&) = delete;

  async::Executor executor_;
  TreeHandlerSettings settings_;
  std::variant<Inspector, zx::vmo> data_;
  cpp17::optional<fidl::ServerBindingRef<fuchsia_inspect::Tree>> binding_;
};

}  // namespace inspect

#endif  // LIB_INSPECT_COMPONENT_CPP_SERVICE_H_
