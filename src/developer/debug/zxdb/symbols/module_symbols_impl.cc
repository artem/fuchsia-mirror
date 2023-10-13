// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/symbols/module_symbols_impl.h"

#include <stdio.h>

#include <algorithm>
#include <memory>

#include "llvm/DebugInfo/DIContext.h"
#include "llvm/DebugInfo/DWARF/DWARFContext.h"
#include "llvm/DebugInfo/DWARF/DWARFUnit.h"
#include "llvm/Object/Binary.h"
#include "llvm/Object/ELFObjectFile.h"
#include "llvm/Object/ObjectFile.h"
#include "src/developer/debug/shared/largest_less_or_equal.h"
#include "src/developer/debug/shared/logging/logging.h"
#include "src/developer/debug/shared/message_loop.h"
#include "src/developer/debug/zxdb/common/file_util.h"
#include "src/developer/debug/zxdb/common/string_util.h"
#include "src/developer/debug/zxdb/symbols/compile_unit.h"
#include "src/developer/debug/zxdb/symbols/dwarf_binary_impl.h"
#include "src/developer/debug/zxdb/symbols/dwarf_expr_eval.h"
#include "src/developer/debug/zxdb/symbols/dwarf_symbol_factory.h"
#include "src/developer/debug/zxdb/symbols/elf_symbol.h"
#include "src/developer/debug/zxdb/symbols/file_line.h"
#include "src/developer/debug/zxdb/symbols/find_line.h"
#include "src/developer/debug/zxdb/symbols/function.h"
#include "src/developer/debug/zxdb/symbols/input_location.h"
#include "src/developer/debug/zxdb/symbols/line_details.h"
#include "src/developer/debug/zxdb/symbols/line_table_impl.h"
#include "src/developer/debug/zxdb/symbols/location.h"
#include "src/developer/debug/zxdb/symbols/resolve_options.h"
#include "src/developer/debug/zxdb/symbols/symbol_context.h"
#include "src/developer/debug/zxdb/symbols/symbol_data_provider.h"
#include "src/developer/debug/zxdb/symbols/unit_symbol_factory.h"
#include "src/developer/debug/zxdb/symbols/variable.h"
#include "src/lib/elflib/elflib.h"

namespace zxdb {

namespace {

// When looking for symbol matches, don't consider any symbol further than this from the looked-up
// location to be a match. We don't want this number to be too large as users can be confused by
// seeing a name for a symbol that's unrelated to the address at hand.
//
// However, when dealing with unsymbolized code, the nearest previous Elf symbol name can be a
// valuable hint about the location of an address.
constexpr uint64_t kMaxElfOffsetForMatch = 4096;

// Implementation of SymbolDataProvider that returns no memory or registers. This is used when
// evaluating global variables' location expressions which normally just declare an address. See
// LocationForVariable().
class GlobalSymbolDataProvider : public SymbolDataProvider {
 public:
  static Err GetContextError() {
    return Err(
        "Global variable requires register or memory data to locate. "
        "Please file a bug with a repro.");
  }

