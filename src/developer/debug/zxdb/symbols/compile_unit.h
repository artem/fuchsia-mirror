// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_COMPILE_UNIT_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_COMPILE_UNIT_H_

#include <optional>

#include "src/developer/debug/zxdb/symbols/dwarf_lang.h"
#include "src/developer/debug/zxdb/symbols/symbol.h"
#include "src/lib/fxl/memory/weak_ptr.h"

namespace zxdb {

class ModuleSymbols;

// Represents a DWARF "unit" DIE, both regular ones and the "skeleton" ones (skeleton units are
// in the main binary and refer to a .dwo file containing the unit info). This is the root of all
// other Debug Information Entries (DIEs) in the unit, and is contained inside the DwarfUnit object.
//
// See the DwarfUnit object which contains the other unit-related stuff like line tables (see that
// class for a longer discussion). Usually "unit" offsets are relative to the DwarfUnit and NOT this
// class.
class CompileUnit final : public Symbol {
 public:
  // Module. This can be null if the module was unloaded while somebody held onto this symbol. It
  // is also null in many unit testing situations where mock symbols are created.
  const fxl::WeakPtr<ModuleSymbols>& module() const { return module_; }

  // Skeleton units refer a .dwo file containing the debug symbols. But they have the same basic
  // attributes as a regular compilation unit.
  bool is_skeleton() const { return tag() == DwarfTag::kSkeletonUnit; }

  // See class comment above. Can be null for synthetic symbols and for testing.
  DwarfUnit* dwarf_unit() const { return dwarf_unit_.get(); }

  DwarfLang language() const { return language_; }

  // The file name that generated this unit.
  const std::string& name() const { return name_; }

  // Returns the DW_AT_addr_base attribute on the unit, if given. This attribute points to the
  // beginning of the compilation unit's contribution to the .debug_addr section of the module. It
  // is used for evaluating some DWARF expressions.
  const std::optional<uint64_t>& addr_base() const { return addr_base_; }

  // Compilation units have a lot of other stuff which we currently have no need for. These can be
  // added here as needed.

 protected:
  // Symbol protected overrides.
  const CompileUnit* AsCompileUnit() const override { return this; }

 private:
  FRIEND_REF_COUNTED_THREAD_SAFE(CompileUnit);
  FRIEND_MAKE_REF_COUNTED(CompileUnit);

  explicit CompileUnit(DwarfTag tag, fxl::WeakPtr<ModuleSymbols> module,
                       fxl::RefPtr<DwarfUnit> dwarf_unit, DwarfLang lang, std::string name,
                       const std::optional<uint64_t>& addr_base);
  ~CompileUnit() override;

  fxl::WeakPtr<ModuleSymbols> module_;

  fxl::RefPtr<DwarfUnit> dwarf_unit_;

  DwarfLang language_ = DwarfLang::kNone;
  std::string name_;
  std::optional<uint64_t> addr_base_;
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_COMPILE_UNIT_H_
