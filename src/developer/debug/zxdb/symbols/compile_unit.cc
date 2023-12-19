// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/symbols/compile_unit.h"

namespace zxdb {

CompileUnit::CompileUnit(DwarfTag tag, fxl::WeakPtr<ModuleSymbols> module,
                         fxl::RefPtr<DwarfUnit> dwarf_unit, DwarfLang lang, std::string name,
                         const std::optional<uint64_t>& addr_base)
    : Symbol(tag),
      module_(std::move(module)),
      dwarf_unit_(std::move(dwarf_unit)),
      language_(lang),
      name_(std::move(name)),
      addr_base_(addr_base) {}

CompileUnit::~CompileUnit() = default;

}  // namespace zxdb
