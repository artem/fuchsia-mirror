// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/names.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(AliasTests, BadDuplicateAlias) {
  TestLibrary library(R"FIDL(
library example;

type Message = struct {
    f alias_of_int16;
};

alias alias_of_int16 = int16;
alias alias_of_int16 = int16;
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, "alias_of_int16", "example.fidl:8:7");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, GoodAliasOfStruct) {
  TestLibrary library(R"FIDL(
library example;
type TypeDecl = struct {
    field1 uint16;
    field2 uint16;
};
alias AliasOfDecl = TypeDecl;
)FIDL");
  ASSERT_COMPILED(library);
  auto type_decl = library.LookupStruct("TypeDecl");
  ASSERT_NOT_NULL(type_decl);
  EXPECT_EQ(type_decl->members.size(), 2);
  ASSERT_NOT_NULL(library.LookupAlias("AliasOfDecl"));
}

TEST(AliasTests, GoodPrimitive) {
  TestLibrary library(R"FIDL(library example;

type Message = struct {
    f alias_of_int16;
};

alias alias_of_int16 = int16;
)FIDL");
  ASSERT_COMPILED(library);
  auto msg = library.LookupStruct("Message");
  ASSERT_NOT_NULL(msg);
  ASSERT_EQ(msg->members.size(), 1);

  auto type = msg->members[0].type_ctor->type;
  ASSERT_EQ(type->kind, fidl::flat::Type::Kind::kPrimitive);
  ASSERT_FALSE(type->IsNullable());

  auto primitive_type = static_cast<const fidl::flat::PrimitiveType*>(type);
  ASSERT_EQ(primitive_type->subtype, fidl::types::PrimitiveSubtype::kInt16);

  auto invocation = msg->members[0].type_ctor->resolved_params;
  ASSERT_NOT_NULL(invocation.from_alias);
  EXPECT_STREQ(fidl::NameFlatName(invocation.from_alias->name).c_str(), "example/alias_of_int16");
  EXPECT_NULL(invocation.element_type_resolved);
  EXPECT_NULL(invocation.size_resolved);
  EXPECT_EQ(invocation.nullability, fidl::types::Nullability::kNonnullable);
}

TEST(AliasTests, GoodPrimitiveAliasBeforeUse) {
  TestLibrary library(R"FIDL(library example;

alias alias_of_int16 = int16;

type Message = struct {
    f alias_of_int16;
};
)FIDL");
  ASSERT_COMPILED(library);
  auto msg = library.LookupStruct("Message");
  ASSERT_NOT_NULL(msg);
  ASSERT_EQ(msg->members.size(), 1);

  auto type = msg->members[0].type_ctor->type;
  ASSERT_EQ(type->kind, fidl::flat::Type::Kind::kPrimitive);
  ASSERT_FALSE(type->IsNullable());

  auto primitive_type = static_cast<const fidl::flat::PrimitiveType*>(type);
  ASSERT_EQ(primitive_type->subtype, fidl::types::PrimitiveSubtype::kInt16);

  auto invocation = msg->members[0].type_ctor->resolved_params;
  ASSERT_NOT_NULL(invocation.from_alias);
  EXPECT_STREQ(fidl::NameFlatName(invocation.from_alias->name).c_str(), "example/alias_of_int16");
  EXPECT_NULL(invocation.element_type_resolved);
  EXPECT_NULL(invocation.size_resolved);
  EXPECT_EQ(invocation.nullability, fidl::types::Nullability::kNonnullable);
}

