// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_DRIVER_RUNNER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_DRIVER_RUNNER_H_

#include <fidl/fuchsia.component/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.host/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/fidl/llcpp/client.h>
#include <lib/fidl/llcpp/wire_messaging.h>
#include <lib/fit/function.h>
#include <lib/fpromise/promise.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/status.h>

#include <list>
#include <unordered_map>

#include <fbl/intrusive_double_list.h>

#include "src/devices/bin/driver_manager/v2/driver_component.h"
#include "src/devices/bin/driver_manager/v2/driver_host.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"

using NodeBindingInfoResultCallback =
    fit::callback<void(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>)>;

// Note, all of the logic here assumes we are operating on a single-threaded
// dispatcher. It is not safe to use a multi-threaded dispatcher with this code.

// This function creates a composite offer based on a 'directory service' offer.
std::optional<fuchsia_component_decl::wire::Offer> CreateCompositeDirOffer(
    fidl::AnyArena& arena, fuchsia_component_decl::wire::Offer& offer,
    std::string_view parents_name);

class Node;

enum class Collection {
  kNone,
  // Collection for driver hosts.
  kHost,
  // Collection for boot drivers.
  kBoot,
  // Collection for package drivers.
  kPackage,
  // Collection for universe package drivers.
  kUniversePackage,
};

// TODO(fxbug.dev/66150): Once FIDL wire types support a Clone() method,
// stop encoding and decoding messages as a workaround.
template <typename T>
class OwnedMessage {
 public:
  static std::unique_ptr<OwnedMessage<T>> From(T& message) {
    // TODO(fxbug.dev/45252): Use FIDL at rest.
    fidl::unstable::OwnedEncodedMessage<T> encoded(fidl::internal::WireFormatVersion::kV2,
                                                   &message);
    ZX_ASSERT_MSG(encoded.ok(), "Failed to encode: %s", encoded.FormatDescription().data());
    return std::make_unique<OwnedMessage>(encoded);
  }

  T& get() { return *decoded_.PrimaryObject(); }

 private:
  friend std::unique_ptr<OwnedMessage<T>> std::make_unique<OwnedMessage<T>>(
      fidl::unstable::OwnedEncodedMessage<T>&);

  // TODO(fxbug.dev/45252): Use FIDL at rest.
  explicit OwnedMessage(fidl::unstable::OwnedEncodedMessage<T>& encoded)
      : converted_(encoded.GetOutgoingMessage()),
        decoded_(fidl::internal::WireFormatVersion::kV2, std::move(converted_.incoming_message())) {
    ZX_ASSERT_MSG(decoded_.ok(), "Failed to decode: %s", decoded_.FormatDescription().c_str());
  }

  fidl::OutgoingToIncomingMessage converted_;
  fidl::unstable::DecodedMessage<T> decoded_;
};

class BindResultTracker {
 public:
  explicit BindResultTracker(size_t expected_result_count,
                             NodeBindingInfoResultCallback result_callback);

  void ReportSuccessfulBind(const std::string_view& node_name, const std::string_view& driver);
  void ReportNoBind();

 private:
  void Complete(size_t current);
  fidl::Arena<> arena_;
  size_t expected_result_count_;
  size_t currently_reported_ TA_GUARDED(lock_);
  std::mutex lock_;
  NodeBindingInfoResultCallback result_callback_;
  std::vector<fuchsia_driver_development::wire::NodeBindingInfo> results_;
};

class DriverBinder {
 public:
  virtual ~DriverBinder() = default;

  // Attempt to bind `node`.
  // A nullptr for result_tracker is acceptable if the caller doesn't intend to
  // track the results.
  virtual void Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) = 0;
};

class Node : public fidl::WireServer<fuchsia_driver_framework::NodeController>,
             public fidl::WireServer<fuchsia_driver_framework::Node>,
             public std::enable_shared_from_this<Node> {
 public:
  using OwnedOffer = std::unique_ptr<OwnedMessage<fuchsia_component_decl::wire::Offer>>;

  Node(std::string_view name, std::vector<Node*> parents, DriverBinder* driver_binder,
       async_dispatcher_t* dispatcher);
  ~Node() override;

  const std::string& name() const;
  const DriverComponent* driver_component() const;
  const std::vector<Node*>& parents() const;
  const std::list<std::shared_ptr<Node>>& children() const;
  std::vector<OwnedOffer>& offers() const;
  fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols() const;
  const std::vector<fuchsia_driver_framework::wire::NodeProperty>& properties() const;
  DriverHostComponent* driver_host() const;

  void set_collection(Collection collection);
  void set_driver_host(DriverHostComponent* driver_host);
  void set_node_ref(fidl::ServerBindingRef<fuchsia_driver_framework::Node> node_ref);
  void set_bound_driver_url(std::optional<std::string_view> bound_driver_url);
  void set_controller_ref(
      fidl::ServerBindingRef<fuchsia_driver_framework::NodeController> controller_ref);
  void set_driver_component(std::unique_ptr<DriverComponent> driver_component);
  void set_parents_names(std::vector<std::string> names) { parents_names_ = std::move(names); }

  std::string TopoName() const;
  fidl::VectorView<fuchsia_component_decl::wire::Offer> CreateOffers(fidl::AnyArena& arena) const;

  fuchsia_driver_framework::wire::NodeAddArgs CreateAddArgs(fidl::AnyArena& arena);

  void OnBind() const;
  void AddToParents();

  // Begin the removal process for a Node. This function ensures that a Node is
  // only removed after all of its children are removed. It also ensures that
  // a Node is only removed after the driver that is bound to it has been stopped.
  // This is safe to call multiple times.
  // There are lots of reasons a Node's removal will be started:
  //   - The Node's driver component wants to exit.
  //   - The `node_ref` server has become unbound.
  //   - The Node's parent is being removed.
  void Remove();

 private:
  // fidl::WireServer<fuchsia_driver_framework::NodeController>
  void Remove(RemoveRequestView request, RemoveCompleter::Sync& completer) override;
  // fidl::WireServer<fuchsia_driver_framework::Node>
  void AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) override;

  const std::string name_;
  // If this is a composite device, this stores the list of each parent's names.
  std::vector<std::string> parents_names_;
  std::vector<Node*> parents_;
  std::list<std::shared_ptr<Node>> children_;
  fit::nullable<DriverBinder*> driver_binder_;
  async_dispatcher_t* const dispatcher_;

  fidl::Arena<128> arena_;
  std::vector<OwnedOffer> offers_;
  std::vector<fuchsia_driver_framework::wire::NodeSymbol> symbols_;
  std::vector<fuchsia_driver_framework::wire::NodeProperty> properties_;

  Collection collection_ = Collection::kNone;
  fit::nullable<DriverHostComponent*> driver_host_;

  bool removal_in_progress_ = false;

  // If this exists, then this `driver_component_` is bound to this node.
  std::unique_ptr<DriverComponent> driver_component_;
  std::optional<std::string> bound_driver_url_;
  std::optional<fidl::ServerBindingRef<fuchsia_driver_framework::Node>> node_ref_;
  std::optional<fidl::ServerBindingRef<fuchsia_driver_framework::NodeController>> controller_ref_;
};

