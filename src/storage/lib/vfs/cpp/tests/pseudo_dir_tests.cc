// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/pseudo_file.h"
#include "src/storage/lib/vfs/cpp/tests/dir_test_util.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace {

TEST(PseudoDir, ApiTest) {
  auto dir = fbl::MakeRefCounted<fs::PseudoDir>();
  auto subdir = fbl::MakeRefCounted<fs::PseudoDir>();
  auto file1 = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>();
  auto file2 = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>();

  // add entries
  EXPECT_EQ(ZX_OK, dir->AddEntry("subdir", subdir));
  EXPECT_EQ(ZX_OK, dir->AddEntry("file1", file1));
  EXPECT_EQ(ZX_OK, dir->AddEntry("file2", file2));
  EXPECT_EQ(ZX_OK, dir->AddEntry("file2b", file2));

  // try to add duplicates
  EXPECT_EQ(ZX_ERR_ALREADY_EXISTS, dir->AddEntry("subdir", subdir));
  EXPECT_EQ(ZX_ERR_ALREADY_EXISTS, dir->AddEntry("file1", subdir));

  // remove entries
  EXPECT_EQ(ZX_OK, dir->RemoveEntry("file2"));
  EXPECT_EQ(ZX_ERR_NOT_FOUND, dir->RemoveEntry("file2"));

  // open as directory
  fs::VnodeConnectionOptions options_directory{.flags = fuchsia_io::OpenFlags::kDirectory};
  fbl::RefPtr<fs::Vnode> redirect;
  auto validated_options = dir->ValidateOptions(options_directory);
  EXPECT_TRUE(validated_options.is_ok());
  EXPECT_EQ(ZX_OK, dir->Open(&redirect));
  EXPECT_NULL(redirect);

  // verify node protocol type
  EXPECT_EQ(dir->GetProtocols(), fuchsia_io::NodeProtocolKinds::kDirectory);

  // verify node attributes
  zx::result attr = dir->GetAttributes();
  ASSERT_TRUE(attr.is_ok());
  constexpr fs::VnodeAttributes kExpectedAttrs{.content_size = 0, .storage_size = 0};
  EXPECT_EQ(kExpectedAttrs, *attr);

  // lookup entries
  fbl::RefPtr<fs::Vnode> node;
  EXPECT_EQ(ZX_OK, dir->Lookup("subdir", &node));
  EXPECT_EQ(subdir.get(), node.get());
  EXPECT_EQ(ZX_OK, dir->Lookup("file1", &node));
  EXPECT_EQ(file1.get(), node.get());
  EXPECT_EQ(ZX_ERR_NOT_FOUND, dir->Lookup("file2", &node));
  EXPECT_EQ(ZX_OK, dir->Lookup("file2b", &node));
  EXPECT_EQ(file2.get(), node.get());

  // readdir
  {
    fs::VdirCookie cookie;
    uint8_t buffer[4096];
    size_t length;
    EXPECT_EQ(dir->Readdir(&cookie, buffer, sizeof(buffer), &length), ZX_OK);
    fs::DirentChecker dc(buffer, length);
    dc.ExpectEntry(".", V_TYPE_DIR);
    dc.ExpectEntry("subdir", V_TYPE_DIR);
    dc.ExpectEntry("file1", V_TYPE_FILE);
    dc.ExpectEntry("file2b", V_TYPE_FILE);
    dc.ExpectEnd();
  }

  // readdir with small buffer
  {
    fs::VdirCookie cookie;
    uint8_t buffer[2 * sizeof(vdirent) + 13];
    size_t length;
    EXPECT_EQ(dir->Readdir(&cookie, buffer, sizeof(buffer), &length), ZX_OK);
    fs::DirentChecker dc(buffer, length);
    dc.ExpectEntry(".", V_TYPE_DIR);
    dc.ExpectEntry("subdir", V_TYPE_DIR);
    dc.ExpectEnd();

    // readdir again
    EXPECT_EQ(dir->Readdir(&cookie, buffer, sizeof(buffer), &length), ZX_OK);
    fs::DirentChecker dc1(buffer, length);
    dc1.ExpectEntry("file1", V_TYPE_FILE);
    dc1.ExpectEntry("file2b", V_TYPE_FILE);
    dc1.ExpectEnd();
  }

  // test removed entries do not appear in readdir or lookup
  dir->RemoveEntry("file1");
  {
    fs::VdirCookie cookie;
    uint8_t buffer[4096];
    size_t length;
    EXPECT_EQ(dir->Readdir(&cookie, buffer, sizeof(buffer), &length), ZX_OK);
    fs::DirentChecker dc(buffer, length);
    dc.ExpectEntry(".", V_TYPE_DIR);
    dc.ExpectEntry("subdir", V_TYPE_DIR);
    dc.ExpectEntry("file2b", V_TYPE_FILE);
    dc.ExpectEnd();
  }
  EXPECT_EQ(ZX_ERR_NOT_FOUND, dir->Lookup("file1", &node));

  // remove all entries
  dir->RemoveAllEntries();

  // readdir again
  {
    fs::VdirCookie cookie;
    uint8_t buffer[4096];
    size_t length;
    EXPECT_EQ(dir->Readdir(&cookie, buffer, sizeof(buffer), &length), ZX_OK);
    fs::DirentChecker dc(buffer, length);
    dc.ExpectEntry(".", V_TYPE_DIR);
    dc.ExpectEnd();
  }

  // FIXME(https://fxbug.dev/42106069): Can't unittest watch/notify (hard to isolate right now).
}

TEST(PseudoDir, RejectOpenFlagNotDirectory) {
  auto dir = fbl::MakeRefCounted<fs::PseudoDir>();
  auto result = dir->ValidateOptions(fs::VnodeConnectionOptions{
      .flags = fuchsia_io::OpenFlags::kNotDirectory, .rights = fuchsia_io::kRStarDir});
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(ZX_ERR_NOT_FILE, result.status_value());
}

}  // namespace