TEST(AliasTests, BadSelfReferentialAlias) {
  TestLibrary library(R"FIDL(
library example;

alias uint32 = uint32;

type Message = struct {
    f uint32;
};
)FIDL");
  library.ExpectFail(fidl::ErrIncludeCycle, "alias 'uint32' -> alias 'uint32'");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadNoOptionalOnPrimitive) {
  TestLibrary library;
  library.AddFile("bad/fi-0156.test.fidl");
  library.ExpectFail(fidl::ErrCannotBeOptional, "int16");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadMultipleConstraintsOnPrimitive) {
  TestLibrary library(R"FIDL(
library test.optionals;

type Bad = struct {
    opt_num int64:<1, 2, 3>;
};

)FIDL");
  library.ExpectFail(fidl::ErrTooManyConstraints, "int64", 0, 3);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadInvalidSizeConstraintType) {
  TestLibrary library;
  library.AddFile("bad/fi-0101-a.test.fidl");
  library.ExpectFail(fidl::ErrTypeCannotBeConvertedToType, "\"255\"", "string:3", "uint32");
  library.ExpectFail(fidl::ErrCouldNotResolveSizeBound);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadInvalidSizeConstraintIsNotValue) {
  TestLibrary library;
  library.AddFile("bad/fi-0101-b.test.fidl");
  library.ExpectFail(fidl::ErrCouldNotResolveSizeBound);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadNoOptionalOnAliasedPrimitive) {
  TestLibrary library(R"FIDL(
library test.optionals;

alias alias = int64;

type Bad = struct {
    opt_num alias:optional;
};

)FIDL");
  library.ExpectFail(fidl::ErrCannotBeOptional, "alias");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, GoodVectorParameterizedOnDecl) {
  TestLibrary library(R"FIDL(library example;

type Message = struct {
    f alias_of_vector_of_string;
};

alias alias_of_vector_of_string = vector<string>;
)FIDL");
  ASSERT_COMPILED(library);
  auto msg = library.LookupStruct("Message");
  ASSERT_NOT_NULL(msg);
  ASSERT_EQ(msg->members.size(), 1);

  auto type = msg->members[0].type_ctor->type;
  ASSERT_EQ(type->kind, fidl::flat::Type::Kind::kVector);
  ASSERT_FALSE(type->IsNullable());

  auto vector_type = static_cast<const fidl::flat::VectorType*>(type);
  ASSERT_EQ(vector_type->element_type->kind, fidl::flat::Type::Kind::kString);
  ASSERT_EQ(vector_type->ElementCount(), fidl::flat::Size::Max().value);

  auto invocation = msg->members[0].type_ctor->resolved_params;
  ASSERT_NOT_NULL(invocation.from_alias);
  EXPECT_STREQ(fidl::NameFlatName(invocation.from_alias->name).c_str(),
               "example/alias_of_vector_of_string");
  EXPECT_NULL(invocation.element_type_resolved);
  EXPECT_NULL(invocation.size_resolved);
  EXPECT_EQ(invocation.nullability, fidl::types::Nullability::kNonnullable);
}

TEST(AliasTests, BadVectorParameterizedOnUse) {
  TestLibrary library(R"FIDL(
library example;

type Message = struct {
    f alias_of_vector<uint8>;
};

alias alias_of_vector = vector;
)FIDL");
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "alias_of_vector", 0, 1);
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "vector", 1, 0);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadVectorBoundedOnDecl) {
  TestLibrary library(R"FIDL(
library example;

type Message = struct {
    f alias_of_vector_max_8<string>;
};

alias alias_of_vector_max_8 = vector:8;
)FIDL");
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "alias_of_vector_max_8", 0, 1);
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "vector", 1, 0);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, GoodVectorBoundedOnUse) {
  TestLibrary library(R"FIDL(library example;

type Message = struct {
    f alias_of_vector_of_string:8;
};

alias alias_of_vector_of_string = vector<string>;
)FIDL");
  ASSERT_COMPILED(library);
  auto msg = library.LookupStruct("Message");
  ASSERT_NOT_NULL(msg);
  ASSERT_EQ(msg->members.size(), 1);

  auto type = msg->members[0].type_ctor->type;
  ASSERT_EQ(type->kind, fidl::flat::Type::Kind::kVector);
  ASSERT_FALSE(type->IsNullable());

  auto vector_type = static_cast<const fidl::flat::VectorType*>(type);
  ASSERT_EQ(vector_type->element_type->kind, fidl::flat::Type::Kind::kString);
  ASSERT_EQ(vector_type->ElementCount(), 8u);

  auto invocation = msg->members[0].type_ctor->resolved_params;
  ASSERT_NOT_NULL(invocation.from_alias);
  EXPECT_STREQ(fidl::NameFlatName(invocation.from_alias->name).c_str(),
               "example/alias_of_vector_of_string");
  EXPECT_NULL(invocation.element_type_resolved);
  EXPECT_NOT_NULL(invocation.size_resolved);
  EXPECT_EQ(static_cast<uint32_t>(*invocation.size_resolved), 8u);
  EXPECT_EQ(invocation.nullability, fidl::types::Nullability::kNonnullable);
}

TEST(AliasTests, GoodUnboundedVectorBoundTwice) {
  TestLibrary library;
  library.AddFile("good/fi-0158.test.fidl");

  ASSERT_COMPILED(library);
}