class DriverRunner : public fidl::WireServer<fuchsia_component_runner::ComponentRunner>,
                     public DriverBinder {
 public:
  DriverRunner(fidl::ClientEnd<fuchsia_component::Realm> realm,
               fidl::ClientEnd<fuchsia_driver_index::DriverIndex> driver_index,
               inspect::Inspector& inspector, async_dispatcher_t* dispatcher);

  fpromise::promise<inspect::Inspector> Inspect() const;
  size_t NumOrphanedNodes() const;
  zx::status<> PublishComponentRunner(const fbl::RefPtr<fs::PseudoDir>& svc_dir);
  zx::status<> StartRootDriver(std::string_view url);
  std::shared_ptr<const Node> root_node() const;
  // This function schedules a callback to attempt to bind all orphaned nodes against
  // the base drivers.
  void ScheduleBaseDriversBinding();
  // Goes through the orphan list and attempts the bind them again. Sends nodes that are still
  // orphaned back to the orphan list. Tracks the result of the bindings and then when finished
  // uses the result_callback to report the results.
  void TryBindAllOrphans(NodeBindingInfoResultCallback result_callback);

 private:
  using CompositeArgs = std::vector<std::weak_ptr<Node>>;
  using DriverUrl = std::string;
  using CompositeArgsIterator = std::unordered_multimap<DriverUrl, CompositeArgs>::iterator;

  // fidl::WireServer<fuchsia_component_runner::ComponentRunner>
  void Start(StartRequestView request, StartCompleter::Sync& completer) override;
  // DriverBinder
  // Attempt to bind `node`.
  // A nullptr for result_tracker is acceptable if the caller doesn't intend to
  // track the results.
  void Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) override;

  // Create a composite node. Returns a `Node` that is owned by its parents.
  zx::status<Node*> CreateCompositeNode(
      Node& node, const fuchsia_driver_index::wire::MatchedCompositeInfo& matched_driver);
  // Adds `matched_driver` to an existing set of composite arguments, or creates
  // a new set of composite arguments. Returns an iterator to the set of
  // composite arguments.
  zx::status<CompositeArgsIterator> AddToCompositeArgs(
      const std::string& name,
      const fuchsia_driver_index::wire::MatchedCompositeInfo& matched_driver);
  zx::status<> StartDriver(Node& node, std::string_view url,
                           fuchsia_driver_index::DriverPackageType package_type);

  zx::status<std::unique_ptr<DriverHostComponent>> StartDriverHost();
  // The untracked version of TryBindAllOrphans.
  void TryBindAllOrphansUntracked();

  struct CreateComponentOpts {
    const Node* node = nullptr;
    zx::handle token;
    fidl::ServerEnd<fuchsia_io::Directory> exposed_dir;
  };
  zx::status<> CreateComponent(std::string name, Collection collection, std::string url,
                               CreateComponentOpts opts);

  uint64_t next_driver_host_id_ = 0;
  fidl::WireClient<fuchsia_component::Realm> realm_;
  fidl::WireClient<fuchsia_driver_index::DriverIndex> driver_index_;
  async_dispatcher_t* const dispatcher_;
  std::shared_ptr<Node> root_node_;

  std::unordered_map<zx_koid_t, Node&> driver_args_;
  std::unordered_multimap<DriverUrl, CompositeArgs> composite_args_;
  fbl::DoublyLinkedList<std::unique_ptr<DriverHostComponent>> driver_hosts_;

  // Orphaned nodes are nodes that have failed to bind to a driver, either
  // because no matching driver could be found, or because the matching driver
  // failed to start.
  std::vector<std::weak_ptr<Node>> orphaned_nodes_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_DRIVER_RUNNER_H_
