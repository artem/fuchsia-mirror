// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/tests/test_library.h"

// This file tests basic validation of the @available attribute. It does not
// test the behavior of versioning beyond that.

namespace fidlc {
namespace {

TEST(VersioningAttributeTests, BadMultipleLibraryDeclarationsAgree) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrDuplicateAttribute, "available", "first.fidl:2:2");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadMultipleLibraryDeclarationsDisagree) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
@available(added=2)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrDuplicateAttribute, "available", "first.fidl:2:2");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadMultipleLibraryDeclarationsHead) {
  TestLibrary library;
  library.AddSource("first.fidl", R"FIDL(
@available(added=HEAD)
library example;
)FIDL");
  library.AddSource("second.fidl", R"FIDL(
@available(added=HEAD)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrDuplicateAttribute, "available", "first.fidl:2:2");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, GoodAllArgumentsOnLibrary) {
  TestLibrary library(R"FIDL(
@available(platform="notexample", added=1, deprecated=2, removed=3, note="use xyz instead", legacy=false)
library example;
)FIDL");
  library.SelectVersion("notexample", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningAttributeTests, GoodAllArgumentsOnDecl) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=2, removed=3, note="use xyz instead", legacy=false)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningAttributeTests, GoodAllArgumentsOnMember) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    @available(added=1, deprecated=2, removed=3, note="use xyz instead", legacy=false)
    member string;
};
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningAttributeTests, GoodAttributeOnEverything) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1)
const CONST uint32 = 1;

@available(added=1)
alias Alias = string;

// TODO(https://fxbug.dev/42158155): Uncomment.
// @available(added=1)
// type Type = string;

@available(added=1)
type Bits = bits {
    @available(added=1)
    MEMBER = 1;
};

@available(added=1)
type Enum = enum {
    @available(added=1)
    MEMBER = 1;
};

@available(added=1)
type Struct = struct {
    @available(added=1)
    member string;
};

@available(added=1)
type Table = table {
    @available(added=1)
    1: member string;
};

@available(added=1)
type Union = union {
    @available(added=1)
    1: member string;
};

@available(added=1)
protocol ProtocolToCompose {};

@available(added=1)
protocol Protocol {
    @available(added=1)
    compose ProtocolToCompose;

    @available(added=1)
    Method() -> ();
};

@available(added=1)
service Service {
    @available(added=1)
    member client_end:Protocol;
};

@available(added=1)
resource_definition Resource : uint32 {
    properties {
        @available(added=1)
        subtype flexible enum : uint32 {};
    };
};
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);

  auto& unfiltered_decls = library.all_libraries()->Lookup("example")->declaration_order;
  auto& filtered_decls = library.declaration_order();
  // Because everything has the same availability, nothing gets split.
  EXPECT_EQ(unfiltered_decls.size(), filtered_decls.size());
}

// TODO(https://fxbug.dev/42146818): Currently attributes `@HERE type Foo = struct {};` and
// `type Foo = @HERE struct {};` are interchangeable. We just disallow using
// both at once (ErrRedundantAttributePlacement). However, @available on the
// anonymous layout is confusing so maybe we should rethink this design.
TEST(VersioningAttributeTests, GoodAnonymousLayoutTopLevel) {
  auto source = R"FIDL(
@available(added=1)
library example;

type Foo = @available(added=2) struct {};
)FIDL";

  {
    TestLibrary library(source);
    library.SelectVersion("example", "1");
    ASSERT_COMPILED(library);
    ASSERT_FALSE(library.HasStruct("Foo"));
  }
  {
    TestLibrary library(source);
    library.SelectVersion("example", "2");
    ASSERT_COMPILED(library);
    ASSERT_TRUE(library.HasStruct("Foo"));
  }
}

TEST(VersioningAttributeTests, BadAnonymousLayoutInMember) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

type Foo = struct {
    member @available(added=2) struct {};
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAttributePlacement, "available");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadInvalidVersionZero) {
  TestLibrary library(R"FIDL(
@available(added=0)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, "0");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, GoodVersionMinNormal) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.version_added(), Version::From(1).value());
}

TEST(VersioningAttributeTests, GoodVersionMaxNormal) {
  TestLibrary library(R"FIDL(
@available(added=0x7fffffff) // 2^31-1
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.version_added(), Version::From(0x7fffffff).value());
}

TEST(VersioningAttributeTests, BadInvalidVersionAboveMaxNormal) {
  TestLibrary library(R"FIDL(
@available(added=0x80000000) // 2^31
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, "0x80000000");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadInvalidVersionUnknownReserved) {
  TestLibrary library(R"FIDL(
@available(added=0x8abc1234)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, "0x8abc1234");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, GoodVersionNextName) {
  TestLibrary library(R"FIDL(
@available(added=NEXT)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.version_added(), Version::kNext);
}

TEST(VersioningAttributeTests, GoodVersionNextNumber) {
  TestLibrary library(R"FIDL(
@available(added=0xFFD00000)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.version_added(), Version::kNext);
}