TEST(AliasTests, GoodVectorNullableOnDecl) {
  TestLibrary library(R"FIDL(library example;

type Message = struct {
    f alias_of_vector_of_string_nullable;
};

alias alias_of_vector_of_string_nullable = vector<string>:optional;
)FIDL");
  ASSERT_COMPILED(library);
  auto msg = library.LookupStruct("Message");
  ASSERT_NOT_NULL(msg);
  ASSERT_EQ(msg->members.size(), 1);

  auto type = msg->members[0].type_ctor->type;
  ASSERT_EQ(type->kind, fidl::flat::Type::Kind::kVector);
  ASSERT_TRUE(type->IsNullable());

  auto vector_type = static_cast<const fidl::flat::VectorType*>(type);
  ASSERT_EQ(vector_type->element_type->kind, fidl::flat::Type::Kind::kString);
  ASSERT_EQ(vector_type->ElementCount(), fidl::flat::Size::Max().value);

  auto invocation = msg->members[0].type_ctor->resolved_params;
  ASSERT_NOT_NULL(invocation.from_alias);
  EXPECT_STREQ(fidl::NameFlatName(invocation.from_alias->name).c_str(),
               "example/alias_of_vector_of_string_nullable");
  EXPECT_NULL(invocation.element_type_resolved);
  EXPECT_NULL(invocation.size_resolved);
  EXPECT_EQ(invocation.nullability, fidl::types::Nullability::kNonnullable);
}

TEST(AliasTests, GoodVectorNullableOnUse) {
  TestLibrary library(R"FIDL(library example;

type Message = struct {
    f alias_of_vector_of_string:optional;
};

alias alias_of_vector_of_string = vector<string>;
)FIDL");
  ASSERT_COMPILED(library);
  auto msg = library.LookupStruct("Message");
  ASSERT_NOT_NULL(msg);
  ASSERT_EQ(msg->members.size(), 1);

  auto type = msg->members[0].type_ctor->type;
  ASSERT_EQ(type->kind, fidl::flat::Type::Kind::kVector);
  ASSERT_TRUE(type->IsNullable());

  auto vector_type = static_cast<const fidl::flat::VectorType*>(type);
  ASSERT_EQ(vector_type->element_type->kind, fidl::flat::Type::Kind::kString);
  ASSERT_EQ(vector_type->ElementCount(), fidl::flat::Size::Max().value);

  auto invocation = msg->members[0].type_ctor->resolved_params;
  ASSERT_NOT_NULL(invocation.from_alias);
  EXPECT_STREQ(fidl::NameFlatName(invocation.from_alias->name).c_str(),
               "example/alias_of_vector_of_string");
  EXPECT_NULL(invocation.element_type_resolved);
  EXPECT_NULL(invocation.size_resolved);
  EXPECT_EQ(invocation.nullability, fidl::types::Nullability::kNullable);
}

TEST(AliasTests, BadCannotParameterizeTwice) {
  TestLibrary library(R"FIDL(
library example;

type Message = struct {
    f alias_of_vector_of_string<string>;
};

alias alias_of_vector_of_string = vector<string>;
)FIDL");
  library.ExpectFail(fidl::ErrWrongNumberOfLayoutParameters, "alias_of_vector_of_string", 0, 1);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadCannotBoundTwice) {
  TestLibrary library;
  library.AddFile("bad/fi-0158.test.fidl");

  library.ExpectFail(fidl::ErrCannotBoundTwice, "ByteVec256");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadCannotNullTwice) {
  TestLibrary library;
  library.AddFile("bad/fi-0160.test.fidl");

  library.ExpectFail(fidl::ErrCannotIndicateOptionalTwice, "MyAlias");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, GoodMultiFileAliasReference) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
library example;

type Protein = struct {
  amino_acids AminoAcids;
};
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
library example;

alias AminoAcids = vector<uint64>:32;
)FIDL");

  ASSERT_COMPILED(library);
}

TEST(AliasTests, GoodMultiFileNullableAliasReference) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
library example;

type Protein = struct {
    amino_acids AminoAcids:optional;
};
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
library example;

alias AminoAcids = vector<uint64>:32;
)FIDL");

  ASSERT_COMPILED(library);
}

TEST(AliasTests, BadRecursiveAlias) {
  TestLibrary library(R"FIDL(
library example;

alias TheAlias = TheStruct;

type TheStruct = struct {
    many_mini_me vector<TheAlias>;
};
)FIDL");

  library.ExpectFail(fidl::ErrIncludeCycle,
                     "alias 'TheAlias' -> struct 'TheStruct' -> alias 'TheAlias'");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, BadCompoundIdentifier) {
  TestLibrary library(R"FIDL(
library example;

alias foo.bar.baz = uint8;
)FIDL");

  library.ExpectFail(fidl::ErrUnexpectedTokenOfKind,
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kDot),
                     fidl::Token::KindAndSubkind(fidl::Token::Kind::kEqual));
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(AliasTests, GoodUsingLibrary) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared, "dependent.fidl", R"FIDL(
library dependent;

type Bar = struct {
    s int8;
};
)FIDL");
  ASSERT_COMPILED(dependency);

  TestLibrary library(&shared, "example.fidl", R"FIDL(
library example;

using dependent;

alias Bar2 = dependent.Bar;

)FIDL");
  ASSERT_COMPILED(library);
}

}  // namespace
