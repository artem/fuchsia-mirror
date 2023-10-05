// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/validate_eventpair.h"

#include <lib/zx/eventpair.h>

#include <gtest/gtest.h>

namespace utils::test {

using fuchsia::ui::views::ViewRef;
using fuchsia::ui::views::ViewRefControl;

TEST(ValidateEventpair, CorrectEventpair) {
  zx::eventpair a, b;
  zx_status_t status = zx::eventpair::create(/*flags*/ 0u, &a, &b);
  ASSERT_EQ(status, ZX_OK);

  EXPECT_TRUE(validate_eventpair(a, ZX_DEFAULT_EVENTPAIR_RIGHTS, b, ZX_DEFAULT_EVENTPAIR_RIGHTS));
}

TEST(ValidateEventpair, SameEventpair) {
  zx::eventpair a, b;
  zx_status_t status = zx::eventpair::create(/*flags*/ 0u, &a, &b);
  ASSERT_EQ(status, ZX_OK);

  EXPECT_FALSE(
      validate_eventpair(a, ZX_DEFAULT_EVENTPAIR_RIGHTS, /*oops!*/ a, ZX_DEFAULT_EVENTPAIR_RIGHTS));
}

TEST(ValidateEventpair, DeadEventpair) {
  zx::eventpair a, b;
  zx_status_t status = zx::eventpair::create(/*flags*/ 0u, &a, &b);
  ASSERT_EQ(status, ZX_OK);

  b.reset();  // Kill b.

  EXPECT_FALSE(validate_eventpair(a, ZX_DEFAULT_EVENTPAIR_RIGHTS, b, ZX_DEFAULT_EVENTPAIR_RIGHTS));
}

TEST(ValidateEventpair, UncorrelatedEventpair) {
  zx::eventpair a, b, c, d;
  zx_status_t status = zx::eventpair::create(/*flags*/ 0u, &a, &b);
  ASSERT_EQ(status, ZX_OK);
  status = zx::eventpair::create(/*flags*/ 0u, &c, &d);
  ASSERT_EQ(status, ZX_OK);

  EXPECT_FALSE(
      validate_eventpair(a, ZX_DEFAULT_EVENTPAIR_RIGHTS, /*oop!*/ c, ZX_DEFAULT_EVENTPAIR_RIGHTS));
}

TEST(ValidateEventpair, MissingCapability) {
  zx::eventpair a, b;
  zx_status_t status = zx::eventpair::create(/*flags*/ 0u, &a, &b);
  ASSERT_EQ(status, ZX_OK);

  status = a.replace(ZX_RIGHTS_BASIC, &a);  // Fewer rights.
  ASSERT_EQ(status, ZX_OK);

  EXPECT_FALSE(validate_eventpair(a, ZX_DEFAULT_EVENTPAIR_RIGHTS, b, ZX_DEFAULT_EVENTPAIR_RIGHTS));
}

TEST(ValidateEventpair, ExcessCapability) {
  zx::eventpair a, b;
  zx_status_t status = zx::eventpair::create(/*flags*/ 0u, &a, &b);
  ASSERT_EQ(status, ZX_OK);

  // a has more rights than expected.
  EXPECT_FALSE(validate_eventpair(a, ZX_RIGHTS_BASIC, b, ZX_DEFAULT_EVENTPAIR_RIGHTS));
}

TEST(ValidateViewRefs, CorrectViewRefLoose) {
  ViewRefControl control_ref;
  ViewRef view_ref;
  zx_status_t status = zx::eventpair::create(
      /*flags*/ 0u, &control_ref.reference, &view_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  status = view_ref.reference.replace(ZX_RIGHTS_BASIC, &view_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  EXPECT_TRUE(validate_viewref(control_ref, view_ref));
}

TEST(ValidateViewRefs, CorrectViewRefTight) {
  ViewRefControl control_ref;
  ViewRef view_ref;
  zx_status_t status = zx::eventpair::create(
      /*flags*/ 0u, &control_ref.reference, &view_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  status = control_ref.reference.replace(ZX_DEFAULT_EVENTPAIR_RIGHTS & (~ZX_RIGHT_DUPLICATE),
                                         &control_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  status = view_ref.reference.replace(ZX_RIGHTS_BASIC, &view_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  EXPECT_TRUE(validate_viewref(control_ref, view_ref));
}

TEST(ValidateViewRefs, DeadViewRef) {
  ViewRefControl control_ref;
  ViewRef view_ref;
  zx_status_t status = zx::eventpair::create(
      /*flags*/ 0u, &control_ref.reference, &view_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  view_ref.reference.reset();  // Kill view_ref.

  EXPECT_FALSE(validate_viewref(control_ref, view_ref));
}

TEST(ValidateViewRefs, UncorrelatedViewRefs) {
  ViewRefControl ctrl_a, ctrl_b;
  ViewRef view_a, view_b;
  zx_status_t status = zx::eventpair::create(
      /*flags*/ 0u, &ctrl_a.reference, &view_a.reference);
  ASSERT_EQ(status, ZX_OK);
  status = zx::eventpair::create(
      /*flags*/ 0u, &ctrl_b.reference, &view_b.reference);
  ASSERT_EQ(status, ZX_OK);

  EXPECT_FALSE(validate_viewref(ctrl_a, view_b));
}

TEST(ValidateViewRefs, ControlRefMissingCapability) {
  ViewRefControl control_ref;
  ViewRef view_ref;
  zx_status_t status = zx::eventpair::create(
      /*flags*/ 0u, &control_ref.reference, &view_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  // Expected reduction of rights.
  view_ref.reference.replace(ZX_RIGHTS_BASIC, &view_ref.reference);

  // Unexpected reduction of rights.
  control_ref.reference.replace(ZX_RIGHTS_BASIC, &control_ref.reference);

  EXPECT_FALSE(validate_viewref(control_ref, view_ref));
}

TEST(ValidateViewRefs, ViewRefExcessCapability) {
  ViewRefControl control_ref;
  ViewRef view_ref;
  zx_status_t status = zx::eventpair::create(
      /*flags*/ 0u, &control_ref.reference, &view_ref.reference);
  ASSERT_EQ(status, ZX_OK);

  // No reduction of rights for view_ref.

  EXPECT_FALSE(validate_viewref(control_ref, view_ref));
}

}  // namespace utils::test
