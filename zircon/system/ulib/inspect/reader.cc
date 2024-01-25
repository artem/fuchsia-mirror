// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/inspect/cpp/reader.h>
#include <lib/inspect/cpp/vmo/block.h>
#include <lib/inspect/cpp/vmo/limits.h>
#include <lib/inspect/cpp/vmo/scanner.h>
#include <lib/inspect/cpp/vmo/snapshot.h>
#include <lib/stdcompat/optional.h>

#include <cstdint>
#include <iterator>
#include <set>
#include <stack>
#include <unordered_map>
#include <vector>

using inspect::internal::BlockIndex;
using inspect::internal::SnapshotTree;

namespace inspect {

namespace {

// Traverses the given hierarchy and passes pointers to any hierarchy containing a link to the
// callback.
//
// The callback is called before the passed hierarchy is traversed into, so it may be modified in
// the context of the callback.
void VisitHierarchiesWithLink(Hierarchy* root, fit::function<void(Hierarchy*)> callback) {
  root->Visit([&](const std::vector<std::string>& path, Hierarchy* hierarchy) {
    if (!hierarchy->node().links().empty()) {
      callback(hierarchy);
    }
    return true;
  });
}

// Iteratively snapshot individual inspectors and then snapshot the children of those inspectors,
// collecting the results in a single SnapshotTree.
//
// Consistency of individual Snapshots is guaranteed.
//
// Child Snapshots are guaranteed only to be taken consistently after their parent Snapshot.
//
// Between the initial Snapshot and reading lazy children, it is possible that the lazy child was
// deleted. In this case, the child snapshot will be missing.
fpromise::promise<SnapshotTree> SnapshotTreeFromInspector(Inspector insp) {
  SnapshotTree ret;
  if (ZX_OK != Snapshot::Create(insp.DuplicateVmo(), &ret.snapshot)) {
    return fpromise::make_result_promise<SnapshotTree>(fpromise::error());
  }

  // Sequence all promises to ensure depth-first traversal. Otherwise traversal may cause
  // all children for a Snapshot to be instantiated simultaneously.
  fpromise::sequencer seq;
  std::vector<fpromise::promise<SnapshotTree>> promises;
  auto child_names = insp.GetChildNames();
  for (const auto& child_name : child_names) {
    promises.emplace_back(
        insp.OpenChild(child_name)
            .and_then([](Inspector& insp) { return SnapshotTreeFromInspector(std::move(insp)); })
            .wrap_with(seq));
  }

  return fpromise::join_promise_vector(std::move(promises))
      .and_then([ret = std::move(ret), names = std::move(child_names)](
                    std::vector<fpromise::result<SnapshotTree>>& children) mutable
                -> fpromise::result<SnapshotTree> {
        ZX_ASSERT(names.size() == children.size());

        for (size_t i = 0; i < names.size(); i++) {
          if (children[i].is_ok()) {
            ret.children.emplace(std::move(names[i]), children[i].take_value());
          }
        }

        return fpromise::ok(std::move(ret));
      });
}

}  // namespace

namespace internal {

// A ParsedNode contains parsed information for a node.
// It is built iteratively as children and values are discovered.
//
// A ParsedNode is valid only if it has been initialized with a name and
// parent index (which happens when its OBJECT_VALUE block is read).
//
// A ParsedNode is "complete" when the number of children in the parsed
// hierarchy matches an expected count. At this point the Hierarchy may be
// removed and the ParsedNode discarded.
struct ParsedNode {
  // The node hierarchy being parsed out of the buffer.
  // Propertys and properties are parsed into here as they are read.
  Hierarchy hierarchy;

  // The number of children expected for this node.
  // The node is considered "complete" once the number of children in the
  // hierarchy matches this count.
  size_t children_count = 0;

  // The index of the parent, only valid if this node is initialized.
  BlockIndex parent;

