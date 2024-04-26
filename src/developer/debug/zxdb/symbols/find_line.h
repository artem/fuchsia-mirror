// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_FIND_LINE_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_FIND_LINE_H_

#include <stdint.h>

#include <string>
#include <tuple>
#include <vector>

#include "src/developer/debug/zxdb/symbols/lazy_symbol.h"

namespace zxdb {

class CodeBlock;
class Function;
class LineTable;
class Location;
struct ResolveOptions;
class SymbolContext;

struct LineMatch {
  LineMatch() = default;
  LineMatch(uint64_t addr, int ln, LazySymbol func)
      : address(addr), line(ln), function(std::move(func)) {}

  bool operator==(const LineMatch& other) const {
    return std::tie(address, line, function) == std::tie(other.address, other.line, other.function);
  }

  uint64_t address = 0;
  int line = 0;

  // Symbol of the most specific inlined for this address or an !is_valid() symbol if there is no
  // function for it. This is used so we don't accidentally treat duplicate line entries in
  // different functions as the same.
  LazySymbol function;
};

// Searches the given line table for the given file/line. Finds the smallest line greater than or
// equal to the input line and returns all instances of that line.
std::vector<LineMatch> GetAllLineTableMatchesInUnit(const LineTable& line_table,
                                                    const std::string& full_path, int line);

// Recursively searches the given code block (normally a function for the first call) for inlined
// function calls whose call location could match the given file/line. The results will be appended
// to the given accumulator.
//
// This is used to workaround the Clang bug
// https://bugs.fuchsia.dev/p/fuchsia/issues/detail?id=112203
// where it does not emit line table entries for the call location of an inline call. See where this
// function is called from in ModuleSymbolsImpl for more.
//
// The function_die_offset is the function corresponding to the input block. When the input block is
// a function (as the initial call will be), this should be set to code_block->GetDieOffset(). But
// for recursive calls, some blocks will be lexical scopes and we want to maintain the DIE offset
// of the parent *function* containing it.
void AppendLineMatchesForInlineCalls(const CodeBlock* block, const std::string& full_path, int line,
                                     const Function* block_fn, std::vector<LineMatch>* accumulator);

// Filters the set of matches to get all instances of the closest match for the line. Typically this
// will result in a maximum of one match per function. It's assumed that the LineMatches are all for
// the same file.
//
// LineMatches are only generated for lines that cross the line in question, so the closest
// LineMatch for this function is the one with the smallest line number.
//
// The "one per function" rule is because a line can often get broken into multiple line table
// entries (sometimes disjoint, sometimes not), and when asking for a line we want the one with the
// lowest address. This rule is occasionally violated for cases like lines that contain code
// generated around rust await points, and generate line tables that would put a breakpoint on an
// invalid instruction (https://github.com/rust-lang/rust/issues/123341). See the comment above
// |ResolveInputLineForFile| for details.
std::vector<LineMatch> GetBestLineMatches(const std::vector<LineMatch>& matches);

// Computes the size in bytes of the given function's prologue. The line table corresponding to
// that address should be passed.
//
// A function prologue is the boilerplate at the beginning that sets up the stack frame. Generally
// one will want to skip over this automatically because the local variables and function parameters
// won't be readable from inside the prologue. On ARM since a call sets the link register rather
// than modifying the stack, the stack pointer won't always be consistent either.
//
// The size is measured from the function's code_ranges().begin(). If a prologue is not found, this
// returns 0.
size_t GetFunctionPrologueSize(const LineTable& line_table, const Function* function);

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_FIND_LINE_H_
