// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/include/fidl/flat/types.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/include/fidl/names.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(ServiceTests, GoodEmptyService) {
  TestLibrary library(R"FIDL(library example;

service SomeService {};
)FIDL");
  ASSERT_COMPILED(library);

  auto service = library.LookupService("SomeService");
  ASSERT_NE(service, nullptr);

  EXPECT_EQ(service->members.size(), 0u);
}

TEST(ServiceTests, GoodService) {
  TestLibrary library(R"FIDL(library example;

protocol SomeProtocol1 {};
protocol SomeProtocol2 {};

service SomeService {
    some_protocol_first_first client_end:SomeProtocol1;
    some_protocol_first_second client_end:SomeProtocol1;
    some_protocol_second client_end:SomeProtocol2;
};
)FIDL");
  ASSERT_COMPILED(library);

  auto service = library.LookupService("SomeService");
  ASSERT_NE(service, nullptr);

  EXPECT_EQ(service->members.size(), 3u);
  const auto& member0 = service->members[0];
  EXPECT_EQ(member0.name.data(), "some_protocol_first_first");
  const auto* type0 = static_cast<const fidl::flat::TransportSideType*>(member0.type_ctor->type);
  EXPECT_EQ(fidl::NameFlatName(type0->protocol_decl->name), "example/SomeProtocol1");
  const auto& member1 = service->members[1];
  EXPECT_EQ(member1.name.data(), "some_protocol_first_second");
  const auto* type1 = static_cast<const fidl::flat::TransportSideType*>(member1.type_ctor->type);
  EXPECT_EQ(fidl::NameFlatName(type1->protocol_decl->name), "example/SomeProtocol1");
  const auto& member2 = service->members[2];
  EXPECT_EQ(member2.name.data(), "some_protocol_second");
  const auto* type2 = static_cast<const fidl::flat::TransportSideType*>(member2.type_ctor->type);
  EXPECT_EQ(fidl::NameFlatName(type2->protocol_decl->name), "example/SomeProtocol2");
}

TEST(ServiceTests, BadCannotHaveConflictingMembers) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {};

service MyService {
    my_service_member client_end:MyProtocol;
    my_service_member client_end:MyProtocol;
};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollision, fidl::flat::Element::Kind::kServiceMember,
                     "my_service_member", fidl::flat::Element::Kind::kServiceMember,
                     "example.fidl:7:5");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ServiceTests, BadNoNullableProtocolMembers) {
  TestLibrary library;
  library.AddFile("bad/fi-0088.test.fidl");
  library.ExpectFail(fidl::ErrOptionalServiceMember);
  library.ExpectFail(fidl::ErrOptionalServiceMember);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ServiceTests, BadOnlyProtocolMembers) {
  TestLibrary library(R"FIDL(
library example;

type NotAProtocol = struct {};

service SomeService {
    not_a_protocol NotAProtocol;
};

)FIDL");
  library.ExpectFail(fidl::ErrOnlyClientEndsInServices);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ServiceTests, BadNoServerEnds) {
  TestLibrary library;
  library.AddFile("bad/fi-0112.test.fidl");
  library.ExpectFail(fidl::ErrOnlyClientEndsInServices);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ServiceTests, BadCannotUseServicesInDecls) {
  TestLibrary library(R"FIDL(
library example;

service SomeService {};

type CannotUseService = struct {
    svc SomeService;
};

)FIDL");
  library.ExpectFail(fidl::ErrExpectedType);
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(ServiceTests, BadCannotUseMoreThanOneProtocolTransportKind) {
  TestLibrary library;
  library.AddFile("bad/fi-0113.test.fidl");
  library.ExpectFail(fidl::ErrMismatchedTransportInServices, "b", "Driver", "a", "Channel");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

}  // namespace
