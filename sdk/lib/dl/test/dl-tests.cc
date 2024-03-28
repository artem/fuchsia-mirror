// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "../diagnostics.h"
#include "../stateful-error.h"
#include "dl-impl-tests.h"
#include "dl-system-tests.h"

namespace {

using ::testing::MatchesRegex;

template <class Fixture>
using DlTests = Fixture;

// This lists the test fixture classes to run DlTests tests against. The
// DlImplTests fixture is a framework for testing the implementation in
// libdl and the DlSystemTests fixture proxies to the system-provided dynamic
// linker. These tests ensure that both dynamic linker implementations meet
// expectations and behave the same way, with exceptions noted within the test.
using TestTypes = ::testing::Types<
#ifdef __ELF__  // Hard to generate usable test modules for non-ELF host.
#ifdef __Fuchsia__
    dl::testing::DlImplTests<dl::testing::TestFuchsia>,
#endif
// TODO(https://fxbug.dev/324650368): Test fixtures currently retrieve files
// from different prefixed locations depending on the platform. Find a way
// to use a singular API to return the prefixed path specific to the platform so
// that the TestPosix fixture can run on Fuchsia as well.
#ifndef __Fuchsia__
    // libdl's POSIX test fixture can also be tested on Fuchsia and is included
    // for any ELF supported host.
    dl::testing::DlImplTests<dl::testing::TestPosix>,
#endif
#endif
    dl::testing::DlSystemTests>;

TYPED_TEST_SUITE(DlTests, TestTypes);

TEST(DlTests, Diagnostics) {
  {
    dl::Diagnostics diag;
    fit::result<dl::Error, void*> result = diag.ok(nullptr);
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    EXPECT_EQ(result.value(), nullptr);
  }
  {
    dl::Diagnostics diag;
    fit::result<dl::Error> result = diag.ok();
    EXPECT_TRUE(result.is_ok()) << result.error_value();
  }
  {
    dl::Diagnostics diag;
    // Injects the prefix on diag.FormatError while in scope.
    ld::ScopedModuleDiagnostics module_diag{diag, "foo"};
    EXPECT_FALSE(diag.FormatError("some error", elfldltl::FileOffset{0x123u}));
    fit::result<dl::Error, int> result = diag.take_error();
    EXPECT_TRUE(result.is_error());
    EXPECT_EQ(result.error_value().take_str(), "foo: some error at file offset 0x123");
  }
  {
    dl::Diagnostics diag;
    {
      // No effect after it goes out of scope again.
      ld::ScopedModuleDiagnostics module_diag{diag, "foo"};
    }
    EXPECT_FALSE(diag.FormatError("some error"));
    fit::result<dl::Error> result = diag.take_error();
    EXPECT_TRUE(result.is_error());
    EXPECT_EQ(result.error_value().take_str(), "some error");
  }
}

TEST(DlTests, StatefuLError) {
  dl::StatefulError error_state;

  // Initially clear.
  EXPECT_EQ(error_state.GetAndClearLastError(), nullptr);

  // Called with success.
  EXPECT_EQ(error_state(fit::result<dl::Error, int>{fit::ok(3)}, -1), 3);

  // Still clear.
  EXPECT_EQ(error_state.GetAndClearLastError(), nullptr);

  // Called with error.
  EXPECT_EQ(error_state(fit::result<dl::Error, void*>{fit::error{dl::Error{"foo"}}}, nullptr),
            nullptr);

  // Returns error.
  EXPECT_STREQ(error_state.GetAndClearLastError(), "foo");

  // Clear after returning error.
  EXPECT_EQ(error_state.GetAndClearLastError(), nullptr);

  // Two errors without checking in between.
  EXPECT_EQ(error_state(fit::result<dl::Error, void*>{fit::error{dl::Error{"foo"}}}, nullptr),
            nullptr);
  EXPECT_EQ(error_state(fit::result<dl::Error, int>{fit::error{dl::Error{"bar"}}}, -1), -1);

  // Returns error.
  EXPECT_STREQ(error_state.GetAndClearLastError(), "bar");

  // Clear after returning error.
  EXPECT_EQ(error_state.GetAndClearLastError(), nullptr);
}

TYPED_TEST(DlTests, NotFound) {
  auto result = this->DlOpen("does_not_exist.so", RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_error());
  if constexpr (TestFixture::kCanMatchExactError) {
    EXPECT_EQ(result.error_value().take_str(), "cannot open does_not_exist.so");
  } else {
    EXPECT_THAT(result.error_value().take_str(),
                MatchesRegex(".*does_not_exist.so:.*(No such file or directory|ZX_ERR_NOT_FOUND)"));
  }
}

TYPED_TEST(DlTests, InvalidMode) {
  if constexpr (!TestFixture::kCanValidateMode) {
    GTEST_SKIP() << "test requires dlopen to validate mode argment";
  }

  int bad_mode = -1;
  // The sanitizer runtimes (on non-Fuchsia hosts) intercept dlopen calls with
  // RTLD_DEEPBIND and make them fail without really calling -ldl's dlopen to
  // see if it would fail anyway.  So avoid having that flag set in the bad
  // mode argument.
#ifdef RTLD_DEEPBIND
  bad_mode &= ~RTLD_DEEPBIND;
#endif

  auto result = this->DlOpen("libld-dep-a.so", bad_mode);
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value().take_str(), "invalid mode parameter")
      << "for mode argument " << bad_mode;
}

TYPED_TEST(DlTests, Basic) {
  // TODO(https://fxbug.dev/323418587): For now, just check that we got a
  // non-null pointer back from dlopen. A subsequent CL will check that the
  // module was loaded correctly with dlsym().
  auto result = this->DlOpen("libld-dep-c.so", RTLD_NOW | RTLD_LOCAL);
  ASSERT_TRUE(result.is_ok()) << result.error_value();
  EXPECT_TRUE(result.value());
}

}  // namespace
