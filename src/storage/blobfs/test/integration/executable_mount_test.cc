// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/io.h>
#include <lib/zx/resource.h>
#include <unistd.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/storage/blobfs/blobfs.h"
#include "src/storage/blobfs/mount.h"
#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/blobfs/test/integration/fdio_test.h"

namespace blobfs {
namespace {

namespace fio = fuchsia_io;

zx_rights_t get_rights(const zx::object_base& handle) {
  zx_info_handle_basic_t info;
  zx_status_t status = handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.rights : ZX_RIGHT_NONE;
}

class ExecutableMountTest : public FdioTest {
 public:
  ExecutableMountTest() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_kernel::VmexResource>();
    ZX_ASSERT(endpoints.status_value() == ZX_OK);
    auto [local, remote] = *std::move(endpoints);

    zx_status_t status =
        fdio_service_connect("/svc/fuchsia.kernel.VmexResource", remote.TakeChannel().release());
    ZX_ASSERT_MSG(status == ZX_OK, "Failed to connect to fuchsia.kernel.VmexResource: %u", status);

    auto client = fidl::WireSyncClient<fuchsia_kernel::VmexResource>{std::move(local)};
    auto result = client->Get();
    ZX_ASSERT_MSG(result.ok(), "fuchsia.kernel.VmexResource.Get() failed: %u", result.status());

    set_vmex_resource(std::move(result.value().resource));
  }
};

// The test fixture for this test provides a valid Resource object to the filesystem when it is
// created, which means it should support fuchsia.io/File.GetBackingMemory with VmoFlags::EXECUTE
// which fdio_get_vmo_exec exercises.
TEST_F(ExecutableMountTest, CanLoadBlobsExecutable) {
  // Create a new blob with random contents on the mounted filesystem.
  std::unique_ptr<BlobInfo> info = GenerateRandomBlob(".", 1 << 16);

  fbl::unique_fd fd(openat(root_fd(), info->path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
  ASSERT_TRUE(fd.is_valid());

  ASSERT_EQ(ftruncate(fd.get(), info->size_data), 0);
  ASSERT_EQ(StreamAll(write, fd.get(), info->data.get(), info->size_data), 0)
      << "Failed to write Data";
  ASSERT_NO_FATAL_FAILURE(VerifyContents(fd.get(), info->data.get(), info->size_data));
  fd.reset();

  // Open the new blob again but with READABLE | EXECUTABLE rights, then confirm that we can get the
  // blob contents as a normal and executable VMO.
  // The +2 here is because ./ is not valid for fuchsia.io.
  ASSERT_EQ(memcmp(info->path, "./", 2), 0);
  ASSERT_EQ(fdio_open_fd_at(root_fd(), info->path + 2,
                            static_cast<uint32_t>(fio::wire::OpenFlags::kRightReadable |
                                                  fio::wire::OpenFlags::kRightExecutable),
                            fd.reset_and_get_address()),
            ZX_OK);
  ASSERT_TRUE(fd.is_valid());

  zx::vmo vmo;
  ASSERT_EQ(fdio_get_vmo_clone(fd.get(), vmo.reset_and_get_address()), ZX_OK);
  ASSERT_TRUE(vmo.is_valid());

  vmo.reset();
  ASSERT_EQ(fdio_get_vmo_exec(fd.get(), vmo.reset_and_get_address()), ZX_OK);
  ASSERT_TRUE(vmo.is_valid());
  zx_rights_t rights = get_rights(vmo);
  ASSERT_TRUE((rights & ZX_RIGHT_EXECUTE) == ZX_RIGHT_EXECUTE);
}

}  // namespace
}  // namespace blobfs
