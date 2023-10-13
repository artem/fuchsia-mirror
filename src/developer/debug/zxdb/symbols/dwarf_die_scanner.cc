// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/symbols/dwarf_die_scanner.h"

#include <lib/syslog/cpp/macros.h>

#include "src/developer/debug/zxdb/symbols/dwarf_tag.h"
#include "src/developer/debug/zxdb/symbols/dwarf_unit.h"

namespace zxdb {

DwarfDieScanner::DwarfDieScanner(const DwarfUnit& unit) : unit_(unit) {
  die_count_ = unit_.GetDieCount();

  // We prefer not to reallocate and normally the C++ component depth is < 8.
  tree_stack_.reserve(8);
}

DwarfDieScanner::~DwarfDieScanner() = default;

const llvm::DWARFDebugInfoEntry* DwarfDieScanner::Prepare() {
  if (done())
    return nullptr;

  cur_die_ = unit_.GetLLVMDieAtIndex(die_index_).getDebugInfoEntry();

  uint32_t parent_idx = cur_die_->getParentIdx().value_or(kNoParent);

  while (!tree_stack_.empty() && tree_stack_.back().index != parent_idx)
    tree_stack_.pop_back();
  tree_stack_.emplace_back(die_index_, false);

  // Fix up the inside function flag.
  switch (static_cast<DwarfTag>(cur_die_->getTag())) {
    case DwarfTag::kLexicalBlock:
    case DwarfTag::kVariable:
      // Inherits from previous. For a block and a variable there should always be the parent,
      // since at least there's the unit root DIE.
      FX_DCHECK(tree_stack_.size() >= 2);
      tree_stack_.back().inside_function = tree_stack_[tree_stack_.size() - 2].inside_function;
      break;
    case DwarfTag::kSubprogram:
    case DwarfTag::kInlinedSubroutine:
      tree_stack_.back().inside_function = true;
      break;
    default:
      tree_stack_.back().inside_function = false;
      break;
  }

  return cur_die_;
}

void DwarfDieScanner::Advance() {
  FX_DCHECK(!done());

  die_index_++;
}

uint32_t DwarfDieScanner::GetParentIndex(uint32_t index) const {
  return unit_.GetLLVMDieAtIndex(index).getDebugInfoEntry()->getParentIdx().value_or(kNoParent);
}

}  // namespace zxdb