  // SymbolDataProvider implementation.
  debug::Arch GetArch() override { return debug::Arch::kUnknown; }
  void GetRegisterAsync(debug::RegisterID, GetRegisterCallback callback) override {
    debug::MessageLoop::Current()->PostTask(
        FROM_HERE, [cb = std::move(callback)]() mutable { cb(GetContextError(), {}); });
  }
  void GetFrameBaseAsync(GetFrameBaseCallback callback) override {
    debug::MessageLoop::Current()->PostTask(
        FROM_HERE, [cb = std::move(callback)]() mutable { cb(GetContextError(), 0); });
  }
  void GetMemoryAsync(uint64_t address, uint32_t size, GetMemoryCallback callback) override {
    debug::MessageLoop::Current()->PostTask(FROM_HERE, [cb = std::move(callback)]() mutable {
      cb(GetContextError(), std::vector<uint8_t>());
    });
  }
  void WriteMemory(uint64_t address, std::vector<uint8_t> data, WriteCallback cb) override {
    debug::MessageLoop::Current()->PostTask(
        FROM_HERE, [cb = std::move(cb)]() mutable { cb(GetContextError()); });
  }
};

// The order of the parameters matters because "line 0" is handled in "greedy" mode only for the
// candidate line. If the caller is asking about an address that matches line 0, we don't want to
// expand that past line boundaries, but we do want to expand other lines actoss line 0 in greedy
// mode.
bool SameFileLine(const llvm::DWARFDebugLine::Row& reference,
                  const llvm::DWARFDebugLine::Row& candidate, bool greedy) {
  // EndSequence entries don't have files or lines associated with them, it's just a marker. The
  // table will report the previous line's file+line as a side-effect fo the way it's encoded, so
  // explicitly fail matching for these.
  if (reference.EndSequence || candidate.EndSequence)
    return false;

  if (greedy && candidate.Line == 0)
    return true;
  return reference.File == candidate.File && reference.Line == candidate.Line;
}

// Determines if the given input location references a special identifier of the given type. If it
// does, returns the name of that symbol. If it does not, returns a null optional.
std::optional<std::string> GetSpecialInputLocation(const InputLocation& loc, SpecialIdentifier si) {
  if (loc.type != InputLocation::Type::kName || loc.name.components().size() != 1)
    return std::nullopt;

  if (loc.name.components()[0].special() == si)
    return loc.name.components()[0].name();

  return std::nullopt;
}

// Returns true if the given input references the special "main" function annotation.
bool ReferencesMainFunction(const InputLocation& loc) {
  if (loc.type != InputLocation::Type::kName || loc.name.components().size() != 1)
    return false;
  return loc.name.components()[0].special() == SpecialIdentifier::kMain;
}

// Returns true if any component of this identifier isn't supported via lookup in the
// ModuleSymbolsImpl.
bool HasOnlySupportedSpecialIdentifierTypes(const Identifier& ident) {
  for (const auto& comp : ident.components()) {
    switch (comp.special()) {
      case SpecialIdentifier::kNone:
      case SpecialIdentifier::kElf:
      case SpecialIdentifier::kPlt:
      case SpecialIdentifier::kAnon:
        break;  // Normal boring component.
      case SpecialIdentifier::kEscaped:
        FX_NOTREACHED();  // "Escaped" annotations shouldn't appear in identifiers.
        break;
      case SpecialIdentifier::kMain:
        // "$main" is supported only when it's the only component ("foo::$main" is invalid).
        return ident.components().size() == 1;
      case SpecialIdentifier::kRegister:
      case SpecialIdentifier::kZxdb:
        return false;  // Can't look up registers in the symbols.
      case SpecialIdentifier::kLast:
        FX_NOTREACHED();  // Not supposed to be a valid value.
        return false;
    }
  }
  return true;
}

}  // namespace

ModuleSymbolsImpl::ModuleSymbolsImpl(std::unique_ptr<DwarfBinaryImpl> binary,
                                     const std::string& build_dir, bool create_index)
    : binary_(std::move(binary)), build_dir_(build_dir), weak_factory_(this) {
  symbol_factory_ = fxl::MakeRefCounted<DwarfSymbolFactory>(GetWeakPtr());
  FillElfSymbols();

  if (create_index) {
    // We could consider creating a new binary/object file just for indexing. The indexing will page
    // all of the binary in, and most of it won't be needed again (it will be paged back in slowly,
    // savings may make such a change worth it for large programs as needed).
    //
    // Although it will be slightly slower to create, the memory savings may make such a change
    // worth it for large programs.
    index_.CreateIndex(*binary_);
  }
}

ModuleSymbolsImpl::~ModuleSymbolsImpl() = default;

fxl::WeakPtr<ModuleSymbolsImpl> ModuleSymbolsImpl::GetWeakPtr() {
  return weak_factory_.GetWeakPtr();
}

ModuleSymbolStatus ModuleSymbolsImpl::GetStatus() const {
  ModuleSymbolStatus status;
  status.build_id = binary_->GetBuildID();
  status.base = 0;               // We don't know this, only ProcessSymbols does.
  status.symbols_loaded = true;  // Since this instance exists at all.
  status.functions_indexed = index_.CountSymbolsIndexed();
  status.files_indexed = index_.files_indexed();
  status.symbol_file = binary_->GetName();
  return status;
}

std::time_t ModuleSymbolsImpl::GetModificationTime() const {
  return binary_->GetModificationTime();
}

std::string ModuleSymbolsImpl::GetBuildDir() const { return build_dir_; }

uint64_t ModuleSymbolsImpl::GetMappedLength() const { return binary_->GetMappedLength(); }

const SymbolFactory* ModuleSymbolsImpl::GetSymbolFactory() const { return symbol_factory_.get(); }

std::vector<Location> ModuleSymbolsImpl::ResolveInputLocation(const SymbolContext& symbol_context,
                                                              const InputLocation& input_location,
                                                              const ResolveOptions& options) const {
  // Thie skip_function_prologue option requires that symbolize be set.
  FX_DCHECK(!options.skip_function_prologue || options.symbolize);

  switch (input_location.type) {
    case InputLocation::Type::kNone:
      return std::vector<Location>();
    case InputLocation::Type::kLine:
      return ResolveLineInputLocation(symbol_context, input_location, options);
    case InputLocation::Type::kName:
      return ResolveSymbolInputLocation(symbol_context, input_location, options);
    case InputLocation::Type::kAddress:
      return ResolveAddressInputLocation(symbol_context, input_location, options);
  }
}

fxl::RefPtr<DwarfUnit> ModuleSymbolsImpl::GetDwarfUnit(const SymbolContext& symbol_context,
                                                       uint64_t absolute_address) const {
  return binary_->UnitForRelativeAddress(symbol_context.AbsoluteToRelative(absolute_address));
}

LineDetails ModuleSymbolsImpl::LineDetailsForAddress(const SymbolContext& symbol_context,
                                                     uint64_t absolute_address, bool greedy) const {
  uint64_t relative_address = symbol_context.AbsoluteToRelative(absolute_address);
  auto unit = binary_->UnitForRelativeAddress(relative_address);
  if (!unit)
    return LineDetails();

  // TODO(brettw) this should use our LineTable wrapper instead of LLVM's so it can be mocked.
  const llvm::DWARFDebugLine::LineTable* line_table = unit->GetLLVMLineTable();
  if (!line_table || line_table->Rows.empty())
    return LineDetails();

  const auto& rows = line_table->Rows;
  uint32_t found_row_index = line_table->lookupAddress({relative_address});

  // The row could be not found or it could be in a "nop" range indicated by an "end sequence"
  // marker. For padding between functions, the compiler will insert a row with this marker to
  // indicate everything until the next address isn't an instruction. With this flag, the other
  // information on the line will be irrelevant (in practice it will be the same as for the previous
  // entry).
  if (found_row_index == line_table->UnknownRowIndex || rows[found_row_index].EndSequence)
    return LineDetails();

  // Adjust the beginning and end ranges to include all matching entries of the same line.
  //
  // Note that this code must not try to hide "line 0" entries (corresponding to compiler-generated
  // code). This function is used by the stepping code which has its own handling for these ranges.
  // Trying to put "line 0" code in with the previous or next entry (what some other code does that
  // tries to hide this from the user) will confuse the stepping code which will always step through
  // these instructions.
  uint32_t first_row_index = found_row_index;
  while (first_row_index > 0 &&
         SameFileLine(rows[found_row_index], rows[first_row_index - 1], greedy)) {
    first_row_index--;
  }
  uint32_t last_row_index = found_row_index;  // Inclusive.
  while (last_row_index < rows.size() - 1 &&
         SameFileLine(rows[found_row_index], rows[last_row_index + 1], greedy)) {
    last_row_index++;
  }

  // Resolve the file name. Skip for "line 0" entries which are compiled-generated code not
  // associated with a line entry, leaving the file name and compilation directory empty. Typically
  // there will be a file if we ask, but that's leftover from the previous row in the table by the
  // state machine and is not relevant.
  std::string file_name;
  std::string compilation_dir;
  if (rows[first_row_index].Line) {
    line_table->getFileNameByIndex(rows[first_row_index].File, "",
                                   llvm::DILineInfoSpecifier::FileLineInfoKind::RelativeFilePath,
                                   file_name);
    if (!build_dir_.empty()) {
      compilation_dir = build_dir_;
    } else {
      compilation_dir = unit->GetCompilationDir();
    }
  }

  LineDetails result(
      FileLine(NormalizePath(file_name), compilation_dir, rows[first_row_index].Line));

  // Add entries for each row. The last row doesn't count because it should be
  // an end_sequence marker to provide the ending size of the previous entry.
  // So never include that.
  for (uint32_t i = first_row_index; i <= last_row_index && i < rows.size() - 1; i++) {
    // With loop bounds we can always dereference @ i + 1.
    if (rows[i + 1].Address < rows[i].Address)
      break;  // Going backwards, corrupted so give up.

    LineDetails::LineEntry entry;
    entry.column = rows[i].Column;
    entry.range = AddressRange(symbol_context.RelativeToAbsolute(rows[i].Address.Address),
                               symbol_context.RelativeToAbsolute(rows[i + 1].Address.Address));
    result.entries().push_back(entry);
  }

  return result;
}

std::vector<std::string> ModuleSymbolsImpl::FindFileMatches(std::string_view name) const {
  // If both build_dir_ and name are absolute, convert it to a relative path against build_dir_
  // because the paths in the index are relative.
  if (!build_dir_.empty() && build_dir_[0] == '/' && name[0] == '/') {
    return index_.FindFileMatches(PathRelativeTo(name, build_dir_).string());
  }
  return index_.FindFileMatches(name);
}

std::vector<fxl::RefPtr<Function>> ModuleSymbolsImpl::GetMainFunctions() const {
  std::vector<fxl::RefPtr<Function>> result;
  for (const auto& ref : index_.main_functions()) {
    auto symbol_ref = IndexSymbolRefToSymbol(ref);
    const Function* func = symbol_ref.Get()->As<Function>();
    if (func)
      result.emplace_back(RefPtrTo(func));
  }
  return result;
}

const Index& ModuleSymbolsImpl::GetIndex() const { return index_; }

LazySymbol ModuleSymbolsImpl::IndexSymbolRefToSymbol(const IndexNode::SymbolRef& ref) const {
  // TODO(bug 53091) in the future we may want to add ELF symbol support here.
  switch (ref.kind()) {
    case IndexNode::SymbolRef::kNull:
      break;
    case IndexNode::SymbolRef::kDwarf:
    case IndexNode::SymbolRef::kDwarfDeclaration:
      // Handled by the DWARF symbol factory.
      return symbol_factory_->MakeLazy(ref.offset());
  }
  return LazySymbol();
}

bool ModuleSymbolsImpl::HasBinary() const { return binary_->HasBinary(); }

std::optional<uint64_t> ModuleSymbolsImpl::GetDebugAddrEntry(uint64_t addr_base,
                                                             uint64_t index) const {
  return binary_->GetDebugAddrEntry(addr_base, index);
}

void ModuleSymbolsImpl::AppendLocationForFunction(const SymbolContext& symbol_context,
                                                  const ResolveOptions& options,
                                                  const Function* func,
                                                  std::vector<Location>* result) const {
  if (func->code_ranges().empty())
    return;  // No code associated with this.

  // Compute the full file/line information if requested. This recomputes function DIE which is
  // unnecessary but makes the code structure simpler and ensures the results are always the same
  // with regard to how things like inlined functions are handled (if the location maps to both a
  // function and an inlined function inside of it).
  uint64_t abs_addr = symbol_context.RelativeToAbsolute(func->code_ranges()[0].begin());
  if (options.symbolize)
    result->push_back(LocationForAddress(symbol_context, abs_addr, options, func));
  else
    result->emplace_back(Location::State::kAddress, abs_addr);
}

std::vector<Location> ModuleSymbolsImpl::ResolveLineInputLocation(
    const SymbolContext& symbol_context, const InputLocation& input_location,
    const ResolveOptions& options) const {
  std::vector<Location> result;
  for (const std::string& file : FindFileMatches(input_location.line.file())) {
    ResolveLineInputLocationForFile(symbol_context, file, input_location.line.line(), options,
                                    &result);
  }
  return result;
}

std::vector<Location> ModuleSymbolsImpl::ResolveSymbolInputLocation(
    const SymbolContext& symbol_context, const InputLocation& input_location,
    const ResolveOptions& options) const {
  FX_DCHECK(input_location.type == InputLocation::Type::kName);
  if (!HasOnlySupportedSpecialIdentifierTypes(input_location.name))
    return {};  // Unsupported symbol type.

  // Special-case for ELF/PLT functions.
  //
  // Note that this only checks the ELF index when explicitly requested. This is because for a given
  // function, say "pthread_key_create", it will have a .so with the implementation, and each module
  // that references it will have a PLT thunk (a type of ELF symbol). Matching all the ELF symbols
  // is not what the user wants when they ask for information or a breakpoint on this function (the
  // breakpoint will end up meaning 2 breaks per call).
  //
  // At the same time, it would be nice to use ELF symbols when debugging non-symbolized binaries.
  // We can't just ask if the index is empty to detech this case since "unsymbolized" binaries can
  // sometimes have a few trivial DWARF symbols.
  //
  // Just falling back to ELF symbols here when the main lookup matches nothing doesn't work because
  // the calling modules in the pthread example above will have only the PLT match. To make it work,
  // every caller of this function that combines results from more than one module (including
  // FindName and ProcessSymbols) needs to have some filtering or prioritizing and how this should
  // work is non-obvious.
  if (auto plt_name = GetSpecialInputLocation(input_location, SpecialIdentifier::kPlt))
    return ResolvePltName(symbol_context, *plt_name);
  if (auto elf_name = GetSpecialInputLocation(input_location, SpecialIdentifier::kElf))
    return ResolveElfName(symbol_context, *elf_name);

  std::vector<Location> result;

  auto symbol_to_find = input_location.name;

  // Special-case for main functions.
  if (ReferencesMainFunction(input_location)) {
    auto main_functions = GetMainFunctions();
    if (!main_functions.empty()) {
      for (const auto& func : GetMainFunctions())
        AppendLocationForFunction(symbol_context, options, func.get(), &result);
      return result;
    } else {
      // Nothing explicitly marked as the main function, fall back on anything in the toplevel
      // namespace named "main".
      symbol_to_find = Identifier(IdentifierQualification::kGlobal, IdentifierComponent("main"));

      // Fall through to symbol finding on the new name.
    }
  }

  // TODO(bug 37654) it would be nice if this could be deleted and all code go through
  // expr/find_name.h to query the index. As-is this duplicates some of FindName's logic in a less
  // flexible way.
  for (const auto& ref : index_.FindExact(symbol_to_find)) {
    LazySymbol lazy_symbol = IndexSymbolRefToSymbol(ref);
    const Symbol* symbol = lazy_symbol.Get();

    if (const Function* function = symbol->As<Function>()) {
      // Symbol is a function.
      AppendLocationForFunction(symbol_context, options, function, &result);
    } else if (const Variable* variable = symbol->As<Variable>()) {
      // Symbol is a variable. This will be the case for global variables and file- and class-level
      // statics. This always symbolizes since we already computed the symbol.
      result.push_back(LocationForVariable(symbol_context, RefPtrTo(variable)));
    } else {
      // Unknown type of symbol.
      continue;
    }
  }

  return result;
}

std::vector<Location> ModuleSymbolsImpl::ResolveAddressInputLocation(
    const SymbolContext& symbol_context, const InputLocation& input_location,
    const ResolveOptions& options) const {
  std::vector<Location> result;
  if (options.symbolize) {
    result.push_back(LocationForAddress(symbol_context, input_location.address, options, nullptr));
  } else {
    result.emplace_back(Location::State::kAddress, input_location.address);
  }
  return result;
}

Location ModuleSymbolsImpl::LocationForAddress(const SymbolContext& symbol_context,
                                               uint64_t absolute_address,
                                               const ResolveOptions& options,
                                               const Function* optional_func) const {
  auto dwarf_loc =
      DwarfLocationForAddress(symbol_context, absolute_address, options, optional_func);
  if (dwarf_loc && dwarf_loc->symbol())
    return std::move(*dwarf_loc);

  // Fall back to use ELF symbol but keep the file/line info.
  FileLine file_line;
  int column = 0;
  if (dwarf_loc) {
    file_line = dwarf_loc->file_line();
    column = dwarf_loc->column();
  }
  if (auto elf_locs =
          ElfLocationForAddress(symbol_context, absolute_address, options, file_line, column))
    return std::move(*elf_locs);

  // Not symbolizable, return an "address" with no symbol information. Mark it symbolized to record
  // that we tried and failed.
  return Location(Location::State::kSymbolized, absolute_address);
}

// This function is similar to llvm::DWARFContext::getLineInfoForAddress.
std::optional<Location> ModuleSymbolsImpl::DwarfLocationForAddress(
    const SymbolContext& symbol_context, uint64_t absolute_address, const ResolveOptions& options,
    const Function* optional_func) const {
  // TODO(bug 5544) handle addresses that aren't code like global variables.
  uint64_t relative_address = symbol_context.AbsoluteToRelative(absolute_address);
  fxl::RefPtr<DwarfUnit> unit = binary_->UnitForRelativeAddress(relative_address);
  if (!unit)  // No DWARF symbol.
    return std::nullopt;

  FileLine file_line;
  int column = 0;

  // Get the innermost subroutine or inlined function for the address. This may be empty, but still
  // lookup the line info below in case its present. This computes both a LazySymbol which we
  // pass to the result, and a possibly-null containing Function* (not an inlined subroutine) to do
  // later computations on.
  fxl::RefPtr<Function> function;  // For prologue computations.
  LazySymbol lazy_function;
  if (optional_func) {
    // The function was passed in and we want to return that exact one. This will happen if the
    // caller has asked for the location of a named function.
    function = RefPtrTo(optional_func);
    lazy_function = LazySymbol(optional_func);
  } else {
    // Resolve the function for this address.
    if (uint64_t fn_die_offset = unit->FunctionDieOffsetForRelativeAddress(relative_address)) {
      // FunctionForRelativeAddress() will return the most specific inlined function for the
      // address.
      lazy_function = symbol_factory_->MakeLazy(fn_die_offset);
      function = RefPtrTo(lazy_function.Get()->As<Function>());

      // The is_inline() check is strictly unnecessary since ambiguous inline computations will
      // work either way. This check allows us to skip the ambiguous inline computations in the
      // common case that we're not in an inline.
      if (function && function->is_inline() &&
          options.ambiguous_inline == ResolveOptions::AmbiguousInline::kOuter) {
        // Adjust the function to be the outermost frame (should be the non-inlined function)
        // for ambiguous locations (at the beginning of one or more inlined functions).
        std::vector<fxl::RefPtr<Function>> inline_chain =
            function->GetAmbiguousInlineChain(symbol_context, absolute_address);
        if (inline_chain.size() > 1) {
          lazy_function = inline_chain.back();

          // Since we picked a non-topmost inline subroutine, we know the file/line because
          // it's the call location of the inline subroutine we skipped. DWARF doesn't encode
          // column information for this type of call.
          const auto& calling_func = inline_chain[inline_chain.size() - 2];
          file_line = calling_func->call_line();
        }
      }
    }
  }

  // Get the file/line location (may fail). Don't overwrite one computed above if already set above
  // using the ambigous inline call site.
  if (!file_line.is_valid()) {
    const LineTable& line_table = unit->GetLineTable();

    // Use the line table to move the address to after the function prologue. Assume if the function
    // is inline there's no prologue. Inlines themselves will have no prologues, and we assume
    // inlines won't appear in the prologue of other functions.
    if (function && !function->is_inline() && options.skip_function_prologue) {
      if (size_t prologue_size = GetFunctionPrologueSize(line_table, function.get())) {
        // The function has a prologue. When it does, we know it has code ranges so don't need to
        // validate it's nonempty before using.
        uint64_t function_begin = function->code_ranges().front().begin();
        if (relative_address >= function_begin &&
            relative_address < function_begin + prologue_size) {
          // Adjust address to the first real instruction.
          relative_address = function_begin + prologue_size;
          absolute_address = symbol_context.RelativeToAbsolute(relative_address);
        }
      }
    }

    // Look up the line info for this address.
    //
    // This re-computes some of what GetFunctionPrologueSize() may have done above. This could be
    // enhanced in the future by having LineTable::GetRowForAddress that include the prologue
    // adjustment as part of one computation.
    LineTable::FoundRow found_row = line_table.GetRowForAddress(symbol_context, absolute_address);
    if (!found_row.empty()) {
      // Line info present. Only set the file name if there's a nonzero line number. "Line 0"
      // entries which are compiled-generated code not associated with a line entry. Typically there
      // will be a file if we ask, but that's leftover from the previous row in the table by the
      // state machine and is not relevant.
      const LineTable::Row& row = found_row.get();
      std::optional<std::string> file_name;
      if (row.Line)
        file_name = line_table.GetFileNameForRow(row);  // Could still return nullopt.
      if (file_name) {
        // It's important this only gets called when row.Line > 0. FileLine will assert for line 0
        // if a file name or build directory is given to ensure that all "no code" locations compare
        // identically. This is guaranteed here because file_name will only be set when the line is
        // nonzero.
        if (build_dir_.empty())
          file_line = FileLine(std::move(*file_name), unit->GetCompilationDir(), row.Line);
        else
          file_line = FileLine(std::move(*file_name), build_dir_, row.Line);
      }
      column = row.Column;
    }
  }

  if (!lazy_function && !file_line.is_valid()) {
    return std::nullopt;
  }

  return Location(absolute_address, file_line, column, symbol_context, std::move(lazy_function));
}

std::optional<Location> ModuleSymbolsImpl::ElfLocationForAddress(
    const SymbolContext& symbol_context, uint64_t absolute_address, const ResolveOptions& options,
    FileLine file_line, int column) const {
  if (elf_addresses_.empty())
    return std::nullopt;

  uint64_t relative_addr = symbol_context.AbsoluteToRelative(absolute_address);
  auto found = debug::LargestLessOrEqual(
      elf_addresses_.begin(), elf_addresses_.end(), relative_addr,
      [](const ElfSymbolRecord* r, uint64_t addr) { return r->relative_address < addr; },
      [](const ElfSymbolRecord* r, uint64_t addr) { return r->relative_address == addr; });
  if (found == elf_addresses_.end())
    return std::nullopt;

  // There could theoretically be multiple matches for this address, but we return only the first.
  const ElfSymbolRecord* record = *found;
  if (relative_addr - record->relative_address > kMaxElfOffsetForMatch)
    return std::nullopt;  // Too far away.
  return Location(
      absolute_address, std::move(file_line), column, symbol_context,
      fxl::MakeRefCounted<ElfSymbol>(const_cast<ModuleSymbolsImpl*>(this)->GetWeakPtr(), *record));
}

Location ModuleSymbolsImpl::LocationForVariable(const SymbolContext& symbol_context,
                                                fxl::RefPtr<Variable> variable) const {
  // Evaluate the DWARF expression for the variable. Global and static variables' locations aren't
  // based on CPU state. In some cases like TLS the location may require CPU state or may result in
  // a constant instead of an address. In these cases give up and return an "unlocated variable."
  // These can easily be evaluated by the expression system so we can still print their values.

  // Need one unique location.
  if (variable->location().locations().size() != 1)
    return Location(symbol_context, std::move(variable));

  auto global_data_provider = fxl::MakeRefCounted<GlobalSymbolDataProvider>();
  DwarfExprEval eval(UnitSymbolFactory(variable.get()), global_data_provider, symbol_context);
  eval.Eval(variable->location().locations()[0].expression,
            [](DwarfExprEval* eval, const Err& err) {});

  // Only evaluate synchronous outputs that result in a pointer.
  if (!eval.is_complete() || !eval.is_success() ||
      eval.GetResultType() != DwarfExprEval::ResultType::kPointer)
    return Location(symbol_context, std::move(variable));

  DwarfStackEntry result = eval.GetResult();
  if (!result.TreatAsUnsigned())
    return Location(symbol_context, std::move(variable));

  // TODO(brettw) in all of the return cases we could in the future fill in the file/line of the
  // definition of the variable. Currently Variables don't provide that (even though it's usually in
  // the DWARF symbols).
  return Location(result.unsigned_value(), FileLine(), 0, symbol_context, std::move(variable));
}

std::vector<Location> ModuleSymbolsImpl::ResolvePltName(const SymbolContext& symbol_context,
                                                        const std::string& mangled_name) const {
  // There can theoretically be multiple symbols with the given name, some might be PLT symbols,
  // some might not be. Check all name matches for a PLT one.
  auto cur = mangled_elf_symbols_.lower_bound(mangled_name);
  while (cur != mangled_elf_symbols_.end() && cur->first == mangled_name) {
    if (cur->second.type == ElfSymbolType::kPlt)
      return {MakeElfSymbolLocation(symbol_context, std::nullopt, cur->second)};
    ++cur;
  }

  // No PLT locations found for this name.
  return {};
}

std::vector<Location> ModuleSymbolsImpl::ResolveElfName(const SymbolContext& symbol_context,
                                                        const std::string& mangled_name) const {
  std::vector<Location> result;

  // There can theoretically be multiple symbols with the given name.
  auto cur = mangled_elf_symbols_.lower_bound(mangled_name);
  while (cur != mangled_elf_symbols_.end() && cur->first == mangled_name) {
    result.push_back(MakeElfSymbolLocation(symbol_context, std::nullopt, cur->second));
    ++cur;
  }

  return result;
}

// To a first approximation we just look up the line in the line table for each compilation unit
// that references the file. Complications:
//
// 1. The line might not be an exact match (the user can specify a blank line or something optimized
//    out). In this case, find the next valid line.
//
// 2. Clang doesn't emit line table entries for the source of an inlined function call.
//    See https://bugs.fuchsia.dev/p/fuchsia/issues/detail?id=112203 and
//    https://github.com/llvm/llvm-project/issues/58483.
//
//    To work around this, for every function with a line table match from complication 1, we search
//    for inlined function calls whose call location matches the input location. (Note: neither GDB
//    nor LLDB do this as of this writing, this is more of a workaround for a Clang bug.)
//
// 3. The above step can find many different locations. Maybe some code from the file in question is
//    inlined into the compilation unit, but not the function with the line in it. Or different
//    template instantiations can mean that a line of code is in some instantiations but don't apply
//    to others.
//
//    To solve this duplication problem, get the resolved line of each of the addresses found above
//    and find the best one. Keep only those locations matching the best one (there can still be
//    multiple).
//
// 4. Inlining and templates can mean there can be multiple matches of the exact same line. Only
//    keep the first match per function or inlined function to catch the case where a line is spread
//    across multiple line table entries.
void ModuleSymbolsImpl::ResolveLineInputLocationForFile(const SymbolContext& symbol_context,
                                                        const std::string& canonical_file,
                                                        int line_number,
                                                        const ResolveOptions& options,
                                                        std::vector<Location>* output) const {
  const std::vector<size_t>* units = index_.FindFileUnitIndices(canonical_file);
  if (!units)
    return;

  std::vector<LineMatch> matches;
  for (size_t index : *units) {
    fxl::RefPtr<DwarfUnit> unit = binary_->GetUnitAtIndex(index);
    const LineTable& line_table = unit->GetLineTable();

    // Complication 1 above: find all matches for this line in the unit.
    std::vector<LineMatch> unit_matches =
        GetAllLineTableMatchesInUnit(line_table, canonical_file, line_number);
    matches.insert(matches.end(), unit_matches.begin(), unit_matches.end());
  }

  if (matches.empty())
    return;

  // Complication 2 above: Get all unique functions we found from complication 1, and add in
  // inline call site locations. This can be removed if the above-mentioned Clang bug is fixed.
  //
  // The LineMatch contains the function DIE offset which we use for uniquifying functions, but
  // we currently don't have a convenient way to map this back to a Symbol object. Instead, look up
  // the corresponding function based on address of the previously found match.
  //
  // There are some limitations of this approach. But since neither GDB nor LLDB use inline call
  // site information at all, our implementation is already better than the competition.
  //
  //  - TODO #1: This will work for most cases but isn't quite sufficient. If you have an inlined
  //    function that consists only of a call to another inlined function (maybe in another file),
  //    the calling inlined function will never get identified in the previous loop and we'll never
  //    search its inlined call locations in this loop. Having an inline only call another inline
  //    isn't actually that uncommon in places like the STL.
  //
  //  - TODO #2: For optimized code, sometimes the line you ask for doesn't have any code. To
  //    support this, the line table searching code looks for lines after the one you asked for
  //    also. To prevent matching all later lines in the file, it searches for *transitions* between
  //    code before the line being searched for to code equal to or after the line being searched
  //    for. The inline call site searching code doesn't have enough context to search for these
  //    transitions so only searches for exact matches.
  //
  // The correct approach would be brute-force search all functions in the units identified at the
  // top of this function that reference the file in question. We would check these functions for
  // inlined calls and save their call locations. (As of this writing, our current symbol
  // implementation doesn't make it easy to go through all functions in a unit).
  //
  // Then we would take these inline call locations and merge them into the line table in some way
  // (probably we would make up a completely new line table that's processed in all the ways we need
  // and cache it on the unit). This way, GetAllLineTableMatchesInUnit() can take inline call
  // locations into account when looking for code line transitions. Then if you request a breakpoint
  // for an optimized-out line *before* the inline call, it can "slide" down to the inline call
  // even if there was no line table entry for it.
  std::set<uint64_t> checked_functions;          // LineMatch.function_die_offsets we've checked.
  size_t original_match_count = matches.size();  // Don't check anything the loop appends.
  for (size_t i = 0; i < original_match_count; i++) {
    const LineMatch& match = matches[i];

    if (checked_functions.find(match.function_die_offset) != checked_functions.end())
      continue;  // Already checked this function.
    checked_functions.insert(match.function_die_offset);

    auto unit = binary_->UnitForRelativeAddress(match.address);
    if (!unit)
      continue;  // Some kind of corruption.

    if (auto fn =
            RefPtrTo(symbol_factory_->CreateSymbol(match.function_die_offset)->As<Function>())) {
      // Make sure we have the outermost function and not some random code block inside it.
      //
      // If GetContainingFunction() returns a different function that we put in (i.e. we put in
      // an inline) but we've already checked it, give up. Don't do this if it gives us the
      // same thing back since we've inserted our current function's DIE offset in the map but
      // not checked it yet.
      auto containing_fn = fn->GetContainingFunction(Function::kPhysicalOnly);
      if (fn->GetDieOffset() != containing_fn->GetDieOffset() &&
          checked_functions.find(fn->GetDieOffset()) != checked_functions.end())
        continue;  // Already checked this toplevel function.

      // Append any new matches.
      AppendLineMatchesForInlineCalls(containing_fn.get(), canonical_file, line_number,
                                      containing_fn->GetDieOffset(), &matches);
    }
  }

  // Complications 3 & 4 above: Get all instances of the best match only with a max of one per
  // function. The best match is the one with the lowest line number (found matches should all be
  // bigger than the input line, so this will be the closest).
  for (const LineMatch& match : GetBestLineMatches(matches)) {
    uint64_t abs_addr = symbol_context.RelativeToAbsolute(match.address);
    if (options.symbolize) {
      output->push_back(LocationForAddress(symbol_context, abs_addr, options, nullptr));
    } else {
      output->push_back(Location(Location::State::kAddress, abs_addr));
    }
  }
}

Location ModuleSymbolsImpl::MakeElfSymbolLocation(const SymbolContext& symbol_context,
                                                  std::optional<uint64_t> relative_address,
                                                  const ElfSymbolRecord& record) const {
  uint64_t absolute_address;
  if (relative_address) {
    // Caller specified a more specific address (normally inside the ELF symbol).
    absolute_address = symbol_context.RelativeToAbsolute(*relative_address);
  } else {
    // Take address from the ELF symbol.
    absolute_address = symbol_context.RelativeToAbsolute(record.relative_address);
  }

  return Location(
      absolute_address, FileLine(), 0, symbol_context,
      fxl::MakeRefCounted<ElfSymbol>(const_cast<ModuleSymbolsImpl*>(this)->GetWeakPtr(), record));
}

void ModuleSymbolsImpl::FillElfSymbols() {
  FX_DCHECK(mangled_elf_symbols_.empty());
  FX_DCHECK(elf_addresses_.empty());

  const std::map<std::string, llvm::ELF::Elf64_Sym>& elf_syms = binary_->GetELFSymbols();
  const std::map<std::string, uint64_t>& plt_syms = binary_->GetPLTSymbols();

  // Insert the regular symbols.
  //
  // The |st_value| is the relative virtual address we want to index. Potentially we might want to
  // save more flags and expose them in the ElfSymbol class.
  for (const auto& [name, sym] : elf_syms) {
    // The symbol type is the low 4 bits. The higher bits encode the visibility which we don't
    // care about. We only need to index objects and code, and a couple of special symbols left
    // specifically for Zxdb's usage.
    int symbol_type = sym.st_info & 0xf;
    if (symbol_type != elflib::STT_OBJECT && symbol_type != elflib::STT_FUNC &&
        symbol_type != elflib::STT_TLS && name.substr(0, 5) != "zxdb.")
      continue;

    if (sym.st_value == 0)
      continue;  // No address for this symbol. Probably imported.

    auto inserted = mangled_elf_symbols_.emplace(
        std::piecewise_construct, std::forward_as_tuple(name),
        std::forward_as_tuple(ElfSymbolType::kNormal, sym.st_value, sym.st_size, name));

    // Append all addresses for now, this will be sorted at the bottom.
    elf_addresses_.push_back(&inserted->second);
  }

  // Insert PLT symbols.
  for (const auto& [name, addr] : plt_syms) {
    // TODO(sadmac): Set the symbol size to the size of a PLT entry on this architecture.
    auto inserted =
        mangled_elf_symbols_.emplace(std::piecewise_construct, std::forward_as_tuple(name),
                                     std::forward_as_tuple(ElfSymbolType::kPlt, addr, 0, name));

    // Append all addresses for now, this will be sorted at the bottom.
    elf_addresses_.push_back(&inserted->second);
  }

  std::sort(elf_addresses_.begin(), elf_addresses_.end(),
            [](const ElfSymbolRecord* left, const ElfSymbolRecord* right) {
              return left->relative_address < right->relative_address;
            });
}

}  // namespace zxdb
