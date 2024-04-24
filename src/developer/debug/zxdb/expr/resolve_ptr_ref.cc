// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/expr/resolve_ptr_ref.h"

#include "src/developer/debug/zxdb/common/err.h"
#include "src/developer/debug/zxdb/expr/eval_context.h"
#include "src/developer/debug/zxdb/expr/expr_value.h"
#include "src/developer/debug/zxdb/expr/resolve_base.h"
#include "src/developer/debug/zxdb/symbols/arch.h"
#include "src/developer/debug/zxdb/symbols/modified_type.h"
#include "src/developer/debug/zxdb/symbols/symbol_data_provider.h"
#include "src/developer/debug/zxdb/symbols/type.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace zxdb {

namespace {

// Backend for the higher-level variant of ResolvePointer that does not handle upcasting to derived
// types.
void DoResolvePointer(const fxl::RefPtr<EvalContext>& eval_context, const ExprValue& pointer,
                      EvalCallback cb) {
  fxl::RefPtr<Type> pointed_to;
  if (Err err = GetPointedToType(eval_context, pointer.type(), &pointed_to); err.has_error())
    return cb(err);

  auto pointer_value_or = ExtractPointerValue(pointer);
  if (pointer_value_or.has_error())
    return cb(pointer_value_or.err());

  // Forward to low-level pointer resolution.
  ResolvePointer(std::move(eval_context), pointer_value_or.value(), std::move(pointed_to),
                 std::move(cb));
}

// Backend for EnsureResolveReference that does not handle upcasting to derived types.
void DoEnsureResolveReference(const fxl::RefPtr<EvalContext>& eval_context, ExprValue value,
                              EvalCallback cb) {
  Type* type = value.type();
  if (!type) {
    // Untyped input, pass the value forward and let the callback handle the problem.
    return cb(std::move(value));
  }

  // The computed type will have the same const, etc. on it as the original, so we need to
  // make it concrete.
  fxl::RefPtr<Type> concrete = eval_context->GetConcreteType(type);
  if (concrete->tag() != DwarfTag::kReferenceType &&
      concrete->tag() != DwarfTag::kRvalueReferenceType) {
    // Not a reference, nothing to do.
    return cb(std::move(value));
  }

  // The symbol provider should have created the right object type.
  const ModifiedType* reference = concrete->As<ModifiedType>();
  FX_DCHECK(reference);
  const Type* underlying_type = reference->modified().Get()->As<Type>();

  auto pointer_value_or = ExtractPointerValue(value);
  if (pointer_value_or.has_error()) {
    cb(pointer_value_or.err());
  } else {
    ResolvePointer(eval_context, pointer_value_or.value(), RefPtrTo(underlying_type),
                   std::move(cb));
  }
}

}  // namespace

// Extracts the pointer value from the ExprValue, assuming it's a pointer. If not, return the error,
// otherwise fill in *pointer_value.
ErrOr<TargetPointer> ExtractPointerValue(const ExprValue& value) {
  Err err = value.EnsureSizeIs(kTargetPointerSize);
  if (err.has_error())
    return err;
  return value.GetAs<TargetPointer>();
}

void ResolvePointer(const fxl::RefPtr<EvalContext>& eval_context, uint64_t address,
                    fxl::RefPtr<Type> type, EvalCallback cb) {
  // We need to be careful to construct the return type with the original type given since it may
  // have const qualifiers, etc., but to use the concrete one (no const, with forward-definitions
  // resolved) for size computation.
  fxl::RefPtr<Type> concrete = eval_context->GetConcreteType(type.get());
  if (!concrete)
    return cb(Err("Missing pointer type."));

  uint32_t type_size = concrete->byte_size();
  eval_context->GetDataProvider()->GetMemoryAsync(
      address, type_size,
      [type = std::move(type), address, type_size, cb = std::move(cb)](
          const Err& err, std::vector<uint8_t> data) mutable {
        // Watch out, "type" may be non-concrete (we need to preserve "const", etc.). Use
        // "type_size" for the concrete size.
        if (err.has_error()) {
          cb(err);
        } else if (data.size() != type_size) {
          // Short read, memory is invalid.
          cb(Err(fxl::StringPrintf("Invalid pointer 0x%" PRIx64, address)));
        } else {
          cb(ExprValue(std::move(type), std::move(data), ExprValueSource(address)));
        }
      });
}

void ResolvePointer(const fxl::RefPtr<EvalContext>& eval_context, const ExprValue& pointer,
                    EvalCallback cb) {
  if (eval_context->ShouldPromoteToDerived()) {
    // Check to see if this is a pointer to a base class that we can convert to a derived class.
    PromotePtrRefToDerived(eval_context, PromoteToDerived::kPtrOnly, pointer,
                           [eval_context, cb = std::move(cb)](ErrOrValue result) mutable {
                             if (result.has_error())
                               cb(result);
                             else
                               DoResolvePointer(eval_context, result.take_value(), std::move(cb));
                           });
  } else {
    // No magic base-class resolution is required, just check the reference.
    DoResolvePointer(eval_context, pointer, std::move(cb));
  }
}

void EnsureResolveReference(const fxl::RefPtr<EvalContext>& eval_context, ExprValue value,
                            EvalCallback cb) {
  if (eval_context->ShouldPromoteToDerived()) {
    // Check to see if this is a reference to a base class that we can convert to a derived class.
    PromotePtrRefToDerived(eval_context, PromoteToDerived::kRefOnly, std::move(value),
                           [eval_context, cb = std::move(cb)](ErrOrValue result) mutable {
                             if (result.has_error()) {
                               cb(result);
                             } else {
                               DoEnsureResolveReference(eval_context, result.take_value(),
                                                        std::move(cb));
                             }
                           });
  } else {
    // No magic base-class resolution is required, just check the reference.
    DoEnsureResolveReference(eval_context, std::move(value), std::move(cb));
  }
}

Err GetPointedToType(const fxl::RefPtr<EvalContext>& eval_context, const Type* input,
                     fxl::RefPtr<Type>* pointed_to, PointedToOptions option) {
  if (!input)
    return Err("No type information.");

  // Convert to a pointer. GetConcreteType() here is more theoretical since current C compilers
  // won't forward-declare pointer types. But it's nice to be sure and this will also strip
  // CV-qualifiers which we do need.
  auto mod_type = eval_context->GetConcreteTypeAs<ModifiedType>(input);
  if (!mod_type || mod_type->tag() != DwarfTag::kPointerType) {
    return Err(fxl::StringPrintf("Attempting to dereference '%s' which is not a pointer.",
                                 input->GetFullName().c_str()));
  }

  if (mod_type->ModifiesVoid() && option == PointedToOptions::kNoVoid) {
    return Err("Can not dereference a pointer to 'void'.");
  }

  // If the caller requested kIncludeVoid, then this may be null. It is the caller's responsibility
  // to check if this is the case.
  *pointed_to = RefPtrTo(mod_type->modified().Get()->As<Type>());

  return Err();
}

}  // namespace zxdb
