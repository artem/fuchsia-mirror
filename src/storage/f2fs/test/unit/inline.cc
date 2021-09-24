// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <unordered_set>

#include <block-client/cpp/fake-device.h>
#include <gtest/gtest.h>

#include "src/storage/f2fs/f2fs.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

TEST(InlineDirTest, InlineDirCreation) {
  std::unique_ptr<Bcache> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable inline dir option
  ASSERT_EQ(options.SetValue(options.GetNameView(kOptInlineDentry), 1), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);

  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // Inline dir creation
  std::string inline_dir_name("inline");
  fbl::RefPtr<fs::Vnode> inline_child;
  ASSERT_EQ(root_dir->Create(inline_dir_name, S_IFDIR, &inline_child), ZX_OK);

  fbl::RefPtr<VnodeF2fs> inline_child_dir =
      fbl::RefPtr<VnodeF2fs>::Downcast(std::move(inline_child));

  FileTester::CheckInlineDir(inline_child_dir.get());

  ASSERT_EQ(inline_child_dir->Close(), ZX_OK);
  inline_child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  FileTester::Unmount(std::move(fs), &bc);

  // Disable inline dir option
  ASSERT_EQ(options.SetValue(options.GetNameView(kOptInlineDentry), 0), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // Check if existing inline dir is still inline regardless of mount option
  FileTester::Lookup(root_dir.get(), inline_dir_name, &inline_child);
  inline_child_dir = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(inline_child));
  FileTester::CheckInlineDir(inline_child_dir.get());

  // However, newly created dir should be non-inline
  std::string non_inline_dir_name("noninline");
  fbl::RefPtr<fs::Vnode> non_inline_child;
  ASSERT_EQ(root_dir->Create(non_inline_dir_name, S_IFDIR, &non_inline_child), ZX_OK);

  fbl::RefPtr<VnodeF2fs> non_inline_child_dir =
      fbl::RefPtr<VnodeF2fs>::Downcast(std::move(non_inline_child));
  FileTester::CheckNonInlineDir(non_inline_child_dir.get());

  ASSERT_EQ(inline_child_dir->Close(), ZX_OK);
  inline_child_dir = nullptr;
  ASSERT_EQ(non_inline_child_dir->Close(), ZX_OK);
  non_inline_child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(bc.get()), ZX_OK);
}

TEST(InlineDirTest, InlineDirConvert) {
  std::unique_ptr<Bcache> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable inline dir option
  ASSERT_EQ(options.SetValue(options.GetNameView(kOptInlineDentry), 1), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);

  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // Inline dir creation
  std::string inline_dir_name("inline");
  fbl::RefPtr<fs::Vnode> inline_child;
  ASSERT_EQ(root_dir->Create(inline_dir_name, S_IFDIR, &inline_child), ZX_OK);

  fbl::RefPtr<Dir> inline_child_dir = fbl::RefPtr<Dir>::Downcast(std::move(inline_child));

  unsigned int child_count = 0;

  // Fill all slots of inline dentry
  // Since two dentry slots are already allocated for "." and "..", decrease 2 from kNrInlineDentry
  for (; child_count < inline_child_dir->MaxInlineDentry() - 2; ++child_count) {
    uint32_t mode = child_count % 2 == 0 ? S_IFDIR : S_IFREG;
    FileTester::CreateChild(inline_child_dir.get(), mode, std::to_string(child_count));
  }

  // It should be inline
  FileTester::CheckInlineDir(inline_child_dir.get());

  ASSERT_EQ(inline_child_dir->Close(), ZX_OK);
  inline_child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  FileTester::Unmount(std::move(fs), &bc);

  // Disable inline dir option
  ASSERT_EQ(options.SetValue(options.GetNameView(kOptInlineDentry), 0), ZX_OK);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // Check if existing inline dir is still inline regardless of mount option
  FileTester::Lookup(root_dir.get(), inline_dir_name, &inline_child);
  inline_child_dir = fbl::RefPtr<Dir>::Downcast(std::move(inline_child));
  FileTester::CheckInlineDir(inline_child_dir.get());

  // If one more dentry is added, it should be converted to non-inline dir
  uint32_t mode = child_count % 2 == 0 ? S_IFDIR : S_IFREG;
  FileTester::CreateChild(inline_child_dir.get(), mode, std::to_string(child_count));

  FileTester::CheckNonInlineDir(inline_child_dir.get());

  ASSERT_EQ(inline_child_dir->Close(), ZX_OK);
  inline_child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(bc.get()), ZX_OK);
}

