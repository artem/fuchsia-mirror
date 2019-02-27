// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ledger/bin/storage/impl/btree/tree_node.h"

#include <algorithm>
#include <utility>

#include <lib/callback/waiter.h>
#include <lib/fit/function.h>
#include <lib/fsl/socket/strings.h>
#include <lib/fxl/logging.h>
#include <lib/fxl/memory/ref_counted.h>
#include <lib/fxl/memory/ref_ptr.h>
#include <lib/fxl/strings/string_printf.h>

#include "peridot/lib/convert/convert.h"
#include "src/ledger/bin/storage/impl/btree/encoding.h"
#include "src/ledger/bin/storage/impl/object_digest.h"
#include "src/ledger/bin/storage/public/constants.h"
#include "src/ledger/bin/storage/public/types.h"

namespace storage {
namespace btree {

TreeNode::TreeNode(PageStorage* page_storage, ObjectIdentifier identifier,
                   uint8_t level, std::vector<Entry> entries,
                   std::map<size_t, ObjectIdentifier> children)
    : page_storage_(page_storage),
      identifier_(std::move(identifier)),
      level_(level),
      entries_(std::move(entries)),
      children_(std::move(children)) {
  FXL_DCHECK(children_.empty() || children_.cbegin()->first <= entries_.size());
}

TreeNode::~TreeNode() {}

void TreeNode::FromIdentifier(
    PageStorage* page_storage, ObjectIdentifier identifier,
    fit::function<void(Status, std::unique_ptr<const TreeNode>)> callback) {
  page_storage->GetObject(
      identifier, PageStorage::Location::NETWORK,
      [page_storage, identifier, callback = std::move(callback)](
          Status status, std::unique_ptr<const Object> object) mutable {
        if (status != Status::OK) {
          callback(status, nullptr);
          return;
        }
        std::unique_ptr<const TreeNode> node;
        status = FromObject(page_storage, std::move(identifier),
                            std::move(object), &node);
        callback(status, std::move(node));
      });
}

void TreeNode::Empty(PageStorage* page_storage,
                     fit::function<void(Status, ObjectIdentifier)> callback) {
  FromEntries(page_storage, 0u, std::vector<Entry>(),
              std::map<size_t, ObjectIdentifier>(), std::move(callback));
}

void TreeNode::FromEntries(
    PageStorage* page_storage, uint8_t level, const std::vector<Entry>& entries,
    const std::map<size_t, ObjectIdentifier>& children,
    fit::function<void(Status, ObjectIdentifier)> callback) {
  FXL_DCHECK(children.empty() || children.rbegin()->first <= entries.size());
  ObjectReferencesAndPriority tree_references;
  for (const auto& [size, identifier] : children) {
    const auto& digest = identifier.object_digest();
    FXL_DCHECK(storage::IsDigestValid(digest));
    if (!GetObjectDigestInfo(digest).is_inlined()) {
      // Node-node references are always treated as eager.
      tree_references.emplace(digest, KeyPriority::EAGER);
    }
  }
  for (const auto& entry : entries) {
    const auto& digest = entry.object_identifier.object_digest();
    if (!GetObjectDigestInfo(digest).is_inlined()) {
      tree_references.emplace(digest, entry.priority);
    }
  }
  std::string encoding = EncodeNode(level, entries, children);
  page_storage->AddObjectFromLocal(
      ObjectType::TREE_NODE, storage::DataSource::Create(std::move(encoding)),
      std::move(tree_references), std::move(callback));
}

int TreeNode::GetKeyCount() const { return entries_.size(); }

Status TreeNode::GetEntry(int index, Entry* entry) const {
  FXL_DCHECK(index >= 0 && index < GetKeyCount());
  *entry = entries_[index];
  return Status::OK;
}

void TreeNode::GetChild(
    int index,
    fit::function<void(Status, std::unique_ptr<const TreeNode>)> callback)
    const {
  FXL_DCHECK(index >= 0 && index <= GetKeyCount());
  const auto it = children_.find(index);
  if (it == children_.end()) {
    callback(Status::NO_SUCH_CHILD, nullptr);
    return;
  }
  return FromIdentifier(page_storage_, it->second, std::move(callback));
}

Status TreeNode::FindKeyOrChild(convert::ExtendedStringView key,
                                int* index) const {
  if (key.empty()) {
    *index = 0;
    return !entries_.empty() && entries_[0].key.empty() ? Status::OK
                                                        : Status::KEY_NOT_FOUND;
  }
  auto it =
      std::lower_bound(entries_.begin(), entries_.end(), key,
                       [](const Entry& entry, convert::ExtendedStringView key) {
                         return entry.key < key;
                       });
  if (it == entries_.end()) {
    *index = entries_.size();
    return Status::KEY_NOT_FOUND;
  }
  *index = it - entries_.begin();
  if (it->key == key) {
    return Status::OK;
  }
  return Status::KEY_NOT_FOUND;
}

const ObjectIdentifier& TreeNode::GetIdentifier() const { return identifier_; }

Status TreeNode::FromObject(PageStorage* page_storage,
                            ObjectIdentifier identifier,
                            std::unique_ptr<const Object> object,
                            std::unique_ptr<const TreeNode>* node) {
  fxl::StringView data;
  Status status = object->GetData(&data);
  if (status != Status::OK) {
    return status;
  }
  uint8_t level;
  std::vector<Entry> entries;
  std::map<size_t, ObjectIdentifier> children;
  if (!DecodeNode(data, &level, &entries, &children)) {
    return Status::FORMAT_ERROR;
  }
  node->reset(new TreeNode(page_storage, std::move(identifier), level,
                           std::move(entries), std::move(children)));
  return Status::OK;
}

}  // namespace btree
}  // namespace storage