  // Initializes the stored node with the given name and parent.
  void InitializeNode(std::string name, BlockIndex new_parent) {
    hierarchy.node_ptr()->set_name(std::move(name));
    parent = new_parent;
    initialized_ = true;
  }

  explicit operator bool() const { return initialized_; }

  bool is_complete() { return hierarchy.children().size() == children_count; }

 private:
  bool initialized_ = false;
};

// The |Reader| supports reading the contents of a |Snapshot|.
// This class constructs a hierarchy of nodes contained in the snapshot
// if the snapshot is valid.
// This |Reader| supports versions 1 and 2 of the Inspect Format.
class Reader {
 public:
  Reader(Snapshot snapshot) : snapshot_(std::move(snapshot)) {}

  // Read the contents of the snapshot and return the root node.
  fpromise::result<Hierarchy> Read();

 private:
  // Gets a pointer to the ParsedNode for the given index. A new ParsedObject
  // is created if one did not exist previously for the index.
  ParsedNode* GetOrCreate(BlockIndex index);

  void InnerScanBlocks();

  // Initialize an Object for the given BlockIndex.
  void InnerCreateObject(BlockIndex index, const Block* block);

  // Parse a numeric property block and attach it to the given parent.
  void InnerParseNumericOrArrayProperty(ParsedNode* parent, const Block* block);

  // Parse a property block and attach it to the given parent.
  void InnerParseProperty(ParsedNode* parent, const Block* block);

  // Parse a link block and attach it to the given parent.
  void InnerParseLink(ParsedNode* parent, const Block* block);

  // Load the contents of an extent chain into buf.
  void ReadExtents(BlockIndex head_extent, size_t remaining_length,
                   std::vector<uint8_t>* buf) const;

  // Helper to load a std::string from either a NAME or STRING_REFERENCE.
  cpp17::optional<std::string> GetAndValidateName(BlockIndex index);

  // Helper to interpret the given block as a NAME block and return a
  // copy of the name contents.
  static cpp17::optional<std::string> LoadName(const Block* block);

  // Helper to interpret the given block as a STRING_REFERENCE and return
  // a copy of the canonical value.
  cpp17::optional<std::string> LoadStringReference(const Block* block) const;

  // Contents of the read VMO.
  Snapshot snapshot_;

