// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/diagnostics/reader/cpp/archive_reader.h>

#include <zxtest/zxtest.h>

namespace {

TEST(ArchiveReaderTest, SanitizeMoniker) {
  auto result = diagnostics::reader::SanitizeMonikerForSelectors("core/coll:bar/baz/other:foo");
  EXPECT_EQ(result, "core/coll\\:bar/baz/other\\:foo");
}

}  // namespace
