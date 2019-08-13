// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/expr/cast.h"

#include <limits>

#include "gtest/gtest.h"
#include "src/developer/debug/zxdb/common/err.h"
#include "src/developer/debug/zxdb/expr/eval_test_support.h"
#include "src/developer/debug/zxdb/expr/expr_value.h"
#include "src/developer/debug/zxdb/expr/mock_eval_context.h"
#include "src/developer/debug/zxdb/symbols/base_type.h"
#include "src/developer/debug/zxdb/symbols/collection.h"
#include "src/developer/debug/zxdb/symbols/enumeration.h"
#include "src/developer/debug/zxdb/symbols/inherited_from.h"
#include "src/developer/debug/zxdb/symbols/modified_type.h"
#include "src/developer/debug/zxdb/symbols/type_test_support.h"

namespace zxdb {

TEST(Cast, Implicit) {
  auto eval_context = fxl::MakeRefCounted<MockEvalContext>();

  auto int32_type = MakeInt32Type();
  auto uint32_type = MakeInt32Type();
  auto int64_type = MakeInt64Type();
  auto uint64_type = MakeInt64Type();
  auto char_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeSignedChar, 1, "char");
  auto bool_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeBoolean, 1, "bool");
  auto float_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeFloat, 4, "float");
  auto double_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeFloat, 8, "double");

  ExprValue int32_one(int32_type, {1, 0, 0, 0});
  ExprValue int32_minus_one(int32_type, {0xff, 0xff, 0xff, 0xff});

  // Simple identity conversion.
  ErrOrValue out = CastExprValue(eval_context.get(), CastType::kImplicit, int32_one, int32_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(int32_one, out.value());

  // Signed/unsigned conversion, should just copy the bits (end up with 0xffffffff),
  out = CastExprValue(eval_context.get(), CastType::kImplicit, int32_minus_one, uint32_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(uint32_type.get(), out.value().type());
  EXPECT_EQ(int32_minus_one.data(), out.value().data());

  // Signed integer promotion.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, int32_minus_one, uint64_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(uint64_type.get(), out.value().type());
  EXPECT_EQ(std::numeric_limits<uint64_t>::max(), out.value().GetAs<uint64_t>());

  // Integer truncation
  out = CastExprValue(eval_context.get(), CastType::kImplicit, int32_minus_one, char_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(char_type.get(), out.value().type());
  EXPECT_EQ(-1, out.value().GetAs<int8_t>());

  // Zero integer to boolean.
  ExprValue int32_zero(int32_type, {0, 0, 0, 0});
  out = CastExprValue(eval_context.get(), CastType::kImplicit, int32_zero, bool_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(bool_type.get(), out.value().type());
  EXPECT_EQ(0, out.value().GetAs<int8_t>());

  // Nonzero integer to boolean.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, int32_minus_one, bool_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(bool_type.get(), out.value().type());
  EXPECT_EQ(1, out.value().GetAs<int8_t>());

  // Zero floating point to boolean.
  ExprValue float_minus_zero(float_type, {0, 0, 0, 0x80});
  out = CastExprValue(eval_context.get(), CastType::kImplicit, float_minus_zero, bool_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(0, out.value().GetAs<int8_t>());

  // Nonzero floating point to boolean.
  ExprValue float_one_third(float_type, {0xab, 0xaa, 0xaa, 0x3e});
  out = CastExprValue(eval_context.get(), CastType::kImplicit, float_one_third, bool_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(1, out.value().GetAs<int8_t>());

  // Float to signed.
  ExprValue float_minus_two(float_type, {0x00, 0x00, 0x00, 0xc0});
  out = CastExprValue(eval_context.get(), CastType::kImplicit, float_minus_two, int32_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(-2, out.value().GetAs<int32_t>());

  // Float to unsigned.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, float_minus_two, uint64_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(static_cast<uint64_t>(-2), out.value().GetAs<uint64_t>());

  // Floating point promotion.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, float_minus_two, double_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(-2.0, out.value().GetAs<double>());
  ExprValue double_minus_two = out.value();

  // Floating point truncation.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, double_minus_two, float_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(-2.0f, out.value().GetAs<float>());

  // Pointer to integer with truncation.
  auto ptr_to_double_type = fxl::MakeRefCounted<ModifiedType>(DwarfTag::kPointerType, double_type);
  ExprValue ptr_value(ptr_to_double_type, {0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12});
  out = CastExprValue(eval_context.get(), CastType::kImplicit, ptr_value, uint32_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(0x9abcdef0, out.value().GetAs<uint32_t>());
  ExprValue big_int_value = out.value();

  // Integer to pointer with expansion.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, int32_minus_one, ptr_to_double_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(-1, out.value().GetAs<int64_t>());
}

// Enums can be casted to and fro.
TEST(Cast, Enum) {
  auto eval_context = fxl::MakeRefCounted<MockEvalContext>();

  // Enumeration values used in both enums below.
  Enumeration::Map values;
  values[0] = "kZero";
  values[1] = "kOne";
  values[static_cast<uint64_t>(-1)] = "kMinus";

  // An untyped enum (old-style C with no qualification). This one is unsigned.
  auto untyped = fxl::MakeRefCounted<Enumeration>("Untyped", LazySymbol(), 4, false, values);

  // An explicitly-typed 8-bit signed enum
  auto char_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeSignedChar, 1, "char");
  auto typed = fxl::MakeRefCounted<Enumeration>("Untyped", char_type, 1, true, values);

  ExprValue untyped_value(untyped, {1, 0, 0, 0});
  ExprValue typed_value(typed, {1});

  // Untyped to int32.
  auto int32_type = MakeInt32Type();
  ErrOrValue out =
      CastExprValue(eval_context.get(), CastType::kImplicit, untyped_value, int32_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(1, out.value().GetAs<int32_t>());

  // Typed to int32.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, typed_value, int32_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(1, out.value().GetAs<int32_t>());

  // Untyped to char.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, untyped_value, char_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(1, out.value().GetAs<int8_t>());

  // Typed to char.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, typed_value, char_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(1, out.value().GetAs<int8_t>());

  // Signed char to untyped (should be sign-extended).
  ExprValue char_minus_one(char_type, {0xff});
  out = CastExprValue(eval_context.get(), CastType::kImplicit, char_minus_one, untyped);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(-1, out.value().GetAs<int32_t>());

  // Signed char to typed.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, char_minus_one, typed);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(-1, out.value().GetAs<int8_t>());
}

// Tests implicit casting when there are derived classes.
TEST(Cast, ImplicitDerived) {
  auto eval_context = fxl::MakeRefCounted<MockEvalContext>();

  DerivedClassTestSetup d;

  // Should be able to implicit cast Derived to Base1 object.
  ErrOrValue out =
      CastExprValue(eval_context.get(), CastType::kImplicit, d.derived_value, d.base1_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(d.base1_type.get(), out.value().type());
  EXPECT_EQ(d.base1_value, out.value());
  // Check the source explicitly since == doesn't check sources.
  EXPECT_EQ(d.base1_value.source(), out.value().source());

  // Same for base 2.
  out = CastExprValue(eval_context.get(), CastType::kImplicit, d.derived_value, d.base2_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(d.base2_type.get(), out.value().type());
  EXPECT_EQ(d.base2_value, out.value());
  // Check the source explicitly since == doesn't check sources.
  EXPECT_EQ(d.base2_value.source(), out.value().source());

  // Should not be able to implicit cast from Base2 to Derived.
  out.value() = ExprValue();
  out = CastExprValue(eval_context.get(), CastType::kImplicit, d.base2_value, d.derived_type);
  EXPECT_TRUE(out.has_error());

  // Pointer casting: should be able to implicit cast derived ptr to base ptr type. This data
  // matches kDerivedAddr in 64-bit little endian.
  out =
      CastExprValue(eval_context.get(), CastType::kImplicit, d.derived_ptr_value, d.base2_ptr_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();

  // That should have adjusted the pointer value appropriately.
  EXPECT_EQ(d.base2_ptr_value, out.value());

  // Should not allow implicit casts from Base to Derived.
  out =
      CastExprValue(eval_context.get(), CastType::kImplicit, d.base2_ptr_value, d.derived_ptr_type);
  EXPECT_TRUE(out.has_error());
  EXPECT_EQ("Can't convert 'Base2*' to unrelated type 'Derived*'.", out.err().msg());

  // Cast a derived to void* is allowed, just a numeric copy.
  auto void_ptr_type = fxl::MakeRefCounted<ModifiedType>(DwarfTag::kPointerType, LazySymbol());
  out = CastExprValue(eval_context.get(), CastType::kImplicit, d.derived_ptr_value, void_ptr_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  ExprValue void_ptr_value(void_ptr_type, d.derived_ptr_value.data());
  EXPECT_EQ(void_ptr_value, out.value());

  // Cast void* to any pointer type (in C this would require an explicit cast).
  out = CastExprValue(eval_context.get(), CastType::kImplicit, void_ptr_value, d.derived_ptr_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(d.derived_ptr_value, out.value());
}

TEST(Cast, Reinterpret) {
  auto eval_context = fxl::MakeRefCounted<MockEvalContext>();

  auto int32_type = MakeInt32Type();
  auto uint32_type = MakeInt32Type();
  auto int64_type = MakeInt64Type();
  auto uint64_type = MakeInt64Type();
  auto char_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeSignedChar, 1, "char");
  auto bool_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeBoolean, 1, "bool");
  auto double_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeFloat, 8, "double");

  auto ptr_to_int32_type = fxl::MakeRefCounted<ModifiedType>(DwarfTag::kPointerType, int32_type);
  auto ptr_to_void_type = fxl::MakeRefCounted<ModifiedType>(DwarfTag::kPointerType, LazySymbol());

  ExprValue int32_minus_one(int32_type, {0xff, 0xff, 0xff, 0xff});
  ExprValue int64_big_num(int32_type, {8, 7, 6, 5, 4, 3, 2, 1});
  ExprValue ptr_to_void(ptr_to_void_type, {8, 7, 6, 5, 4, 3, 2, 1});
  ExprValue double_pi(double_type, {0x18, 0x2d, 0x44, 0x54, 0xfb, 0x21, 0x09, 0x40});

  // Two pointer types: reinterpret_cast<int32_t*>(ptr_to_void);
  ErrOrValue out =
      CastExprValue(eval_context.get(), CastType::kReinterpret, ptr_to_void, ptr_to_int32_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(0x0102030405060708u, out.value().GetAs<uint64_t>());
  EXPECT_EQ(ptr_to_int32_type.get(), out.value().type());

  // Conversion from int to void*. C++ would prohibit this case because the integer is 32 bits and
  // the pointer is 64, but the debugger allows it.  This should not be sign extended.
  out =
      CastExprValue(eval_context.get(), CastType::kReinterpret, int32_minus_one, ptr_to_void_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(0xffffffffu, out.value().GetAs<uint64_t>());

  // Truncation of a number. This is also disallowed in C++.
  out = CastExprValue(eval_context.get(), CastType::kReinterpret, int64_big_num, int32_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(4u, out.value().data().size());
  EXPECT_EQ(0x05060708u, out.value().GetAs<uint32_t>());

  // Prohibit conversions between a double and a pointer: reinterpret_cast<void*>(3.14159265258979);
  out = CastExprValue(eval_context.get(), CastType::kReinterpret, double_pi, ptr_to_void_type);
  EXPECT_TRUE(out.has_error());
  EXPECT_EQ("Can't cast from a 'double'.", out.err().msg());
}

// Static cast is mostly implement as implicit cast. This only tests the additional behavior.
TEST(Cast, Static) {
  auto eval_context = fxl::MakeRefCounted<MockEvalContext>();

  DerivedClassTestSetup d;

  // Should NOT be able to static cast from Base2 to Derived.  This is "static_cast<Derived>(base);"
  ErrOrValue out =
      CastExprValue(eval_context.get(), CastType::kStatic, d.base2_value, d.derived_type);
  EXPECT_TRUE(out.has_error());

  // Cast a derived class reference to a base class reference.  This is
  // "static_cast<Base&>(derived_reference);
  out = CastExprValue(eval_context.get(), CastType::kStatic, d.derived_ref_value, d.base2_ref_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(d.base2_ref_value, out.value());

  // Should be able to go from base->derived with pointers.  This is "static_cast<Derived*>(&base);"
  out = CastExprValue(eval_context.get(), CastType::kStatic, d.base2_ptr_value, d.derived_ptr_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(d.derived_ptr_value, out.value());

  // Base->derived conversion for references.
  out = CastExprValue(eval_context.get(), CastType::kStatic, d.base2_ref_value, d.derived_ref_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(d.derived_ref_value, out.value());

  // Allow conversion of rvalue references and regular references.
  auto derived_rvalue_type =
      fxl::MakeRefCounted<ModifiedType>(DwarfTag::kRvalueReferenceType, d.derived_type);
  ExprValue derived_rvalue_value(derived_rvalue_type, d.derived_ref_value.data());
  out =
      CastExprValue(eval_context.get(), CastType::kStatic, derived_rvalue_value, d.base2_ref_type);
  EXPECT_FALSE(out.has_error());
  EXPECT_EQ(d.base2_ref_value, out.value());

  // Don't allow reference->pointer or pointer->reference casts.
  out =
      CastExprValue(eval_context.get(), CastType::kStatic, d.derived_ref_value, d.derived_ptr_type);
  EXPECT_TRUE(out.has_error());
  out =
      CastExprValue(eval_context.get(), CastType::kStatic, d.derived_ptr_value, d.derived_ref_type);
  EXPECT_TRUE(out.has_error());
}

TEST(Cast, C) {
  auto eval_context = fxl::MakeRefCounted<MockEvalContext>();

  DerivedClassTestSetup d;

  // A C-style cast should be like a static cast when casting between related types.
  ErrOrValue out =
      CastExprValue(eval_context.get(), CastType::kC, d.derived_ref_value, d.base2_ref_type);
  EXPECT_FALSE(out.has_error()) << out.err().msg();
  EXPECT_EQ(d.base2_ref_value, out.value());

  // When there are unrelated pointers, it should fall back to reinterpret.
  auto double_type = fxl::MakeRefCounted<BaseType>(BaseType::kBaseTypeFloat, 8, "double");
  auto ptr_to_double_type = fxl::MakeRefCounted<ModifiedType>(DwarfTag::kPointerType, double_type);
  ExprValue ptr_value(ptr_to_double_type, {0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12});
  out = CastExprValue(eval_context.get(), CastType::kC, ptr_value, d.base2_ptr_type);
  EXPECT_FALSE(out.has_error());

  // For the reinterpret cast, the type should be the new one, with the data being the original.
  EXPECT_EQ(d.base2_ptr_type.get(), out.value().type());
  EXPECT_EQ(ptr_value.data(), out.value().data());

  // Can't cast from a ptr to a ref.
  out = CastExprValue(eval_context.get(), CastType::kC, ptr_value, d.base2_ref_type);
  EXPECT_TRUE(out.has_error());
}

}  // namespace zxdb
