// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_DWARF_DIE_DECODER_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_DWARF_DIE_DECODER_H_

#include <optional>
#include <string>
#include <utility>

#include "lib/fit/function.h"
#include "llvm/BinaryFormat/Dwarf.h"
#include "llvm/DebugInfo/DWARF/DWARFDataExtractor.h"
#include "src/lib/fxl/macros.h"

namespace llvm {
class DWARFUnit;
class DWARFContext;
class DWARFDataExtractor;
class DWARFDebugInfoEntry;
class DWARFDie;
class DWARFFormValue;
}  // namespace llvm

namespace zxdb {

class ConstValue;

// Decodes the desired attributes of a given DWARF Debug Info Entry ("DIE").
//
// This transparently follows DW_AT_abstract_origin attributes. This is used to implement
// "inheritance" of DIEs.
//
// To use, create once for the unit and register the output variables with the Add* functions. Then
// loop through the relevant entries. In the loop first reset() the output variables (so you can
// tell which were set), then call Decode().
class DwarfDieDecoder {
 public:
  // Callback for handling form values. This takes the unit in addition to the form value.
  // llvm::DWARFFormValue::getUnit() exists but this won't always be set (depending on the decode
  // path, currently the implicit constant path ignores it) and there's currently no API to set it
  // explicitly. As a result, if the unit is needed, always use the passed-in parameter rather than
  // getUnit().
  using AttributeHandler = fit::function<void(llvm::DWARFUnit*, const llvm::DWARFFormValue&)>;

  // DW_AT_high_pc is special: If it is of class "address", it's an address, and if it's of class
  // "constant" it's an unsigned integer offset from the low PC. This struct encodes whether it was
  // a constant or not in the output. Use with AddHighPC().
  struct HighPC {
    HighPC() = default;
    HighPC(bool c, uint64_t v) : is_constant(c), value(v) {}

    bool is_constant = false;
    uint64_t value = 0;
  };

  // The context and unit must outlive this class.
  explicit DwarfDieDecoder(llvm::DWARFContext* context);
  ~DwarfDieDecoder();

  // Adds a check for the given attribute. If the attribute is encountered, the given boolean will
  // be set to true. You can share a bool pointer between different calls to AddPresenceCheck() to
  // check if any of a set of attributes is available. It does not check the type of validity of the
  // attribute.
  //
  // The output pointer must remain valid until the last call to Decode() has returned.
  void AddPresenceCheck(llvm::dwarf::Attribute attribute, bool* present);

  // These register for a given attribute, and call the similarly-named function in
  // llvm::DWARFFormValue to extract the attribute and place it into the given output variable.
  //
  // The output pointers must remain valid until the last call to Decode() has returned.
  void AddBool(llvm::dwarf::Attribute attribute, std::optional<bool>* output);
  void AddUnsignedConstant(llvm::dwarf::Attribute attribute, std::optional<uint64_t>* output);
  void AddSignedConstant(llvm::dwarf::Attribute attribute, std::optional<int64_t>* output);
  void AddAddress(llvm::dwarf::Attribute attribute, std::optional<uint64_t>* output);
  void AddHighPC(std::optional<HighPC>* output);
  void AddCString(llvm::dwarf::Attribute attribute, std::optional<const char*>* output);
  void AddLineTableFile(llvm::dwarf::Attribute attribute, std::optional<std::string>* output);
  void AddConstValue(llvm::dwarf::Attribute attribute, ConstValue* const_value);
  void AddSectionOffset(llvm::dwarf::Attribute attribute, std::optional<uint64_t>* offset);
  void AddBlock(llvm::dwarf::Attribute attribute, std::optional<std::vector<uint8_t>>* block);

  // For cross-DIE references. Note that the resulting DIE may not be in the same unit. If the
  // attribute doesn't exist or is invalid, this DIE will be !isValid().
  void AddReference(llvm::dwarf::Attribute attribute, llvm::DWARFDie* output);

  // Extract a file name. File names (e.g. for DW_AT_decl_file) are not strings but rather indices
  // into the file name table for the corresponding unit. This accessor resolves the string
  // automatically.
  void AddFile(llvm::dwarf::Attribute attribute, std::optional<std::string>* output);

  // A special handler to get the parent of the most deep abstract origin.
  //
  // Most DIEs can have an "abstract origin" which is another DIE that underlays values.
  // Theoretically abstract origins can be linked into arbitrarily long chains. In the current Clang
  // this mostly happens for inlined functions, where the inlined instance references the actual
  // function definition as its abstract origin. But abstract origins can theoretically appear
  // almost anywhere.
  //
  // Normally this class handles abstract origins transparently when querying attributes. But the
  // parent DIE is not an attribute so needs to be handled explicitly. In the example of inlined
  // functions, the parent of the inlined subroutine DIE will be the block it's inlined into, but
  // the parent of the abstract origin will be the namespace or class that lexically encloses that
  // function.
  //
  // This function will cause the parent of the deepest abstract origin to be placed into the given
  // output when the DIE is decoded.
  //
  // If there is no abstract origin, this will be filled in with the regular parent of the DIE. The
  // only case the output should be !isValid() is when decoding a toplevel DIE with no parent.
  void AddAbstractParent(llvm::DWARFDie* output);

  // Extracts data with a custom callback. When the attribute is encountered, the callback is
  // executed with the associated form value. This can be used to cover attributes that could be
  // encoded using multiple different encodings.
  void AddCustom(llvm::dwarf::Attribute attribute, AttributeHandler callback);

  // Decode one info entry. Returns true on success, false means the DIE was corrupt. The outputs
  // for each encountered attribute will be set.
  bool Decode(const llvm::DWARFDie& die);

 public:
  using Dispatch = std::pair<llvm::dwarf::Attribute, AttributeHandler>;

  // Backend for Decode() above.
  //
  // Following abstract origins generates a recursive call. To prevent infinite recursion for
  // corrupt symbols, this function takes a maximum number of abstract origin references to follow
  // which is decremented each time a recursive call is made. When this gets to 0, no more abstract
  // origin references will be followed.
  bool DecodeInternal(const llvm::DWARFDie& die, int abstract_origin_refs_to_follow);

  // Decodes a cross-DIE reference. Return value will be !isValid() on failure.
  llvm::DWARFDie DecodeReference(llvm::DWARFUnit* unit, const llvm::DWARFFormValue& form);

  llvm::DWARFContext* context_;

  // Normally there will be few attributes and a brute-force search through a contiguous array will
  // be faster than a map lookup.
  std::vector<Dispatch> attrs_;

  // Non-null indicates that the caller has requested the abstract parent (see AddAbstractParent()
  // above) be computed. This variable will hold the desired output location for the parent of the
  // decoded DIE.
  llvm::DWARFDie* abstract_parent_ = nullptr;

  // Used during decode. This should be cleared each time a new DIE is decoded and tracks which
  // attributes have been seen across recursive calls of DecodeInternal (following abstract
  // references). This prevents us from decoding the same attribute more than once across abstract
  // origins (we always want the first one).
  std::vector<llvm::dwarf::Attribute> seen_attrs_;

  FXL_DISALLOW_COPY_AND_ASSIGN(DwarfDieDecoder);
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_SYMBOLS_DWARF_DIE_DECODER_H_
