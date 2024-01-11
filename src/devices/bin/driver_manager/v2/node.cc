// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/node.h"

#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <deque>
#include <unordered_set>
#include <utility>

#include <bind/fuchsia/platform/cpp/bind.h>

#include "src/devices/bin/driver_manager/v2/node_removal_tracker.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/join_strings.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
namespace fdecl = fuchsia_component_decl;
namespace fcomponent = fuchsia_component;

namespace dfv2 {

namespace {

const std::string kUnboundUrl = "unbound";

// TODO(https://fxbug.dev/124976): Remove this flag once composite node spec rebind once all clients are updated
// to the new Rebind() behavior and this is fully implemented on both DFv1 and DFv2.
constexpr bool kEnableCompositeNodeSpecRebind = false;

template <typename R, typename F>
std::optional<R> VisitOffer(fdecl::Offer& offer, F apply) {
  // Note, we access each field of the union as mutable, so that `apply` can
  // modify the field if necessary.
  switch (offer.Which()) {
    case fdecl::Offer::Tag::kService:
      return apply(offer.service());
    case fdecl::Offer::Tag::kProtocol:
      return apply(offer.protocol());
    case fdecl::Offer::Tag::kDirectory:
      return apply(offer.directory());
    case fdecl::Offer::Tag::kStorage:
      return apply(offer.storage());
    case fdecl::Offer::Tag::kRunner:
      return apply(offer.runner());
    case fdecl::Offer::Tag::kResolver:
      return apply(offer.resolver());
    case fdecl::Offer::Tag::kEventStream:
      return apply(offer.event_stream());
    default:
      return {};
  }
}

const char* CollectionName(Collection collection) {
  switch (collection) {
    case Collection::kNone:
      return "";
    case Collection::kBoot:
      return "boot-drivers";
    case Collection::kPackage:
      return "pkg-drivers";
    case Collection::kFullPackage:
      return "full-pkg-drivers";
  }
}

uint32_t GetProtocolId(const std::vector<fdf::wire::NodeProperty>& properties) {
  uint32_t protocol_id = 0;
  for (const fdf::wire::NodeProperty& property : properties) {
    if (!property.key.is_int_value()) {
      continue;
    }
    if (property.key.int_value() != BIND_PROTOCOL) {
      continue;
    }
    protocol_id = property.value.int_value();
  }
  return protocol_id;
}

fit::result<fdf::wire::NodeError, fdecl::Offer> AddOfferToNodeOffer(fdecl::Offer add_offer,
                                                                    fdecl::Ref source) {
  auto has_source_name =
      VisitOffer<bool>(add_offer, [](const auto& decl) { return decl->source_name().has_value(); });
  if (!has_source_name.value_or(false)) {
    return fit::as_error(fdf::wire::NodeError::kOfferSourceNameMissing);
  }
  auto has_ref = VisitOffer<bool>(add_offer, [](const auto& decl) {
    return decl->source().has_value() || decl->target().has_value();
  });
  if (has_ref.value_or(false)) {
    return fit::as_error(fdf::wire::NodeError::kOfferRefExists);
  }

  // Assign the source of the offer.
  VisitOffer<bool>(add_offer, [source = std::move(source)](auto decl) mutable {
    decl->source(std::move(source));
    return true;
  });

  return fit::ok(std::move(add_offer));
}

bool IsDefaultOffer(std::string_view target_name) {
  return std::string_view("default").compare(target_name) == 0;
}

template <typename T>
void CloseIfExists(std::optional<fidl::ServerBinding<T>>& ref) {
  if (ref) {
    ref->Close(ZX_OK);
  }
}

fit::result<fdf::wire::NodeError> ValidateSymbols(std::vector<fdf::NodeSymbol>& symbols) {
  std::unordered_set<std::string_view> names;
  for (auto& symbol : symbols) {
    if (!symbol.name().has_value()) {
      LOGF(ERROR, "SymbolError: a symbol is missing a name");
      return fit::error(fdf::wire::NodeError::kSymbolNameMissing);
    }
    if (!symbol.address().has_value()) {
      LOGF(ERROR, "SymbolError: symbol '%s' is missing an address", symbol.name().value().c_str());
      return fit::error(fdf::wire::NodeError::kSymbolAddressMissing);
    }
    auto [_, inserted] = names.emplace(symbol.name().value());
    if (!inserted) {
      LOGF(ERROR, "SymbolError: symbol '%s' already exists", symbol.name().value().c_str());
      return fit::error(fdf::wire::NodeError::kSymbolAlreadyExists);
    }
  }
  return fit::ok();
}

}  // namespace

std::optional<fdecl::wire::Offer> CreateCompositeServiceOffer(fidl::AnyArena& arena,
                                                              fdecl::wire::Offer& offer,
                                                              std::string_view parents_name,
                                                              bool primary_parent) {
  if (!offer.is_service() || !offer.service().has_source_instance_filter() ||
      !offer.service().has_renamed_instances()) {
    return std::nullopt;
  }

  size_t new_instance_count = offer.service().renamed_instances().count();
  if (primary_parent) {
    for (auto& instance : offer.service().renamed_instances()) {
      if (IsDefaultOffer(instance.target_name.get())) {
        new_instance_count++;
      }
    }
  }

  size_t new_filter_count = offer.service().source_instance_filter().count();
  if (primary_parent) {
    for (auto& filter : offer.service().source_instance_filter()) {
      if (IsDefaultOffer(filter.get())) {
        new_filter_count++;
      }
    }
  }

  // We have to create a new offer so we aren't manipulating our parent's offer.
  auto service = fdecl::wire::OfferService::Builder(arena);
  if (offer.service().has_source_name()) {
    service.source_name(offer.service().source_name());
  }
  if (offer.service().has_target_name()) {
    service.target_name(offer.service().target_name());
  }
  if (offer.service().has_source()) {
    service.source(offer.service().source());
  }
  if (offer.service().has_target()) {
    service.target(offer.service().target());
  }

  size_t index = 0;
  fidl::VectorView<fdecl::wire::NameMapping> mappings(arena, new_instance_count);
  for (auto instance : offer.service().renamed_instances()) {
    // The instance is not "default", so copy it over.
    if (!IsDefaultOffer(instance.target_name.get())) {
      mappings[index].source_name = fidl::StringView(arena, instance.source_name.get());
      mappings[index].target_name = fidl::StringView(arena, instance.target_name.get());
      index++;
      continue;
    }

    // We are the primary parent, so add the "default" offer.
    if (primary_parent) {
      mappings[index].source_name = fidl::StringView(arena, instance.source_name.get());
      mappings[index].target_name = fidl::StringView(arena, instance.target_name.get());
      index++;
    }

    // Rename the instance to match the parent's name.
    mappings[index].source_name = fidl::StringView(arena, instance.source_name.get());
    mappings[index].target_name = fidl::StringView(arena, parents_name);
    index++;
  }
  ZX_ASSERT(index == new_instance_count);

  index = 0;
  fidl::VectorView<fidl::StringView> filters(arena, new_instance_count);
  for (auto filter : offer.service().source_instance_filter()) {
    // The filter is not "default", so copy it over.
    if (!IsDefaultOffer(filter.get())) {
      filters[index] = fidl::StringView(arena, filter.get());
      index++;
      continue;
    }

    // We are the primary parent, so add the "default" filter.
    if (primary_parent) {
      filters[index] = fidl::StringView(arena, "default");
      index++;
    }

    // Rename the filter to match the parent's name.
    filters[index] = fidl::StringView(arena, parents_name);
    index++;
  }
  ZX_ASSERT(index == new_filter_count);

  service.renamed_instances(mappings);
  service.source_instance_filter(filters);

  return fdecl::wire::Offer::WithService(arena, service.Build());
}

std::optional<fdecl::wire::Offer> CreateCompositeOffer(fidl::AnyArena& arena,
                                                       fdecl::wire::Offer& offer,
                                                       std::string_view parents_name,
                                                       bool primary_parent) {
  // We route 'service' capabilities based on the parent's name.
  if (offer.is_service()) {
    return CreateCompositeServiceOffer(arena, offer, parents_name, primary_parent);
  }

  // Other capabilities we can simply forward unchanged, but allocated on the new arena.
  return fidl::ToWire(arena, fidl::ToNatural(offer));
}

Node::Node(std::string_view name, std::vector<std::weak_ptr<Node>> parents,
           NodeManager* node_manager, async_dispatcher_t* dispatcher, DeviceInspect inspect,
           uint32_t primary_index, NodeType type)
    : name_(name),
      type_(type),
      parents_(std::move(parents)),
      primary_index_(primary_index),
      node_manager_(node_manager),
      dispatcher_(dispatcher),
      inspect_(std::move(inspect)) {
  if (type == NodeType::kNormal) {
    ZX_ASSERT(parents_.size() <= 1);
  }

  ZX_ASSERT(primary_index_ == 0 || primary_index_ < parents_.size());
  if (auto primary_parent = GetPrimaryParent()) {
    // By default, we set `driver_host_` to match the primary parent's
    // `driver_host_`. If the node is then subsequently bound to a driver in a
    // different driver host, this value will be updated to match.
    driver_host_ = primary_parent->driver_host_;
  }
}

zx::result<std::shared_ptr<Node>> Node::CreateCompositeNode(
    std::string_view node_name, std::vector<std::weak_ptr<Node>> parents,
    std::vector<std::string> parents_names,
    const std::vector<fuchsia_driver_framework::wire::NodeProperty>& properties,
    NodeManager* driver_binder, async_dispatcher_t* dispatcher, bool is_legacy,
    uint32_t primary_index) {
  ZX_ASSERT(!parents.empty());
  if (primary_index >= parents.size()) {
    LOGF(ERROR, "Primary node index is out of bounds");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto primary_node_ptr = parents[primary_index].lock();
  if (!primary_node_ptr) {
    LOGF(ERROR, "Primary node freed before use");
    return zx::error(ZX_ERR_INTERNAL);
  }
  DeviceInspect inspect =
      primary_node_ptr->inspect_.CreateChild(std::string(node_name), zx::vmo(), 0);
  std::shared_ptr composite = std::make_shared<Node>(
      node_name, std::move(parents), driver_binder, dispatcher, std::move(inspect), primary_index,
      is_legacy ? NodeType::kLegacyComposite : NodeType::kComposite);
  composite->parents_names_ = std::move(parents_names);

  for (const auto& prop : properties) {
    auto natural = fidl::ToNatural(prop);
    auto new_prop = fidl::ToWire(composite->arena_, std::move(natural));
    composite->properties_.push_back(new_prop);
  }
  composite->SetAndPublishInspect();

  Node* primary = composite->GetPrimaryParent();
  // We know that our device has a parent because we're creating it.
  ZX_ASSERT(primary);

  // Copy the symbols from the primary parent.
  composite->symbols_.reserve(primary->symbols_.size());
  for (auto& symbol : primary->symbols_) {
    composite->symbols_.emplace_back(fdf::wire::NodeSymbol::Builder(composite->arena_)
                                         .name(composite->arena_, symbol.name().get())
                                         .address(symbol.address())
                                         .Build());
  }

  // Copy the offers from each parent.
  std::vector<fdecl::wire::Offer> node_offers;
  size_t parent_index = 0;
  for (const std::weak_ptr<Node> parent : composite->parents_) {
    auto parent_ptr = parent.lock();
    if (!parent_ptr) {
      LOGF(ERROR, "Composite parent node freed before use");
      return zx::error(ZX_ERR_INTERNAL);
    }
    auto parent_offers = parent_ptr->offers();
    node_offers.reserve(node_offers.size() + parent_offers.count());

    for (auto& parent_offer : parent_offers) {
      auto offer = CreateCompositeOffer(composite->arena_, parent_offer,
                                        composite->parents_names_[parent_index],
                                        parent_index == primary_index);
      if (offer) {
        node_offers.push_back(*offer);
      }
    }
    parent_index++;
  }
  composite->offers_ = std::move(node_offers);

  composite->AddToParents();
  ZX_ASSERT_MSG(primary->devfs_device_.topological_node().has_value(), "%s",
                composite->MakeTopologicalPath().c_str());

  primary->devfs_device_.topological_node().value().add_child(
      composite->name_, std::nullopt, composite->CreateDevfsPassthrough(std::nullopt, std::nullopt),
      composite->devfs_device_);
  composite->devfs_device_.publish();
  return zx::ok(std::move(composite));
}

Node::~Node() {
  // TODO(https://fxbug.dev/135416): Notify the NodeRemovalTracker if the node is deallocated before shutdown is
  // complete.
  if (GetNodeState() != NodeState::kStopped) {
    LOGF(INFO, "Node %s deallocating while at state %s", MakeComponentMoniker().c_str(),
         GetShutdownHelper().NodeStateAsString());
  }

  CloseIfExists(controller_ref_);
  CloseIfExists(node_ref_);

  if (pending_bind_completer_.has_value()) {
    pending_bind_completer_.value()(zx::error(ZX_ERR_CANCELED));
    pending_bind_completer_.reset();
  }

  if (composite_rebind_completer_.has_value() && composite_rebind_completer_.value()) {
    composite_rebind_completer_.value()(zx::ok());
    composite_rebind_completer_.reset();
  }
}

const std::string& Node::driver_url() const {
  if (driver_component_) {
    return driver_component_->driver_url;
  }
  return kUnboundUrl;
}

std::string Node::MakeTopologicalPath() const {
  std::deque<std::string_view> names;
  for (auto node = this; node != nullptr; node = node->GetPrimaryParent()) {
    names.push_front(node->name());
  }
  return fxl::JoinStrings(names, "/");
}

std::string Node::MakeComponentMoniker() const {
  std::string topo_path = MakeTopologicalPath();

  // The driver's component name is based on the node name, which means that the
  // node name cam only have [a-z0-9-_.] characters. DFv1 composites contain ':'
  // which is not allowed, so replace those characters.
  // TODO(https://fxbug.dev/111156): Migrate driver names to only use CF valid characters.
  std::replace(topo_path.begin(), topo_path.end(), ':', '_');
  std::replace(topo_path.begin(), topo_path.end(), '/', '.');
  return topo_path;
}

void Node::OnBind() const {
  if (controller_ref_) {
    fidl::Status result = fidl::WireSendEvent(*controller_ref_)->OnBind();
    if (!result.ok()) {
      LOGF(ERROR, "Failed to send OnBind event: %s", result.FormatDescription().data());
    }
  }
}

void Node::Stop(StopCompleter::Sync& completer) {
  LOGF(DEBUG, "Calling Remove on %s because of Stop() from component framework.", name().c_str());
  Remove(RemovalSet::kAll, nullptr);
}

void Node::Kill(KillCompleter::Sync& completer) {
  LOGF(DEBUG, "Calling Remove on %s because of Kill() from component framework.", name().c_str());
  Remove(RemovalSet::kAll, nullptr);
}

void Node::CompleteBind(zx::result<> result) {
  if (result.is_error()) {
    LOGF(WARNING, "Bind failed for node '%s'", MakeComponentMoniker().c_str());
    driver_component_.reset();
  }

  if (driver_component_) {
    ZX_ASSERT_MSG(!driver_component_->is_bind_complete,
                  "CompleteBind() called multiple times for node %s", name().c_str());
    driver_component_->is_bind_complete = true;
  }

  auto completer = std::move(pending_bind_completer_);
  pending_bind_completer_.reset();
  if (completer.has_value()) {
    completer.value()(result);
  }

  GetShutdownHelper().CheckNodeState();
}

void Node::AddToParents() {
  auto this_node = shared_from_this();
  for (auto& parent : parents_) {
    if (auto ptr = parent.lock(); ptr) {
      ptr->children_.push_back(this_node);
      continue;
    }
    LOGF(WARNING, "Parent freed before child %s could be added to it", name().c_str());
  }
}

ShutdownHelper& Node::GetShutdownHelper() {
  if (!shutdown_helper_) {
    bool is_shutdown_test_delay_enabled =
        node_manager_.has_value() && node_manager_.value()->IsTestShutdownDelayEnabled();
    auto shutdown_rng = node_manager_.has_value() ? node_manager_.value()->GetShutdownTestRng()
                                                  : std::weak_ptr<std::mt19937>();
    shutdown_helper_ = std::make_unique<ShutdownHelper>(
        this, dispatcher_, is_shutdown_test_delay_enabled, shutdown_rng);
  }
  return *shutdown_helper_.get();
}

// TODO(https://fxbug.dev/124976): If the node invoking this function cannot multibind to composites,
// is parenting one composite node, and is not in a state for removal, then it
// should attempt to bind to something else.
void Node::RemoveChild(const std::shared_ptr<Node>& child) {
  LOGF(DEBUG, "RemoveChild %s from parent %s", child->name().c_str(), name().c_str());
  children_.erase(std::find(children_.begin(), children_.end(), child));
  GetShutdownHelper().CheckNodeState();
}

void Node::FinishShutdown(fit::callback<void()> shutdown_callback) {
  ZX_ASSERT_MSG(GetNodeState() == NodeState::kWaitingOnDriverComponent,
                "FinishShutdown called in invalid node state: %s",
                GetShutdownHelper().NodeStateAsString());
  LOGF(INFO, "Node: %s finishing shutdown", name().c_str());

  if (shutdown_intent() == ShutdownIntent::kRestart) {
    shutdown_callback();
    FinishRestart();
    return;
  }

  LOGF(DEBUG, "Node: %s unbinding and resetting", name().c_str());
  CloseIfExists(controller_ref_);
  CloseIfExists(node_ref_);
  devfs_device_.unpublish();

  // Store a shared_ptr to ourselves so we won't be freed halfway through this function.
  std::shared_ptr this_node = shared_from_this();
  driver_component_.reset();
  for (auto& parent : parents()) {
    if (auto ptr = parent.lock(); ptr) {
      ptr->RemoveChild(this_node);
      continue;
    }
    LOGF(WARNING, "Parent freed before child %s could be removed from it", name().c_str());
  }
  parents_.clear();

  shutdown_callback();

  if (remove_complete_callback_) {
    remove_complete_callback_();
  }

  if (shutdown_intent() == ShutdownIntent::kRebindComposite && composite_rebind_completer_ &&
      composite_rebind_completer_.value()) {
    composite_rebind_completer_.value()(zx::ok());
    composite_rebind_completer_.reset();
  }
}

void Node::FinishRestart() {
  ZX_ASSERT_MSG(shutdown_intent() == ShutdownIntent::kRestart,
                "FinishRestart called when node is not restarting.");

  GetShutdownHelper().ResetShutdown();

  // Store previous url before we reset the driver_component_.
  std::string previous_url = driver_url();

  // Perform cleanups for previous driver before we try to start the next driver.
  driver_component_.reset();
  CloseIfExists(node_ref_);

  if (restart_driver_url_suffix_.has_value()) {
    auto tracker = CreateBindResultTracker();
    node_manager_.value()->BindToUrl(*this, restart_driver_url_suffix_.value(), std::move(tracker));
    restart_driver_url_suffix_.reset();
    return;
  }

  zx::result start_result =
      node_manager_.value()->StartDriver(*this, previous_url, driver_package_type_);
  if (start_result.is_error()) {
    LOGF(ERROR, "Failed to start driver '%s': %s", name().c_str(), start_result.status_string());
  }
}

void Node::ClearHostDriver() {
  if (driver_component_) {
    driver_component_->driver = {};
    driver_component_->is_bind_complete = false;
  }
}

// State table for package driver:
//                                   Initial States
//                 Running | Prestop|  WoC   | WoDriver | Stopping
// Remove(kPkg)      WoC   |  WoC   | Ignore |  Error!  |  Error!
// Remove(kAll)      WoC   |  WoC   |  WoC   |  Error!  |  Error!
// children empty    N/A   |  N/A   |WoDriver|  Error!  |  Error!
// Driver exit       WoC   |  WoC   |  WoC   | Stopping |  Error!
//
// State table for boot driver:
//                                   Initial States
//                  Running | Prestop |  WoC   | WoDriver | Stopping
// Remove(kPkg)     Prestop | Ignore  | Ignore |  Ignore  |  Ignore
// Remove(kAll)      WoC    |   WoC   | Ignore |  Ignore  |  Ignore
// children empty    N/A    |   N/A   |WoDriver|  Ignore  |  Ignore
// Driver exit       WoC    |   WoC   |  WoC   | Stopping |  Ignore
// Boot drivers go into the Prestop state when Remove(kPackage) is set, to signify that
// a removal is taking place, but this node will not be removed yet, even if all its children
// are removed.
void Node::Remove(RemovalSet removal_set, NodeRemovalTracker* removal_tracker) {
  GetShutdownHelper().Remove(shared_from_this(), removal_set, removal_tracker);
}

void Node::RestartNode() {
  GetShutdownHelper().set_shutdown_intent(ShutdownIntent::kRestart);
  Remove(RemovalSet::kAll, nullptr);
}

// TODO(https://fxbug.dev/132254): Handle the case in which this function is called during node removal.
void Node::RestartNodeWithRematch(std::optional<std::string> restart_driver_url_suffix,
                                  fit::callback<void(zx::result<>)> completer) {
  if (pending_bind_completer_.has_value()) {
    completer(zx::error(ZX_ERR_ALREADY_EXISTS));
    return;
  }

  pending_bind_completer_ = std::move(completer);
  restart_driver_url_suffix_ = std::move(restart_driver_url_suffix);
  RestartNode();
}

void Node::RestartNodeWithRematch() {
  RestartNodeWithRematch("", [](zx::result<> result) {});
}

// TODO(https://fxbug.dev/132254): Handle the case in which this function is called during node removal.
void Node::RemoveCompositeNodeForRebind(fit::callback<void(zx::result<>)> completer) {
  if (composite_rebind_completer_.has_value()) {
    completer(zx::error(ZX_ERR_ALREADY_EXISTS));
    return;
  }

  if (type_ != NodeType::kComposite) {
    completer(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  composite_rebind_completer_ = std::move(completer);
  GetShutdownHelper().set_shutdown_intent(ShutdownIntent::kRebindComposite);
  Remove(RemovalSet::kAll, nullptr);
}

std::shared_ptr<BindResultTracker> Node::CreateBindResultTracker() {
  return std::make_shared<BindResultTracker>(
      1, [weak_self = weak_from_this()](
             fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> info) {
        std::shared_ptr self = weak_self.lock();
        if (!self) {
          return;
        }
        // We expect a single successful "bind". If we don't get it, we can assume the bind
        // request failed. If we do get it, we will continue to wait for the driver's start hook
        // to complete, which will only occur after the successful bind. The remaining flow will
        // be similar to the RestartNode flow.
        if (info.count() < 1) {
          self->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
        } else if (info.count() > 1) {
          LOGF(ERROR, "Unexpectedly bound multiple drivers to a single node");
          self->CompleteBind(zx::error(ZX_ERR_BAD_STATE));
        }
      });
}

fit::result<fuchsia_driver_framework::wire::NodeError, std::shared_ptr<Node>> Node::AddChildHelper(
    fuchsia_driver_framework::NodeAddArgs args,
    fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller,
    fidl::ServerEnd<fuchsia_driver_framework::Node> node) {
  if (node_manager_ == nullptr) {
    LOGF(WARNING, "Failed to add Node, as this Node '%s' was removed", name().data());
    return fit::as_error(fdf::wire::NodeError::kNodeRemoved);
  }
  if (GetShutdownHelper().IsShuttingDown()) {
    LOGF(WARNING, "Failed to add Node, as this Node '%s' is being removed", name().c_str());
    return fit::as_error(fdf::wire::NodeError::kNodeRemoved);
  }
  if (!args.name().has_value()) {
    LOGF(ERROR, "Failed to add Node, a name must be provided");
    return fit::as_error(fdf::wire::NodeError::kNameMissing);
  }
  std::string_view name = args.name().value();
  for (auto& child : children_) {
    if (child->name() == name) {
      LOGF(ERROR, "Failed to add Node '%.*s', name already exists among siblings",
           static_cast<int>(name.size()), name.data());
      return fit::as_error(fdf::wire::NodeError::kNameAlreadyExists);
    }
  };
  zx::vmo inspect_vmo;
  if (args.devfs_args().has_value() && args.devfs_args()->inspect().has_value()) {
    inspect_vmo = std::move(args.devfs_args()->inspect().value());
  }
  DeviceInspect inspect = inspect_.CreateChild(std::string(name), std::move(inspect_vmo), 0);
  std::shared_ptr child =
      std::make_shared<Node>(name, std::vector<std::weak_ptr<Node>>{weak_from_this()},
                             *node_manager_, dispatcher_, std::move(inspect));

  if (args.offers().has_value()) {
    child->offers_.reserve(args.offers().value().size());

    // Find a parent node with a collection. This indicates that a driver has
    // been bound to the node, and the driver is running within the collection.
    Node* source_node = this;
    while (source_node && source_node->collection_ == Collection::kNone) {
      source_node = source_node->GetPrimaryParent();
    }
    fdecl::Ref source_ref =
        fdecl::Ref::WithChild(fdecl::ChildRef()
                                  .name(source_node->MakeComponentMoniker())
                                  .collection(CollectionName(source_node->collection_)));

    for (auto& offer : args.offers().value()) {
      fit::result new_offer = AddOfferToNodeOffer(offer, source_ref);
      if (new_offer.is_error()) {
        LOGF(ERROR, "Failed to add Node '%s': Bad add offer: %d",
             child->MakeTopologicalPath().c_str(), new_offer.error_value());
        return new_offer.take_error();
      }
      child->offers_.push_back(fidl::ToWire(child->arena_, new_offer.value()));
    }
  }

  if (args.properties().has_value()) {
    child->properties_.reserve(args.properties()->size() + 1);  // + 1 for DFv2 prop.
    for (auto& property : args.properties().value()) {
      child->properties_.emplace_back(fidl::ToWire(child->arena_, property));
    }
  }

  // We set a property for DFv2 devices.
  child->properties_.emplace_back(fdf::MakeProperty(
      child->arena_, bind_fuchsia_platform::DRIVER_FRAMEWORK_VERSION, static_cast<uint32_t>(2)));

  child->SetAndPublishInspect();

  if (args.symbols().has_value()) {
    auto is_valid = ValidateSymbols(args.symbols().value());
    if (is_valid.is_error()) {
      LOGF(ERROR, "Failed to add Node '%.*s', bad symbols", static_cast<int>(name.size()),
           name.data());
      return fit::as_error(is_valid.error_value());
    }

    child->symbols_.reserve(args.symbols().value().size());
    for (auto& symbol : args.symbols().value()) {
      child->symbols_.emplace_back(fdf::wire::NodeSymbol::Builder(child->arena_)
                                       .name(child->arena_, symbol.name().value())
                                       .address(symbol.address().value())
                                       .Build());
    }
  }

  Devnode::Target devfs_target;
  std::optional<std::string_view> devfs_class_path;
  auto& devfs_args = args.devfs_args();
  if (devfs_args.has_value()) {
    if (devfs_args->class_name().has_value()) {
      devfs_class_path = devfs_args->class_name();
    }

    devfs_target = child->CreateDevfsPassthrough(std::move(devfs_args->connector()),
                                                 devfs_args->connector_supports());
  } else {
    devfs_target = child->CreateDevfsPassthrough(std::nullopt, std::nullopt);
  }
  ZX_ASSERT(devfs_device_.topological_node().has_value());
  zx_status_t status = devfs_device_.topological_node()->add_child(
      child->name_, devfs_class_path, std::move(devfs_target), child->devfs_device_);
  ZX_ASSERT_MSG(status == ZX_OK, "%s failed to export: %s", child->MakeTopologicalPath().c_str(),
                zx_status_get_string(status));
  ZX_ASSERT(child->devfs_device_.topological_node().has_value());
  child->devfs_device_.publish();

  if (controller.is_valid()) {
    child->controller_ref_.emplace(dispatcher_, std::move(controller), child.get(),
                                   fidl::kIgnoreBindingClosure);
  }
  if (node.is_valid()) {
    child->node_ref_.emplace(dispatcher_, std::move(node), child.get(), [](Node* node, auto) {
      node->node_ref_.reset();
      LOGF(WARNING, "Removing node %s because of binding closed", node->name().c_str());
      node->Remove(RemovalSet::kAll, nullptr);
    });
  } else {
    // We don't care about tracking binds here, sending nullptr is fine.
    (*node_manager_)->Bind(*child, nullptr);
  }

  child->AddToParents();
  return fit::ok(child);
}

void Node::WaitForChildToExit(
    std::string_view name,
    fit::callback<void(fit::result<fuchsia_driver_framework::wire::NodeError>)> callback) {
  for (auto& child : children_) {
    if (child->name() != name) {
      continue;
    }
    if (!child->GetShutdownHelper().IsShuttingDown()) {
      LOGF(ERROR, "Failed to add Node '%.*s', name already exists among siblings",
           static_cast<int>(name.size()), name.data());
      callback(fit::as_error(fdf::wire::NodeError::kNameAlreadyExists));
      return;
    }
    if (child->remove_complete_callback_) {
      LOGF(ERROR,
           "Failed to add Node '%.*s': Node with name already exists and is marked to be replaced.",
           static_cast<int>(name.size()), name.data());
      callback(fit::as_error(fdf::wire::NodeError::kNameAlreadyExists));
      return;
    }
    child->remove_complete_callback_ = [callback = std::move(callback)]() mutable {
      callback(fit::success());
    };
    return;
  };
  callback(fit::success());
}

void Node::AddChild(fuchsia_driver_framework::NodeAddArgs args,
                    fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller,
                    fidl::ServerEnd<fuchsia_driver_framework::Node> node,
                    AddNodeResultCallback callback) {
  if (!args.name().has_value()) {
    LOGF(ERROR, "Failed to add Node, a name must be provided");
    callback(fit::as_error(fdf::wire::NodeError::kNameMissing));
    return;
  }
  std::string name = args.name().value();
  WaitForChildToExit(
      name, [self = shared_from_this(), args = std::move(args), controller = std::move(controller),
             node = std::move(node), callback = std::move(callback)](
                fit::result<fuchsia_driver_framework::wire::NodeError> result) mutable {
        if (result.is_error()) {
          callback(result.take_error());
          return;
        }
        callback(self->AddChildHelper(std::move(args), std::move(controller), std::move(node)));
      });
}

void Node::Remove(RemoveCompleter::Sync& completer) {
  LOGF(DEBUG, "Remove() Fidl call for %s", name().c_str());
  Remove(RemovalSet::kAll, nullptr);
}

void Node::RequestBind(RequestBindRequestView request, RequestBindCompleter::Sync& completer) {
  bool force_rebind = false;
  if (request->has_force_rebind()) {
    force_rebind = request->force_rebind();
  }
  if (driver_component_.has_value() && !force_rebind) {
    completer.ReplyError(ZX_ERR_ALREADY_BOUND);
    return;
  }

  if (pending_bind_completer_.has_value()) {
    completer.ReplyError(ZX_ERR_ALREADY_EXISTS);
    return;
  }

  std::optional<std::string> driver_url_suffix;
  if (request->has_driver_url_suffix()) {
    driver_url_suffix = std::string(request->driver_url_suffix().get());
  }

  auto completer_wrapper = [completer = completer.ToAsync()](zx::result<> result) mutable {
    if (result.is_ok()) {
      completer.ReplySuccess();
    } else {
      completer.ReplyError(result.error_value());
    }
  };

  if (driver_component_.has_value()) {
    RestartNodeWithRematch(driver_url_suffix, std::move(completer_wrapper));
    return;
  }

  pending_bind_completer_ = std::move(completer_wrapper);
  auto tracker = CreateBindResultTracker();
  if (driver_url_suffix.has_value()) {
    node_manager_.value()->BindToUrl(*this, driver_url_suffix.value(), std::move(tracker));
  } else {
    node_manager_.value()->Bind(*this, std::move(tracker));
  }
}

void Node::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_driver_framework::NodeController> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  std::string method_type;
  switch (metadata.unknown_method_type) {
    case fidl::UnknownMethodType::kOneWay:
      method_type = "one-way";
      break;
    case fidl::UnknownMethodType::kTwoWay:
      method_type = "two-way";
      break;
  };

  LOGF(WARNING, "fdf::NodeController received unknown %s method. Ordinal: %lu", method_type.c_str(),
       metadata.method_ordinal);
}

void Node::AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) {
  AddChild(fidl::ToNatural(request->args), std::move(request->controller), std::move(request->node),
           [completer = completer.ToAsync()](
               fit::result<fuchsia_driver_framework::wire::NodeError, std::shared_ptr<Node>>
                   result) mutable {
             if (result.is_error()) {
               completer.Reply(result.take_error());
             } else {
               completer.ReplySuccess();
             }
           });
}

void Node::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_driver_framework::Node> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  std::string method_type;
  switch (metadata.unknown_method_type) {
    case fidl::UnknownMethodType::kOneWay:
      method_type = "one-way";
      break;
    case fidl::UnknownMethodType::kTwoWay:
      method_type = "two-way";
      break;
  };

  LOGF(WARNING, "fdf::Node received unknown %s method. Ordinal: %lu", method_type.c_str(),
       metadata.method_ordinal);
}

void Node::StartDriver(fuchsia_component_runner::wire::ComponentStartInfo start_info,
                       fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
                       fit::callback<void(zx::result<>)> cb) {
  auto url = start_info.resolved_url().get();
  bool colocate =
      fdf_internal::ProgramValue(start_info.program(), "colocate").value_or("") == "true";
  bool host_restart_on_crash =
      fdf_internal::ProgramValue(start_info.program(), "host_restart_on_crash").value_or("") ==
      "true";

  if (host_restart_on_crash && colocate) {
    LOGF(ERROR,
         "Failed to start driver '%.*s'. Both host_restart_on_crash and colocate cannot be true.",
         static_cast<int>(url.size()), url.data());
    cb(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  host_restart_on_crash_ = host_restart_on_crash;

  if (colocate && !driver_host_) {
    LOGF(ERROR,
         "Failed to start driver '%.*s', driver is colocated but does not have a prent with a "
         "driver host",
         static_cast<int>(url.size()), url.data());
    cb(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  auto symbols = fidl::VectorView<fdf::wire::NodeSymbol>();
  if (colocate) {
    symbols = this->symbols();
  }

  // Launch a driver host if we are not colocated.
  if (!colocate) {
    auto result = (*node_manager_)->CreateDriverHost();
    if (result.is_error()) {
      cb(result.take_error());
      return;
    }
    driver_host_ = result.value();
  }

  // Bind the Node associated with the driver.
  auto endpoints = fidl::CreateEndpoints<fdf::Node>();
  if (endpoints.is_error()) {
    cb(zx::error(endpoints.error_value()));
    return;
  }
  node_ref_.emplace(
      dispatcher_, std::move(endpoints->server), this, [](Node* node, fidl::UnbindInfo info) {
        node->node_ref_.reset();
        // If the unbind is initiated from us, we don't need to do anything to handle
        // the closure.
        if (info.is_user_initiated()) {
          return;
        }

        if (node->GetNodeState() == NodeState::kRunning) {
          // If the node is running but this node closure has happened, then we want to restart
          // the node if it has the host_restart_on_crash_ enabled on it.
          if (node->host_restart_on_crash_) {
            LOGF(INFO, "Restarting node %s due to node closure while running.",
                 node->name().c_str());
            node->RestartNode();
            return;
          }

          LOGF(WARNING, "fdf::Node binding for node %s closed while the node was running: %s",
               node->name().c_str(), info.FormatDescription().c_str());
        }

        node->Remove(RemovalSet::kAll, nullptr);
      });

  LOGF(INFO, "Binding %.*s to  %s", static_cast<int>(url.size()), url.data(), name().c_str());
  // Start the driver within the driver host.
  zx::result driver_endpoints = fidl::CreateEndpoints<fuchsia_driver_host::Driver>();
  if (driver_endpoints.is_error()) {
    cb(driver_endpoints.take_error());
    return;
  }
  driver_component_.emplace(*this, std::string(url), std::move(controller),
                            std::move(driver_endpoints->client));
  driver_host_.value()->Start(
      std::move(endpoints->client), name_, symbols, start_info, std::move(driver_endpoints->server),
      [weak_self = weak_from_this(), cb = std::move(cb)](zx::result<> result) mutable {
        auto node_ptr = weak_self.lock();
        if (!node_ptr) {
          LOGF(WARNING, "Node freed before it is used");
          cb(result);
          return;
        }

        if (result.is_error()) {
          LOGF(WARNING, "Failed to start driver host for %s",
               node_ptr->MakeComponentMoniker().c_str());
          node_ptr->driver_component_.reset();
          node_ptr->GetShutdownHelper().CheckNodeState();
        }
        cb(result);
      });
}

bool Node::EvaluateRematchFlags(fuchsia_driver_development::RestartRematchFlags rematch_flags,
                                std::string_view requested_url) {
  if (type_ == NodeType::kLegacyComposite &&
      !(rematch_flags & fuchsia_driver_development::RestartRematchFlags::kLegacyComposite)) {
    return false;
  }

  if (type_ == NodeType::kComposite &&
      !(rematch_flags & fuchsia_driver_development::RestartRematchFlags::kCompositeSpec)) {
    return false;
  }

  if (driver_url() == requested_url &&
      !(rematch_flags & fuchsia_driver_development::RestartRematchFlags::kRequested)) {
    return false;
  }

  if (driver_url() != requested_url &&
      !(rematch_flags & fuchsia_driver_development::RestartRematchFlags::kNonRequested)) {
    return false;
  }

  return true;
}

std::pair<std::string, Collection> Node::GetRemovalTrackerInfo() {
  return {MakeComponentMoniker(), collection_};
}

void Node::StopDriver() {
  ZX_ASSERT_MSG(GetNodeState() == NodeState::kWaitingOnChildren,
                "StopDriverComponent called in invalid node state: %s",
                GetShutdownHelper().NodeStateAsString());
  if (!HasDriver()) {
    return;
  }

  if (!driver_component_->is_bind_complete) {
    LOGF(WARNING, "Stopping driver '%s' for node '%s' while bind is in process",
         driver_component_->driver_url.c_str(), MakeComponentMoniker().c_str());
  }

  fidl::OneWayStatus result = driver_component_->driver->Stop();
  if (result.ok()) {
    return;  // We'll now wait for the channel to close
  }

  LOGF(ERROR, "Node: %s failed to stop driver: %s", name().c_str(),
       result.FormatDescription().data());
  // Continue to clear out the driver, since we can't talk to it.
  ClearHostDriver();
}

void Node::StopDriverComponent() {
  ZX_ASSERT_MSG(GetNodeState() == NodeState::kWaitingOnDriver,
                "StopDriverComponent called in invalid node state: %s",
                GetShutdownHelper().NodeStateAsString());

  if (!driver_component_) {
    return;
  }

  // Send an epitaph to the component manager and close the connection. The
  // server of a `ComponentController` protocol is expected to send an epitaph
  // before closing the associated connection.
  auto this_node = shared_from_this();
  driver_component_->component_controller_ref.Close(ZX_OK);
  if (!node_manager_.has_value()) {
    return;
  }
  node_manager_.value()->DestroyDriverComponent(
      *this_node,
      [self = this_node](fidl::WireUnownedResult<fcomponent::Realm::DestroyChild>& result) {
        if (!result.ok()) {
          auto error = result.error().FormatDescription();
          LOGF(ERROR, "Node: %s: Failed to send request to destroy component: %.*s",
               self->name_.c_str(), static_cast<int>(error.size()), error.data());
        }
        if (result->is_error() &&
            result->error_value() != fcomponent::wire::Error::kInstanceNotFound) {
          LOGF(ERROR, "Node: %.*s: Failed to destroy driver component: %u",
               static_cast<int>(self->name_.size()), self->name_.data(), result->error_value());
        }

        LOGF(INFO, "Destroyed driver component for %s", self->MakeComponentMoniker().c_str());
        self->driver_component_->is_destroyed = true;
        self->GetShutdownHelper().CheckNodeState();
      });
}

void Node::on_fidl_error(fidl::UnbindInfo info) {
  ClearHostDriver();

  // The only valid way a driver host should shut down the Driver channel
  // is with the ZX_OK epitaph.
  if (info.reason() != fidl::Reason::kPeerClosedWhileReading || info.status() != ZX_OK) {
    LOGF(ERROR, "Node: %s: driver channel shutdown with: %s", name().c_str(),
         info.FormatDescription().data());
  }

  if (GetNodeState() == NodeState::kWaitingOnDriver) {
    LOGF(INFO, "Node: %s: realm channel had expected shutdown.", MakeComponentMoniker().c_str());
    GetShutdownHelper().CheckNodeState();
    return;
  }

  if (GetNodeState() == NodeState::kWaitingOnDriverComponent) {
    LOGF(DEBUG, "Node: %s: driver channel had expected shutdown.", name().c_str());
    if (driver_component_) {
      driver_component_->is_destroyed = true;
    }
    GetShutdownHelper().CheckNodeState();
    return;
  }

  if (host_restart_on_crash_) {
    LOGF(WARNING, "Restarting node %s because of unexpected driver channel shutdown.",
         name().c_str());
    RestartNode();
    return;
  }

  LOGF(WARNING, "Removing node %s because of unexpected driver channel shutdown.", name().c_str());
  Remove(RemovalSet::kAll, nullptr);
}

Node::DriverComponent::DriverComponent(
    Node& node, std::string url,
    fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
    fidl::ClientEnd<fuchsia_driver_host::Driver> driver)
    : component_controller_ref(
          node.dispatcher_, std::move(controller), &node,
          [](Node* node, fidl::UnbindInfo info) {
            if (!info.is_user_initiated()) {
              LOGF(WARNING, "Removing node %s because of ComponentController binding closed: %s",
                   node->name().c_str(), info.FormatDescription().c_str());
              node->Remove(RemovalSet::kAll, nullptr);
            }
          }),
      driver(std::move(driver), node.dispatcher_, &node),
      driver_url(std::move(url)) {}

void Node::SetAndPublishInspect() {
  constexpr char kDeviceTypeString[] = "Device";
  constexpr char kCompositeDeviceTypeString[] = "Composite Device";

  std::vector<zx_device_prop_t> property_vector;
  for (auto& property : properties_) {
    if (property.key.is_int_value() && property.value.is_int_value()) {
      property_vector.push_back(zx_device_prop_t{
          .id = static_cast<uint16_t>(property.key.int_value()),
          .value = property.value.int_value(),
      });
    }
  }

  inspect_.SetStaticValues(MakeTopologicalPath(), GetProtocolId(properties_),
                           IsComposite() ? kCompositeDeviceTypeString : kDeviceTypeString, 0,
                           property_vector, "");
  if (zx::result result = inspect_.Publish(); result.is_error()) {
    LOGF(ERROR, "%s: Failed to publish inspect: %s", MakeTopologicalPath().c_str(),
         result.status_string());
  }
}

void Node::ConnectToDeviceFidl(ConnectToDeviceFidlRequestView request,
                               ConnectToDeviceFidlCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void Node::ConnectToController(ConnectToControllerRequestView request,
                               ConnectToControllerCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void Node::Bind(BindRequestView request, BindCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Node::Rebind(RebindRequestView request, RebindCompleter::Sync& completer) {
  std::optional<std::string> url;
  if (!request->driver.is_null() && !request->driver.empty()) {
    url = std::string(request->driver.get());
  }

  auto rebind_callback = [completer = completer.ToAsync()](zx::result<> result) mutable {
    if (result.is_ok()) {
      completer.ReplySuccess();
    } else {
      completer.ReplyError(result.error_value());
    }
  };

  if (kEnableCompositeNodeSpecRebind && type_ == NodeType::kComposite) {
    node_manager_.value()->RebindComposite(name_, url, std::move(rebind_callback));
    return;
  }

  RestartNodeWithRematch(url, std::move(rebind_callback));
}

void Node::UnbindChildren(UnbindChildrenCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void Node::ScheduleUnbind(ScheduleUnbindCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void Node::GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) {
  completer.ReplySuccess(fidl::StringView::FromExternal(MakeTopologicalPath()));
}

void Node::GetMinDriverLogSeverity(GetMinDriverLogSeverityCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void Node::SetMinDriverLogSeverity(SetMinDriverLogSeverityRequestView request,
                                   SetMinDriverLogSeverityCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

Devnode::Target Node::CreateDevfsPassthrough(
    std::optional<fidl::ClientEnd<fuchsia_device_fs::Connector>> connector,
    std::optional<fuchsia_device_fs::ConnectionType> connector_supports) {
  auto supported_by_connector =
      connector_supports.value_or(fuchsia_device_fs::ConnectionType::kDevice);
  return Devnode::PassThrough(
      fuchsia_device_fs::ConnectionType::kDevice,
      [connector = std::move(connector), supported_by_connector, node = weak_from_this(),
       node_name = name_](zx::channel server_end, fuchsia_device_fs::ConnectionType type) {
        // If the connector supports all of the requested types, connect with the connector.
        if (connector.has_value() && type == (supported_by_connector & type)) {
          return fidl::WireCall(connector.value())->Connect(std::move(server_end)).status();
        }

        if (type & fuchsia_device_fs::ConnectionType::kDevice ||
            type & fuchsia_device_fs::ConnectionType::kNode) {
          LOGF(WARNING, "Cannot include device or node for %s.", node_name.c_str());
        }

        if (!(type & fuchsia_device_fs::ConnectionType::kController)) {
          LOGF(WARNING, "Controller not requested for %s.", node_name.c_str());
          return ZX_ERR_NOT_SUPPORTED;
        }

        std::shared_ptr locked_node = node.lock();
        if (!locked_node) {
          LOGF(ERROR, "Node was freed before it was used for %s.", node_name.c_str());
          return ZX_ERR_BAD_STATE;
        }

        locked_node->dev_controller_bindings_.AddBinding(
            locked_node->dispatcher_,
            fidl::ServerEnd<fuchsia_device::Controller>{std::move(server_end)}, locked_node.get(),
            fidl::kIgnoreBindingClosure);
        return ZX_OK;
      });
}

}  // namespace dfv2
