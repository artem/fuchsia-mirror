// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sstream>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/utils.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(CanonicalNamesTests, BadCollision) {
  TestLibrary library;
  library.AddFile("bad/fi-0035.test.fidl");
  library.ExpectFail(fidl::ErrNameCollisionCanonical, "Color", "COLOR", "bad/fi-0035.test.fidl:6:7",
                     "color");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, GoodCollisionFixRename) {
  TestLibrary library;
  library.AddFile("good/fi-0035.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodTopLevel) {
  TestLibrary library(R"FIDL(library example;

alias foobar = bool;
const f_oobar bool = true;
type fo_obar = struct {};
type foo_bar = struct {};
type foob_ar = table {};
type fooba_r = strict union {
    1: x bool;
};
type FoObAr = strict enum {
    A = 1;
};
type FooBaR = strict bits {
    A = 1;
};
protocol FoObaR {};
service FOoBAR {};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodAttributes) {
  TestLibrary library(R"FIDL(library example;

@foobar
@foo_bar
@f_o_o_b_a_r
type Example = struct {};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodAttributeArguments) {
  TestLibrary library(R"FIDL(library example;

@some_attribute(foobar="", foo_bar="", f_o_o_b_a_r="")
type Example = struct {};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodStructMembers) {
  TestLibrary library(R"FIDL(library example;

type Example = struct {
    foobar bool;
    foo_bar bool;
    f_o_o_b_a_r bool;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodTableMembers) {
  TestLibrary library(R"FIDL(library example;

type Example = table {
    1: foobar bool;
    2: foo_bar bool;
    3: f_o_o_b_a_r bool;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodUnionMembers) {
  TestLibrary library(R"FIDL(library example;

type Example = strict union {
    1: foobar bool;
    2: foo_bar bool;
    3: f_o_o_b_a_r bool;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodEnumMembers) {
  TestLibrary library(R"FIDL(library example;

type Example = strict enum {
    foobar = 1;
    foo_bar = 2;
    f_o_o_b_a_r = 3;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodBitsMembers) {
  TestLibrary library(R"FIDL(library example;

type Example = strict bits {
    foobar = 1;
    foo_bar = 2;
    f_o_o_b_a_r = 4;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodProtocolMethods) {
  TestLibrary library(R"FIDL(library example;

protocol Example {
    foobar() -> ();
    foo_bar() -> ();
    f_o_o_b_a_r() -> ();
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodMethodParameters) {
  TestLibrary library(R"FIDL(library example;

protocol Example {
    example(struct {
        foobar bool;
        foo_bar bool;
        f_o_o_b_a_r bool;
    }) -> ();
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodMethodResults) {
  TestLibrary library(R"FIDL(library example;

protocol Example {
    example() -> (struct {
        foobar bool;
        foo_bar bool;
        f_o_o_b_a_r bool;
    });
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodServiceMembers) {
  TestLibrary library(R"FIDL(library example;

protocol P {};
service Example {
    foobar client_end:P;
    foo_bar client_end:P;
    f_o_o_b_a_r client_end:P;
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodResourceProperties) {
  TestLibrary library(R"FIDL(library example;

resource_definition Example {
    properties {
        // This property is required for compilation, but is not otherwise under test.
        subtype flexible enum : uint32 {};
        foobar uint32;
        foo_bar uint32;
        f_o_o_b_a_r uint32;
    };
};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodUpperAcronym) {
  TestLibrary library(R"FIDL(library example;

type HTTPServer = struct {};
type httpserver = struct {};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodCurrentLibrary) {
  TestLibrary library(R"FIDL(library example;

type example = struct {};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, GoodDependentLibrary) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared, "foobar.fidl", R"FIDL(library foobar;

type Something = struct {};
)FIDL");
  ASSERT_COMPILED(dependency);

  TestLibrary library(&shared, "example.fidl", R"FIDL(
library example;

using foobar;

alias f_o_o_b_a_r = foobar.Something;
const f_oobar bool = true;
type fo_obar = struct {};
type foo_bar = struct {};
type foob_ar = table {};
type fooba_r = union { 1: x bool; };
type FoObAr = enum { A = 1; };
type FooBaR = bits { A = 1; };
protocol FoObaR {};
service FOoBAR {};
)FIDL");
  ASSERT_COMPILED(library);
}

TEST(CanonicalNamesTests, BadTopLevel) {
  const auto lower = {
      "alias fooBar = bool;",                 // these comments prevent clang-format
      "const fooBar bool = true;",            // from packing multiple items per line
      "type fooBar = struct {};",             //
      "type fooBar = struct {};",             //
      "type fooBar = table {};",              //
      "type fooBar = union { 1: x bool; };",  //
      "type fooBar = enum { A = 1; };",       //
      "type fooBar = bits { A = 1; };",       //
      "protocol fooBar {};",                  //
      "service fooBar {};",                   //
  };
  const auto upper = {
      "alias FooBar = bool;",                 //
      "const FooBar bool = true;",            //
      "type FooBar = struct {};",             //
      "type FooBar = struct {};",             //
      "type FooBar = table {};",              //
      "type FooBar = union { 1: x bool; };",  //
      "type FooBar = enum { A = 1; };",       //
      "type FooBar = bits { A = 1; };",       //
      "protocol FooBar {};",                  //
      "service FooBar {};",                   //
  };

  for (const auto line1 : lower) {
    for (const auto line2 : upper) {
      std::ostringstream s;
      s << "library example;\n\n" << line1 << '\n' << line2 << '\n';
      const auto fidl = s.str();
      TestLibrary library(fidl);
      ASSERT_FALSE(library.Compile(), "%s", fidl.c_str());
      const auto& errors = library.errors();
      ASSERT_EQ(errors.size(), 1, "%s", fidl.c_str());
      ASSERT_ERR(errors[0], fidl::ErrNameCollisionCanonical, "%s", fidl.c_str());
      ASSERT_SUBSTR(errors[0]->msg.c_str(), "fooBar", "%s", fidl.c_str());
      ASSERT_SUBSTR(errors[0]->msg.c_str(), "FooBar", "%s", fidl.c_str());
      ASSERT_SUBSTR(errors[0]->msg.c_str(), "foo_bar", "%s", fidl.c_str());
    }
  }
}

TEST(CanonicalNamesTests, BadAttributes) {
  TestLibrary library(R"FIDL(
library example;

@fooBar
@FooBar
type Example = struct {};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateAttributeCanonical, "FooBar", "fooBar", "example.fidl:4:2",
                     "foo_bar");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadAttributeArguments) {
  TestLibrary library(R"FIDL(
library example;

@some_attribute(fooBar="", FooBar="")
type Example = struct {};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateAttributeArgCanonical, "some_attribute", "FooBar", "fooBar",
                     "example.fidl:4:17", "foo_bar");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadStructMembers) {
  TestLibrary library(R"FIDL(
library example;

type MyStruct = struct {
    myStructMember string;
    MyStructMember uint64;
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kStructMember, "MyStructMember", "myStructMember",
                     "example.fidl:5:5", "my_struct_member");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadTableMembers) {
  TestLibrary library(R"FIDL(
library example;

type MyTable = table {
    1: myField bool;
    2: MyField bool;
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kTableMember, "MyField", "myField",
                     "example.fidl:5:8", "my_field");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadUnionMembers) {
  TestLibrary library(R"FIDL(
library example;

type MyUnion = union {
    1: myVariant bool;
    2: MyVariant bool;
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kUnionMember, "MyVariant", "myVariant",
                     "example.fidl:5:8", "my_variant");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadEnumMembers) {
  TestLibrary library(R"FIDL(
library example;

type Example = enum {
  fooBar = 1;
  FooBar = 2;
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical, fidl::flat::Element::Kind::kEnumMember,
                     "FooBar", "fooBar", "example.fidl:5:3", "foo_bar");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadBitsMembers) {
  TestLibrary library(R"FIDL(
library example;

type MyBits = bits {
    fooBar = 1;
    FooBar = 2;
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical, fidl::flat::Element::Kind::kBitsMember,
                     "FooBar", "fooBar", "example.fidl:5:5", "foo_bar");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadProtocolMethods) {
  TestLibrary library;
  library.AddFile("bad/fi-0079.test.fidl");

  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kProtocolMethod, "MyMethod", "myMethod",
                     library.find_source_span("myMethod"), "my_method");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadMethodParameters) {
  TestLibrary library(R"FIDL(
library example;

protocol Example {
  example(struct { fooBar bool; FooBar bool; }) -> ();
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kStructMember, "FooBar", "fooBar",
                     "example.fidl:5:20", "foo_bar");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadMethodResults) {
  TestLibrary library(R"FIDL(
library example;

protocol Example {
  example() -> (struct { fooBar bool; FooBar bool; });
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kStructMember, "FooBar", "fooBar",
                     "example.fidl:5:26", "foo_bar");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadServiceMembers) {
  TestLibrary library(R"FIDL(
library example;

protocol MyProtocol {};

service MyService {
    myServiceMember client_end:MyProtocol;
    MyServiceMember client_end:MyProtocol;
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kServiceMember, "MyServiceMember",
                     "myServiceMember", "example.fidl:7:5", "my_service_member");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadResourceProperties) {
  TestLibrary library(R"FIDL(
library example;

resource_definition MyResource : uint32 {
    properties {
        subtype flexible enum : uint32 {};
        rights uint32;
        Rights uint32;
    };
};
)FIDL");
  library.ExpectFail(fidl::ErrDuplicateElementNameCanonical,
                     fidl::flat::Element::Kind::kResourceProperty, "Rights", "rights",
                     "example.fidl:7:9", "rights");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadMemberValues) {
  TestLibrary library;
  library.AddFile("bad/fi-0054.test.fidl");
  library.ExpectFail(fidl::ErrMemberNotFound, "enum 'Enum'", "FOO_BAR");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadUpperAcronym) {
  TestLibrary library(R"FIDL(
library example;

type HTTPServer = struct {};
type HttpServer = struct {};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollisionCanonical, "HttpServer", "HTTPServer",
                     "example.fidl:4:6", "http_server");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadDependentLibrary) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared, "foobar.fidl", R"FIDL(library foobar;

type Something = struct {};
)FIDL");
  ASSERT_COMPILED(dependency);

  TestLibrary library(&shared, "lib.fidl", R"FIDL(
library example;

using foobar;

alias FOOBAR = foobar.Something;
)FIDL");
  library.ExpectFail(fidl::ErrDeclNameConflictsWithLibraryImportCanonical, "FOOBAR", "foobar");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadVariousCollisions) {
  const auto base_names = {
      "a", "a1", "x_single_start", "single_end_x", "x_single_both_x", "single_x_middle",
  };
  const auto functions = {
      fidl::utils::to_lower_snake_case,
      fidl::utils::to_upper_snake_case,
      fidl::utils::to_lower_camel_case,
      fidl::utils::to_upper_camel_case,
  };

  for (const auto base_name : base_names) {
    for (const auto f1 : functions) {
      for (const auto f2 : functions) {
        std::ostringstream s;
        const auto name1 = f1(base_name);
        const auto name2 = f2(base_name);
        s << "library example;\n\ntype " << name1 << " = struct {};\ntype " << name2
          << " = struct {};\n";
        const auto fidl = s.str();
        TestLibrary library(fidl);
        ASSERT_FALSE(library.Compile(), "%s", fidl.c_str());
        const auto& errors = library.errors();
        ASSERT_EQ(errors.size(), 1, "%s", fidl.c_str());
        if (name1 == name2) {
          ASSERT_ERR(errors[0], fidl::ErrNameCollision, "%s", fidl.c_str());
          ASSERT_SUBSTR(errors[0]->msg.c_str(), name1.c_str(), "%s", fidl.c_str());
        } else {
          ASSERT_ERR(errors[0], fidl::ErrNameCollisionCanonical, "%s", fidl.c_str());
          ASSERT_SUBSTR(errors[0]->msg.c_str(), name1.c_str(), "%s", fidl.c_str());
          ASSERT_SUBSTR(errors[0]->msg.c_str(), name2.c_str(), "%s", fidl.c_str());
          ASSERT_SUBSTR(errors[0]->msg.c_str(), fidl::utils::canonicalize(name1).c_str(), "%s",
                        fidl.c_str());
        }
      }
    }
  }
}

TEST(CanonicalNamesTests, BadConsecutiveUnderscores) {
  TestLibrary library(R"FIDL(
library example;

type it_is_the_same = struct {};
type it__is___the____same = struct {};
)FIDL");
  library.ExpectFail(fidl::ErrNameCollisionCanonical, "it_is_the_same", "it__is___the____same",
                     "example.fidl:5:6", "it_is_the_same");
  ASSERT_COMPILER_DIAGNOSTICS(library);
}

TEST(CanonicalNamesTests, BadInconsistentTypeSpelling) {
  const auto decl_templates = {
      "alias %s = bool;",                 //
      "type %s = struct {};",             //
      "type %s = struct {};",             //
      "type %s = table {};",              //
      "type %s = union { 1: x bool; };",  //
      "type %s = enum { A = 1; };",       //
      "type %s = bits { A = 1; };",       //
  };
  const auto use_template = "type Example = struct { val %s; };";

  const auto names = {
      std::make_pair("foo_bar", "FOO_BAR"),
      std::make_pair("FOO_BAR", "foo_bar"),
      std::make_pair("fooBar", "FooBar"),
  };

  for (const auto decl_template : decl_templates) {
    for (const auto& [decl_name, use_name] : names) {
      std::string decl(decl_template), use(use_template);
      decl.replace(decl.find("%s"), 2, decl_name);
      use.replace(use.find("%s"), 2, use_name);
      std::ostringstream s;
      s << "library example;\n\n" << decl << '\n' << use << '\n';
      const auto fidl = s.str();
      TestLibrary library(fidl);
      ASSERT_FALSE(library.Compile(), "%s", fidl.c_str());
      const auto& errors = library.errors();
      ASSERT_EQ(errors.size(), 1, "%s", fidl.c_str());
      ASSERT_ERR(errors[0], fidl::ErrNameNotFound, "%s", fidl.c_str());
      ASSERT_SUBSTR(errors[0]->msg.c_str(), use_name, "%s", fidl.c_str());
    }
  }
}

TEST(CanonicalNamesTests, BadInconsistentConstSpelling) {
  const auto names = {
      std::make_pair("foo_bar", "FOO_BAR"),
      std::make_pair("FOO_BAR", "foo_bar"),
      std::make_pair("fooBar", "FooBar"),
  };

  for (const auto& [decl_name, use_name] : names) {
    std::ostringstream s;
    s << "library example;\n\n"
      << "const " << decl_name << " bool = false;\n"
      << "const EXAMPLE bool = " << use_name << ";\n";
    const auto fidl = s.str();
    TestLibrary library(fidl);
    library.ExpectFail(fidl::ErrNameNotFound, use_name, "example");
    ASSERT_COMPILER_DIAGNOSTICS(library);
  }
}

TEST(CanonicalNamesTests, BadInconsistentEnumMemberSpelling) {
  const auto names = {
      std::make_pair("foo_bar", "FOO_BAR"),
      std::make_pair("FOO_BAR", "foo_bar"),
      std::make_pair("fooBar", "FooBar"),
  };

  for (const auto& [decl_name, use_name] : names) {
    std::ostringstream s;
    s << "library example;\n\n"
      << "type Enum = enum { " << decl_name << " = 1; };\n"
      << "const EXAMPLE Enum = Enum." << use_name << ";\n";
    const auto fidl = s.str();
    TestLibrary library(fidl);
    ASSERT_FALSE(library.Compile(), "%s", fidl.c_str());
    const auto& errors = library.errors();
    ASSERT_EQ(errors.size(), 1, "%s", fidl.c_str());
    ASSERT_ERR(errors[0], fidl::ErrMemberNotFound, "%s", fidl.c_str());
  }
}

TEST(CanonicalNamesTests, BadInconsistentBitsMemberSpelling) {
  const auto names = {
      std::make_pair("foo_bar", "FOO_BAR"),
      std::make_pair("FOO_BAR", "foo_bar"),
      std::make_pair("fooBar", "FooBar"),
  };

  for (const auto& [decl_name, use_name] : names) {
    std::ostringstream s;
    s << "library example;\n\n"
      << "type Bits = bits { " << decl_name << " = 1; };\n"
      << "const EXAMPLE Bits = Bits." << use_name << ";\n";
    const auto fidl = s.str();
    TestLibrary library(fidl);
    ASSERT_FALSE(library.Compile(), "%s", fidl.c_str());
    const auto& errors = library.errors();
    ASSERT_EQ(errors.size(), 1, "%s", fidl.c_str());
    ASSERT_ERR(errors[0], fidl::ErrMemberNotFound, "%s", fidl.c_str());
  }
}

}  // namespace