TEST(InlineDirTest, InlineDentryOps) {
  std::unique_ptr<Bcache> bc;
  FileTester::MkfsOnFakeDev(&bc);

  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  // Enable inline dir option
  ASSERT_EQ(options.SetValue(options.GetNameView(kOptInlineDentry), 1), ZX_OK);
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);

  fbl::RefPtr<Dir> root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  // Inline dir creation
  std::string inline_dir_name("inline");
  fbl::RefPtr<fs::Vnode> inline_child;
  ASSERT_EQ(root_dir->Create(inline_dir_name, S_IFDIR, &inline_child), ZX_OK);

  fbl::RefPtr<Dir> inline_child_dir = fbl::RefPtr<Dir>::Downcast(std::move(inline_child));

  std::unordered_set<std::string> child_set = {"a", "b", "c", "d", "e"};

  Dir *dir_ptr = inline_child_dir.get();

  for (auto iter : child_set) {
    FileTester::CreateChild(dir_ptr, S_IFDIR, iter);
  }
  FileTester::CheckChildrenFromReaddir(dir_ptr, child_set);

  // remove "b" and "d"
  ASSERT_EQ(dir_ptr->Unlink("b", true), ZX_OK);
  child_set.erase("b");
  ASSERT_EQ(dir_ptr->Unlink("d", true), ZX_OK);
  child_set.erase("d");
  FileTester::CheckChildrenFromReaddir(dir_ptr, child_set);

  // create "f" and "g"
  FileTester::CreateChild(dir_ptr, S_IFDIR, "f");
  child_set.insert("f");
  FileTester::CreateChild(dir_ptr, S_IFDIR, "g");
  child_set.insert("g");
  FileTester::CheckChildrenFromReaddir(dir_ptr, child_set);

  // rename "g" to "h"
  ASSERT_EQ(dir_ptr->Rename(inline_child_dir, "g", "h", true, true), ZX_OK);
  child_set.erase("g");
  child_set.insert("h");
  FileTester::CheckChildrenFromReaddir(dir_ptr, child_set);

  // fill all inline dentry slots
  auto child_count = child_set.size();
  for (; child_count < inline_child_dir->MaxInlineDentry() - 2; ++child_count) {
    FileTester::CreateChild(dir_ptr, S_IFDIR, std::to_string(child_count));
    child_set.insert(std::to_string(child_count));
  }
  FileTester::CheckChildrenFromReaddir(dir_ptr, child_set);

  // It should be inline
  FileTester::CheckInlineDir(dir_ptr);

  // one more entry
  FileTester::CreateChild(dir_ptr, S_IFDIR, std::to_string(child_count));
  child_set.insert(std::to_string(child_count));
  FileTester::CheckChildrenFromReaddir(dir_ptr, child_set);

  // It should be non inline
  FileTester::CheckNonInlineDir(dir_ptr);

  ASSERT_EQ(inline_child_dir->Close(), ZX_OK);
  inline_child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;
  FileTester::Unmount(std::move(fs), &bc);

  // Check dentry after remount
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  FileTester::Lookup(root_dir.get(), inline_dir_name, &inline_child);
  inline_child_dir = fbl::RefPtr<Dir>::Downcast(std::move(inline_child));
  dir_ptr = inline_child_dir.get();

  FileTester::CheckNonInlineDir(dir_ptr);
  FileTester::CheckChildrenFromReaddir(dir_ptr, child_set);

  ASSERT_EQ(inline_child_dir->Close(), ZX_OK);
  inline_child_dir = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;

  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(bc.get()), ZX_OK);
}

}  // namespace
}  // namespace f2fs
