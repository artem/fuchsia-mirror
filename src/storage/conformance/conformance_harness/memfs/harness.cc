// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io.test/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <sys/stat.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <atomic>
#include <cstdlib>
#include <memory>

#include <fbl/ref_ptr.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"
#include "src/storage/memfs/memfs.h"
#include "src/storage/memfs/vnode_dir.h"
#include "src/storage/memfs/vnode_file.h"

namespace fio = fuchsia_io;
namespace fio_test = fuchsia_io_test;

void AddEntry(const fio_test::DirectoryEntry& entry, memfs::VnodeDir& dir) {
  switch (entry.Which()) {
    case fio_test::DirectoryEntry::Tag::kDirectory: {
      zx::result node = dir.Create(entry.directory()->name(), fs::CreationType::kDirectory);
      ZX_ASSERT_MSG(node.is_ok(), "Failed to create directory: %s", node.status_string());
      auto sub_dir = fbl::RefPtr<memfs::VnodeDir>::Downcast(*std::move(node));
      for (const auto& entry : entry.directory()->entries()) {
        AddEntry(*entry, *sub_dir);
      }

      break;
    }
    case fio_test::DirectoryEntry::Tag::kFile: {
      zx::result node = dir.Create(entry.file()->name(), fs::CreationType::kFile);
      ZX_ASSERT_MSG(node.is_ok(), "Failed to create file: %s", node.status_string());
      auto file = fbl::RefPtr<memfs::VnodeFile>::Downcast(*std::move(node));
      const auto& contents = entry.file()->contents();
      if (!contents.empty()) {
        zx::result<zx::stream> stream = file->CreateStream(ZX_STREAM_MODE_WRITE);
        ZX_ASSERT(stream.is_ok());
        size_t actual;
        zx_iovec_t iovec = {
            .buffer = const_cast<uint8_t*>(contents.data()),
            .capacity = contents.size(),
        };
        ZX_ASSERT(stream->writev(0, &iovec, 1, &actual) == ZX_OK);
        ZX_ASSERT(actual == contents.size());
      }
      break;
    }
    case fio_test::DirectoryEntry::Tag::kRemoteDirectory:
      ZX_PANIC("Remote directories are not supported");
      break;
    case fio_test::DirectoryEntry::Tag::kExecutableFile:
      ZX_PANIC("Executable files are not supported");
      break;
  }
}

class TestHarness : public fidl::Server<fio_test::Io1Harness> {
 public:
  explicit TestHarness(std::unique_ptr<memfs::Memfs> memfs, fbl::RefPtr<memfs::VnodeDir> root)
      : memfs_(std::move(memfs)), root_(std::move(root)) {}

  void GetConfig(GetConfigCompleter::Sync& completer) final {
    fio_test::Io1Config config;

    // Supported options
    config.supports_open2(true);
    config.supports_get_backing_memory(true);
    config.supports_get_token(true);
    config.supports_create(true);
    config.supports_rename(true);
    config.supports_link(true);
    config.supports_unlink(true);
    config.supports_directory_watchers(true);
    config.supports_append(true);
    config.supported_attributes(
        fio::NodeAttributesQuery::kCreationTime | fio::NodeAttributesQuery::kModificationTime |
        fio::NodeAttributesQuery::kContentSize | fio::NodeAttributesQuery::kStorageSize |
        fio::NodeAttributesQuery::kId | fio::NodeAttributesQuery::kLinkCount |
        fio::NodeAttributesQuery::kMode | fio::NodeAttributesQuery::kUid |
        fio::NodeAttributesQuery::kGid);

    completer.Reply(config);
  }

  void GetDirectory(GetDirectoryRequest& request, GetDirectoryCompleter::Sync& completer) final {
    uint64_t test_id = test_counter_.fetch_add(1);
    std::string directory_name = fxl::StringPrintf("test.%ld", test_id);

    zx::result test_root = root_->Create(directory_name, fs::CreationType::kDirectory);
    ZX_ASSERT_MSG(test_root.is_ok(), "Failed to create test root: %s", test_root.status_string());
    auto root_dir = fbl::RefPtr<memfs::VnodeDir>::Downcast(*std::move(test_root));

    for (auto& entry : request.root().entries()) {
      AddEntry(*entry, *root_dir);
    }

    zx::result options = fs::VnodeConnectionOptions::FromOpen1Flags(request.flags());
    ZX_ASSERT_MSG(options.is_ok(), "Failed to validate flags: %s", options.status_string());
    zx_status_t status =
        memfs_->Serve(root_dir, request.directory_request().TakeChannel(), *options);
    ZX_ASSERT_MSG(status == ZX_OK, "Failed to serve directory: %s", zx_status_get_string(status));
  }

 private:
  std::unique_ptr<memfs::Memfs> memfs_;
  fbl::RefPtr<memfs::VnodeDir> root_;
  std::atomic_uint64_t test_counter_ = 0;
};

int main(int argc, const char** argv) {
  fuchsia_logging::SetTags({"io_conformance_harness_memfs"});

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(dispatcher);

  zx::result result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << result.status_string();
    return EXIT_FAILURE;
  }

  auto memfs = memfs::Memfs::Create(dispatcher, "memfs");
  if (memfs.is_error()) {
    FX_LOGS(ERROR) << "Failed to create memfs: " << memfs.status_string();
  }

  result = outgoing.AddProtocol<fio_test::Io1Harness>(
      std::make_unique<TestHarness>(std::move(memfs->first), std::move(memfs->second)));
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to server test harness: " << result.status_string();
    return EXIT_FAILURE;
  }

  loop.Run();
  return EXIT_SUCCESS;
}