  // Map of block index to the parsed node being constructed for that address.
  std::unordered_map<BlockIndex, ParsedNode> parsed_nodes_;
};

cpp17::optional<std::string> Reader::LoadName(const Block* const block) {
  if (!block) {
    return {};
  }

  size_t capacity = PayloadCapacity(GetOrder(block));
  auto len = NameBlockFields::Length::Get<size_t>(block->header);
  // Do not parse the name if the declared length is greater than what the block can hold.
  if (len > capacity) {
    return {};
  }

  return std::string(block->payload_ptr(), len);
}

cpp17::optional<std::string> Reader::LoadStringReference(const Block* const block) const {
  if (!block) {
    return {};
  }

  std::vector<uint8_t> buffer;

  const auto total_length =
      StringReferenceBlockPayload::TotalLength::Get<size_t>(block->payload.u64);
  buffer.reserve(total_length);
  const auto max_inlinable_length =
      PayloadCapacity(GetOrder(block)) - StringReferenceBlockPayload::TotalLength::SizeInBytes();
  buffer.insert(buffer.cend(),
                block->payload_ptr() + StringReferenceBlockPayload::TotalLength::SizeInBytes(),
                block->payload_ptr() + StringReferenceBlockPayload::TotalLength::SizeInBytes() +
                    std::min(total_length, max_inlinable_length));

  if (total_length == buffer.size()) {
    return std::string{buffer.cbegin(), buffer.cend()};
  }

  ReadExtents(StringReferenceBlockFields::NextExtentIndex::Get<BlockIndex>(block->header),
              total_length - max_inlinable_length, &buffer);

  return std::string{buffer.cbegin(), buffer.cend()};
}

cpp17::optional<std::string> Reader::GetAndValidateName(BlockIndex index) {
  const auto* const block = internal::GetBlock(&snapshot_, index);
  if (!block) {
    return {};
  }

  switch (GetType(block)) {
    case BlockType::kName:
      return LoadName(block);
    case BlockType::kStringReference:
      return LoadStringReference(block);
    default:
      return {};
  }
}

void Reader::InnerScanBlocks() {
  ScanBlocks(snapshot_.data(), snapshot_.size(), [this](BlockIndex index, const Block* block) {
    BlockType type = GetType(block);
    if (index == 0) {
      if (type != BlockType::kHeader) {
        return false;
      }
    } else if (type == BlockType::kNodeValue) {
      // This block defines an Object, use the value to fill out the name of
      // the ParsedNode.
      InnerCreateObject(index, block);
    } else if (type == BlockType::kIntValue || type == BlockType::kUintValue ||
               type == BlockType::kDoubleValue || type == BlockType::kArrayValue ||
               type == BlockType::kBoolValue) {
      // This block defines a numeric property for an Object, parse the
      // property into the properties field of the ParsedNode.
      auto parent_index = ValueBlockFields::ParentIndex::Get<BlockIndex>(block->header);
      InnerParseNumericOrArrayProperty(GetOrCreate(parent_index), block);
    } else if (type == BlockType::kBufferValue) {
      // This block defines a property for an Object, parse the property
      // into the properties field of the ParsedNode.
      auto parent_index = ValueBlockFields::ParentIndex::Get<BlockIndex>(block->header);
      InnerParseProperty(GetOrCreate(parent_index), block);
    } else if (type == BlockType::kLinkValue) {
      // This block defines a link to an adjacent hierarchy stored outside the currently parsed one.
      // For now we parse and store the contents of the link with the NodeValue, and we will handle
      // merging trees in a separate step.
      auto parent_index = ValueBlockFields::ParentIndex::Get<BlockIndex>(block->header);
      InnerParseLink(GetOrCreate(parent_index), block);
    }

    return true;
  });
}

fpromise::result<Hierarchy> Reader::Read() {
  if (!snapshot_) {
    // Snapshot is invalid, return an error.
    return fpromise::error();
  }

  // Initialize the implicit root node, which uses index 0.
  ParsedNode root;
  root.InitializeNode("root", 0);
  parsed_nodes_.emplace(0, std::move(root));

  // Scan blocks into the parsed_node map. This creates ParsedNodes with
  // properties and an accurate count of the number of expected
  // children. ParsedNodes with a valid OBJECT_VALUE block are initialized
  // with a name and parent index.
  InnerScanBlocks();

  // Stack of completed nodes to process. Entries consist of the completed
  // Hierarchy and the block index of their parent.
  std::stack<std::pair<Hierarchy, BlockIndex>> complete_nodes;

  // Iterate over the map of parsed nodes and find those nodes that are
  // already "complete." These nodes are moved to the complete_nodes map for
  // bottom-up processing.
  for (auto it = parsed_nodes_.begin(); it != parsed_nodes_.end();) {
    if (!it->second) {
      // The node is not valid, ignore.
      it = parsed_nodes_.erase(it);
      continue;
    }

    if (it->second.is_complete()) {
      if (it->first == 0) {
        // The root is complete, return it.
        return fpromise::ok(std::move(it->second.hierarchy));
      }

      // The node is valid and complete, push it onto the stack.
      complete_nodes.push(std::make_pair(std::move(it->second.hierarchy), it->second.parent));
      it = parsed_nodes_.erase(it);
      continue;
    }

    ++it;
  }

  // Construct a valid hierarchy from the bottom up by attaching completed
  // nodes to their parent node. Once a parent becomes complete, add it to
  // the stack to recursively bubble the completed children towards the root.
  while (!complete_nodes.empty()) {
    auto obj = std::move(complete_nodes.top());
    complete_nodes.pop();

    // Get the parent node, which was created during block scanning.
    auto it = parsed_nodes_.find(obj.second);
    if (it == parsed_nodes_.end()) {
      // Parent node did not exist, ignore this node.
      continue;
    }
    auto* parent = &it->second;
    parent->hierarchy.add_child(std::move(obj.first));
    if (parent->is_complete()) {
      if (obj.second == 0) {
        // This was the last node that needed to be added to the root to complete it.
        // Return the root.
        return fpromise::ok(std::move(parent->hierarchy));
      }

      // The parent node is now complete, push it onto the stack.
      complete_nodes.push(std::make_pair(std::move(parent->hierarchy), parent->parent));
      parsed_nodes_.erase(it);
    }
  }

  // We processed all completed nodes but could not find a complete root,
  // return an error.
  return fpromise::error();
}

ParsedNode* Reader::GetOrCreate(BlockIndex index) {
  return &parsed_nodes_.emplace(index, ParsedNode()).first->second;
}

ArrayDisplayFormat ArrayBlockFormatToDisplay(ArrayBlockFormat format) {
  switch (format) {
    case ArrayBlockFormat::kLinearHistogram:
      return ArrayDisplayFormat::kLinearHistogram;
    case ArrayBlockFormat::kExponentialHistogram:
      return ArrayDisplayFormat::kExponentialHistogram;
    default:
      return ArrayDisplayFormat::kFlat;
  }
}

void Reader::InnerParseNumericOrArrayProperty(ParsedNode* parent, const Block* block) {
  auto name = GetAndValidateName(ValueBlockFields::NameIndex::Get<size_t>(block->header));
  if (!name.has_value()) {
    return;
  }

  auto* parent_node = parent->hierarchy.node_ptr();

  BlockType type = GetType(block);
  switch (type) {
    case BlockType::kIntValue:
      parent_node->add_property(
          PropertyValue(std::move(name.value()), IntPropertyValue(block->payload.i64)));
      return;
    case BlockType::kUintValue:
      parent_node->add_property(
          PropertyValue(std::move(name.value()), UintPropertyValue(block->payload.u64)));
      return;
    case BlockType::kDoubleValue:
      parent_node->add_property(
          PropertyValue(std::move(name.value()), DoublePropertyValue(block->payload.f64)));
      return;
    case BlockType::kBoolValue:
      parent_node->add_property(
          PropertyValue(std::move(name.value()), BoolPropertyValue(block->payload.u64)));
      return;
    case BlockType::kArrayValue: {
      const auto entry_type = ArrayBlockPayload::EntryType::Get<BlockType>(block->payload.u64);
      const auto count = ArrayBlockPayload::Count::Get<uint8_t>(block->payload.u64);
      if (GetArraySlot<const int64_t>(block, count - 1) == nullptr) {
        // Block does not store the entire array.
        return;
      }

      auto array_format = ArrayBlockFormatToDisplay(
          ArrayBlockPayload::Flags::Get<ArrayBlockFormat>(block->payload.u64));

      if (entry_type == BlockType::kIntValue) {
        std::vector<int64_t> values;
        values.reserve(count);
        std::copy(GetArraySlot<const int64_t>(block, 0), GetArraySlot<const int64_t>(block, count),
                  std::back_inserter(values));

        parent_node->add_property(
            PropertyValue(std::move(name.value()), IntArrayValue(std::move(values), array_format)));
      } else if (entry_type == BlockType::kUintValue) {
        std::vector<uint64_t> values;
        values.reserve(count);
        std::copy(GetArraySlot<const uint64_t>(block, 0),
                  GetArraySlot<const uint64_t>(block, count), std::back_inserter(values));

        parent_node->add_property(PropertyValue(std::move(name.value()),
                                                UintArrayValue(std::move(values), array_format)));
      } else if (entry_type == BlockType::kDoubleValue) {
        std::vector<double> values;
        values.reserve(count);
        std::copy(GetArraySlot<const double>(block, 0), GetArraySlot<const double>(block, count),
                  std::back_inserter(values));

        parent_node->add_property(PropertyValue(std::move(name.value()),
                                                DoubleArrayValue(std::move(values), array_format)));
      } else if (entry_type == BlockType::kStringReference) {
        std::vector<std::string> values(count);
        for (auto i = 0; i < count; i++) {
          const auto value_index = GetArraySlotForString(block, i);
          if (!value_index.has_value()) {
            continue;
          }

          if (value_index.value() == kEmptyStringSlotIndex) {
            // values.at(i) is defaulted to std::string{}, don't need to set it
            continue;
          }

          auto read_string =
              LoadStringReference(internal::GetBlock(&snapshot_, value_index.value()));
          if (!read_string.has_value()) {
            continue;
          }

          values.at(i) = std::move(read_string.value());
        }

        parent_node->add_property(PropertyValue(std::move(name.value()),
                                                StringArrayValue(std::move(values), array_format)));
      }
      return;
    }
    default:
      return;
  }
}

void Reader::ReadExtents(const BlockIndex head_extent, size_t remaining_length,
                         std::vector<uint8_t>* buf) const {
  auto* extent = internal::GetBlock(&snapshot_, head_extent);
  while (remaining_length > 0) {
    if (!extent || GetType(extent) != BlockType::kExtent) {
      break;
    }
    size_t len = std::min(remaining_length, PayloadCapacity(GetOrder(extent)));
    buf->insert(buf->cend(), extent->payload_ptr(), extent->payload_ptr() + len);
    remaining_length -= len;

    BlockIndex next_extent = ExtentBlockFields::NextExtentIndex::Get<BlockIndex>(extent->header);

    extent = internal::GetBlock(&snapshot_, next_extent);
  }
}

void Reader::InnerParseProperty(ParsedNode* parent, const Block* block) {
  auto name = GetAndValidateName(ValueBlockFields::NameIndex::Get<size_t>(block->header));
  if (!name.has_value()) {
    return;
  }

  // Do not allow reading more bytes than exist in the buffer for any property. This safeguards
  // against cycles and excessive memory usage.
  size_t remaining_length = std::min(
      snapshot_.size(), PropertyBlockPayload::TotalLength::Get<size_t>(block->payload.u64));
  BlockIndex cur_extent = PropertyBlockPayload::ExtentIndex::Get<BlockIndex>(block->payload.u64);

  std::vector<uint8_t> buf;
  buf.reserve(remaining_length);
  ReadExtents(cur_extent, remaining_length, &buf);

  auto* parent_node = parent->hierarchy.node_ptr();
  if (PropertyBlockPayload::Flags::Get<uint8_t>(block->payload.u64) &
      static_cast<uint8_t>(PropertyBlockFormat::kBinary)) {
    parent_node->add_property(
        inspect::PropertyValue(std::move(name.value()), inspect::ByteVectorPropertyValue(buf)));
  } else {
    parent_node->add_property(inspect::PropertyValue(
        std::move(name.value()),
        inspect::StringPropertyValue(std::string(buf.cbegin(), buf.cend()))));
  }
}

void Reader::InnerParseLink(ParsedNode* parent, const Block* block) {
  auto name = GetAndValidateName(ValueBlockFields::NameIndex::Get<size_t>(block->header));
  if (name->empty()) {
    return;
  }

  auto content =
      GetAndValidateName(LinkBlockPayload::ContentIndex::Get<size_t>(block->payload.u64));
  if (content->empty()) {
    return;
  }

  LinkDisposition disposition;
  switch (LinkBlockPayload::Flags::Get<LinkBlockDisposition>(block->payload.u64)) {
    case LinkBlockDisposition::kChild:
      disposition = LinkDisposition::kChild;
      break;
    case LinkBlockDisposition::kInline:
      disposition = LinkDisposition::kInline;
      break;
  }

  parent->hierarchy.node_ptr()->add_link(
      LinkValue(std::move(name.value()), std::move(content.value()), disposition));
}

void Reader::InnerCreateObject(BlockIndex index, const Block* block) {
  auto name = GetAndValidateName(ValueBlockFields::NameIndex::Get<BlockIndex>(block->header));
  if (!name.has_value()) {
    return;
  }
  auto* parsed_node = GetOrCreate(index);
  auto parent_index = ValueBlockFields::ParentIndex::Get<BlockIndex>(block->header);
  parsed_node->InitializeNode(std::move(name.value()), parent_index);
  if (parent_index != index) {
    // Only link to a parent if the parent can be valid (not index 0).
    auto* parent = GetOrCreate(parent_index);
    parent->children_count += 1;
  }
}

fpromise::result<Hierarchy> ReadFromSnapshotTree(const SnapshotTree& tree) {
  auto read_result = internal::Reader(tree.snapshot).Read();
  if (!read_result.is_ok()) {
    return read_result;
  }

  auto ret = read_result.take_value();

  VisitHierarchiesWithLink(&ret, [&](Hierarchy* cur) {
    for (const auto& link : cur->node().links()) {
      auto it = tree.children.find(link.content());
      if (it == tree.children.end()) {
        cur->add_missing_value(MissingValueReason::kLinkNotFound, link.name());
        continue;
      }

      // TODO(crjohns): Remove recursion, or limit depth.
      auto next_result = ReadFromSnapshotTree(it->second);
      if (!next_result.is_ok()) {
        cur->add_missing_value(MissingValueReason::kLinkHierarchyParseFailure, link.name());
        continue;
      }

      if (link.disposition() == LinkDisposition::kChild) {
        // The linked hierarchy is a child of this node, add it to the list of children.
        next_result.value().node_ptr()->set_name(link.name());
        cur->add_child(next_result.take_value());
      } else if (link.disposition() == LinkDisposition::kInline) {
        // The linked hierarchy is meant to have its contents inlined into this node.
        auto next = next_result.take_value();

        // Move properties into this node.
        for (auto& prop : next.node_ptr()->take_properties()) {
          cur->node_ptr()->add_property(std::move(prop));
        }

        // Move children into this node.
        for (auto& child : next.take_children()) {
          cur->add_child(std::move(child));
        }

        // Copy missing values.
        for (const auto& missing : next.missing_values()) {
          cur->add_missing_value(missing.reason, missing.name);
        }

        // There will be no further links that have not already been processed, since we recursively
        // get the hierarchy from a SnapshotTree.
      } else {
        cur->add_missing_value(MissingValueReason::kLinkInvalid, link.name());
      }
    }
  });

  return fpromise::ok(std::move(ret));
}

}  // namespace internal

fpromise::result<Hierarchy> ReadFromSnapshot(Snapshot snapshot) {
  internal::Reader reader(std::move(snapshot));
  return reader.Read();
}

fpromise::result<Hierarchy> ReadFromVmo(const zx::vmo& vmo) {
  inspect::Snapshot snapshot;
  if (inspect::Snapshot::Create(std::move(vmo), &snapshot) != ZX_OK) {
    return fpromise::error();
  }
  return ReadFromSnapshot(std::move(snapshot));
}

fpromise::result<Hierarchy> ReadFromBuffer(std::vector<uint8_t> buffer) {
  inspect::Snapshot snapshot;
  if (inspect::Snapshot::Create(BackingBuffer(std::move(buffer)), &snapshot) != ZX_OK) {
    // TODO(https://fxbug.dev/42124075): Best-effort read of invalid snapshots.
    return fpromise::error();
  }
  return ReadFromSnapshot(std::move(snapshot));
}

fpromise::promise<Hierarchy> ReadFromInspector(Inspector inspector) {
  return SnapshotTreeFromInspector(std::move(inspector)).and_then(internal::ReadFromSnapshotTree);
}

}  // namespace inspect