TEST(VersioningAttributeTests, GoodVersionHeadName) {
  TestLibrary library(R"FIDL(
@available(added=HEAD)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.version_added(), Version::kHead);
}

TEST(VersioningAttributeTests, GoodVersionHeadNumber) {
  TestLibrary library(R"FIDL(
@available(added=0xFFE00000)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  ASSERT_COMPILED(library);
  EXPECT_EQ(library.version_added(), Version::kHead);
}

TEST(VersioningAttributeTests, BadInvalidVersionLegacyName) {
  TestLibrary library(R"FIDL(
@available(added=LEGACY)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, "LEGACY");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadInvalidVersionLegacyNumber) {
  TestLibrary library(R"FIDL(
@available(added=0xFFF00000)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidVersion, "0xFFF00000");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadInvalidVersionNegative) {
  TestLibrary library(R"FIDL(
@available(added=-1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrCouldNotResolveAttributeArg);
  library.ExpectFail(ErrConstantOverflowsType, "-1", "uint32");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadInvalidVersionOverflowUint32) {
  TestLibrary library(R"FIDL(
@available(added=0x100000000) // 2^32
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrCouldNotResolveAttributeArg);
  library.ExpectFail(ErrConstantOverflowsType, "0x100000000", "uint32");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadNoArguments) {
  TestLibrary library;
  library.AddFile("bad/fi-0147.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrAvailableMissingArguments);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadLibraryMissingAddedOnlyRemoved) {
  TestLibrary library;
  library.AddFile("bad/fi-0150-a.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrLibraryAvailabilityMissingAdded);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadLibraryMissingAddedOnlyPlatform) {
  TestLibrary library;
  library.AddFile("bad/fi-0150-b.test.fidl");
  library.SelectVersion("foo", "HEAD");
  library.ExpectFail(ErrLibraryAvailabilityMissingAdded);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadLibraryReplaced) {
  TestLibrary library;
  library.AddFile("bad/fi-0204.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrLibraryReplaced);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadNoteWithoutDeprecation) {
  TestLibrary library;
  library.AddFile("bad/fi-0148.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrNoteWithoutDeprecation);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadRemovedAndReplaced) {
  TestLibrary library;
  library.AddFile("bad/fi-0203.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrRemovedAndReplaced);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadPlatformNotOnLibrary) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(platform="bad")
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrPlatformNotOnLibrary);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadUseInUnversionedLibrary) {
  TestLibrary library;
  library.AddFile("bad/fi-0151.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrMissingLibraryAvailability, "test.bad.fi0151");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadUseInUnversionedLibraryReportedOncePerAttribute) {
  TestLibrary library(R"FIDL(
library example;

@available(added=1)
type Foo = struct {
    @available(added=2)
    member1 bool;
    member2 bool;
};
)FIDL");
  library.SelectVersion("example", "HEAD");
  // Note: Only twice, not a third time for member2.
  library.ExpectFail(ErrMissingLibraryAvailability, "example");
  library.ExpectFail(ErrMissingLibraryAvailability, "example");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadAddedEqualsRemoved) {
  TestLibrary library;
  library.AddFile("bad/fi-0154-a.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadAddedEqualsReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=2, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadAddedGreaterThanRemoved) {
  TestLibrary library(R"FIDL(
@available(added=2, removed=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadAddedGreaterThanReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=3, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, GoodAddedEqualsDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=1, deprecated=1)
library example;
)FIDL");
  library.SelectVersion("example", "1");
  ASSERT_COMPILED(library);
}

TEST(VersioningAttributeTests, BadAddedGreaterThanDeprecated) {
  TestLibrary library(R"FIDL(
@available(added=2, deprecated=1)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadDeprecatedEqualsRemoved) {
  TestLibrary library;
  library.AddFile("bad/fi-0154-b.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadDeprecatedEqualsReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=2, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadDeprecatedGreaterThanRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, deprecated=3, removed=2)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < removed");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadDeprecatedGreaterThanReplaced) {
  TestLibrary library(R"FIDL(
@available(added=1)
library example;

@available(added=1, deprecated=3, replaced=2)
type Foo = struct {};
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrInvalidAvailabilityOrder, "added <= deprecated < replaced");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadLegacyTrueNotRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, legacy=true)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrLegacyWithoutRemoval, "legacy");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadLegacyFalseNotRemoved) {
  TestLibrary library(R"FIDL(
@available(added=1, legacy=false)
library example;
)FIDL");
  library.SelectVersion("example", "HEAD");
  library.ExpectFail(ErrLegacyWithoutRemoval, "legacy");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(VersioningAttributeTests, BadLegacyTrueNotRemovedMethod) {
  TestLibrary library;
  library.AddFile("bad/fi-0182.test.fidl");
  library.SelectVersion("test", "HEAD");
  library.ExpectFail(ErrLegacyWithoutRemoval, "legacy");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
}  // namespace fidlc
