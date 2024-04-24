// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/expr/mock_eval_context.h"

#include "src/developer/debug/shared/register_info.h"
#include "src/developer/debug/zxdb/common/err.h"
#include "src/developer/debug/zxdb/expr/abi_null.h"
#include "src/developer/debug/zxdb/expr/builtin_types.h"
#include "src/developer/debug/zxdb/expr/expr_value.h"
#include "src/developer/debug/zxdb/expr/find_name.h"
#include "src/developer/debug/zxdb/expr/found_name.h"
#include "src/developer/debug/zxdb/expr/register_utils.h"
#include "src/developer/debug/zxdb/expr/resolve_type.h"
#include "src/developer/debug/zxdb/symbols/identifier.h"

namespace zxdb {

namespace {

// Returns a copy of the input with the global qualification forced to true.
ParsedIdentifier GloballyQualified(const ParsedIdentifier& input) {
  ParsedIdentifier result = input;
  result.set_qualification(IdentifierQualification::kGlobal);
  return result;
}

}  // namespace

MockEvalContext::MockEvalContext()
    : abi_(std::make_unique<AbiNull>()),
      data_provider_(fxl::MakeRefCounted<MockSymbolDataProvider>()) {}

MockEvalContext::~MockEvalContext() = default;

void MockEvalContext::AddName(const ParsedIdentifier& ident, FoundName found) {
  names_[GloballyQualified(ident)] = std::move(found);
}

void MockEvalContext::AddVariable(const std::string& name, ExprValue v) {
  values_by_name_[name] = v;

  fxl::RefPtr<Variable> var =
      fxl::MakeRefCounted<Variable>(DwarfTag::kVariable, name, v.type_ref(), VariableLocation());
  // The FoundName constructor takes a reference to the Variable object.
  AddName(ParsedIdentifier(ParsedIdentifierComponent(name)), FoundName(var.get()));
}

void MockEvalContext::AddVariable(const Value* key, ExprValue v) {
  values_by_symbol_[key] = v;

  fxl::RefPtr<Variable> var = fxl::MakeRefCounted<Variable>(
      DwarfTag::kVariable, key->GetAssignedName(), v.type_ref(), VariableLocation());
  // The FoundName constructor takes a reference to the Variable object.
  AddName(ToParsedIdentifier(key->GetIdentifier()), FoundName(var.get()));
}

void MockEvalContext::AddLocation(uint64_t address, Location location) {
  locations_[address] = std::move(location);
}

void MockEvalContext::AddBuiltinFunction(const ParsedIdentifier& name, BuiltinFuncCallback impl) {
  builtin_funcs_[name] = std::move(impl);
}

void MockEvalContext::FindName(const FindNameOptions& options, const ParsedIdentifier& looking_for,
                               std::vector<FoundName>* results) const {
  // Check the mocks first.
  auto found = names_.find(GloballyQualified(looking_for));
  if (found != names_.end()) {
    results->push_back(found->second);
    // Assume if a mock was provided, we don't need to do a full search for anything else.
    return;
  }

  // Fall back on normal name lookup.
  ::zxdb::FindName(GetFindNameContext(), options, looking_for, results);
}

FindNameContext MockEvalContext::GetFindNameContext() const { return FindNameContext(language_); }

void MockEvalContext::GetNamedValue(const ParsedIdentifier& ident, EvalCallback cb) const {
  // Can ignore the symbol output for this test, it's not needed by the expression evaluation
  // system.
  auto found = values_by_name_.find(ident.GetFullName());
  if (found != values_by_name_.end()) {
    return cb(found->second);
  }

  // Try to resolve the ident as a register.
  auto reg = GetRegisterID(data_provider_->GetArch(), ident);
  if (reg == debug::RegisterID::kUnknown)
    return cb(Err("MockEvalContext::GetNamedValue '%s' not found.", ident.GetFullName().c_str()));

  // Do not perform any implicit asynchronous memory fetches in the mock.
  if (std::optional<cpp20::span<const uint8_t>> opt_reg_data = data_provider_->GetRegister(reg)) {
    if (opt_reg_data->empty())
      return cb(GetUnavailableRegisterErr(reg));
    else
      return cb(RegisterDataToValue(language_, reg, GetVectorRegisterFormat(), *opt_reg_data));
  }

  return cb(Err("MockEvalContext::GetNamedValue '%s' not found.", ident.GetFullName().c_str()));
}

void MockEvalContext::GetVariableValue(fxl::RefPtr<Value> variable, EvalCallback cb) const {
  auto found = values_by_symbol_.find(variable.get());
  if (found == values_by_symbol_.end())
    cb(Err("MockEvalContext::GetVariableValue '%s' not found.", variable->GetFullName().c_str()));
  else
    cb(found->second);
}

const EvalContext::BuiltinFuncCallback* MockEvalContext::GetBuiltinFunction(
    const ParsedIdentifier& name) const {
  auto found = builtin_funcs_.find(name);
  if (found != builtin_funcs_.end())
    return &found->second;
  return nullptr;
}

const ProcessSymbols* MockEvalContext::GetProcessSymbols() const { return nullptr; }

fxl::RefPtr<SymbolDataProvider> MockEvalContext::GetDataProvider() { return data_provider_; }

Location MockEvalContext::GetLocationForAddress(uint64_t address) const {
  auto found = locations_.find(address);
  if (found == locations_.end())
    return Location(Location::State::kAddress, address);
  return found->second;
}

}  // namespace zxdb
