// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.gpu.magma/cpp/test_base.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.sys2/cpp/wire.h>
#include <fidl/fuchsia.vulkan.loader/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <filesystem>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

// This is the first and only ICD loaded, so it should have a "0-" prepended.
const char* kIcdFilename = "0-libvulkan_fake.so";

class VulkanLoader : public testing::Test {
 protected:
  void SetUp() override {
    loop_.StartThread();
    auto builder = component_testing::RealmBuilder::CreateFromRelativeUrl("#meta/test_realm.cm");
    realm_ = builder.Build(loop_.dispatcher());
    auto client_end = realm().component().Connect<fuchsia_vulkan_loader::Loader>();
    ASSERT_TRUE(client_end.is_ok()) << client_end.status_string();
    loader_ = fidl::WireSyncClient(std::move(*client_end));
  }

  void TearDown() override { loop_.Shutdown(); }

  component_testing::RealmRoot& realm() { return *realm_; }

  const auto& loader() const { return *loader_; }

  zx::result<zx::vmo> GetIcd(std::string_view icd_filename) {
    auto response = loader()->Get(fidl::StringView::FromExternal(icd_filename));
    if (!response.ok()) {
      return zx::error(response.status());
    }
    return zx::ok(std::move(response->lib));
  }

 private:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  std::optional<component_testing::RealmRoot> realm_;
  std::optional<fidl::WireSyncClient<fuchsia_vulkan_loader::Loader>> loader_;
};

TEST_F(VulkanLoader, ManifestLoad) {
  // manifest.json remaps this to bin/pkg-server.
  zx::result icd = GetIcd(kIcdFilename);
  ASSERT_TRUE(icd.is_ok()) << icd.status_string();
  ASSERT_TRUE(icd->is_valid());
  zx_info_handle_basic_t handle_info;
  EXPECT_EQ(ZX_OK, icd->get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info), nullptr,
                                 nullptr));
  EXPECT_TRUE(handle_info.rights & ZX_RIGHT_EXECUTE);
  EXPECT_FALSE(handle_info.rights & ZX_RIGHT_WRITE);

  zx::result not_present = GetIcd("-present");
  ASSERT_TRUE(not_present.is_ok()) << not_present.status_string();
  EXPECT_FALSE(not_present->is_valid());
}

// Check that writes to one VMO returned by the server will not modify a separate VMO returned by
// the service.
TEST_F(VulkanLoader, VmosIndependent) {
  // manifest.json remaps this to bin/pkg-server.
  zx::result icd = GetIcd(kIcdFilename);
  ASSERT_TRUE(icd.is_ok()) << icd.status_string();
  ASSERT_TRUE(icd->is_valid());

  fzl::VmoMapper mapper;
  EXPECT_EQ(ZX_OK,
            mapper.Map(*icd, 0, 0, ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ | ZX_VM_ALLOW_FAULTS));
  uint8_t original_value = *static_cast<uint8_t*>(mapper.start());
  uint8_t byte_to_write = original_value + 1;
  size_t actual;
  // zx_process_write_memory can write to memory mapped without ZX_VM_PERM_WRITE. If that ever
  // changes, this test can probably be removed.
  zx_status_t status = zx::process::self()->write_memory(
      reinterpret_cast<uint64_t>(mapper.start()), &byte_to_write, sizeof(byte_to_write), &actual);

  // zx_process_write_memory may be disabled using a kernel command-line flag.
  if (status == ZX_ERR_NOT_SUPPORTED) {
    EXPECT_EQ(original_value, *static_cast<uint8_t*>(mapper.start()));
  } else {
    EXPECT_EQ(ZX_OK, status);

    EXPECT_EQ(byte_to_write, *static_cast<uint8_t*>(mapper.start()));
  }

  // Ensure that the new clone is unaffected.
  zx::result icd2 = GetIcd(kIcdFilename);
  ASSERT_TRUE(icd2.is_ok()) << icd2.status_string();
  ASSERT_TRUE(icd2->is_valid());

  fzl::VmoMapper mapper2;
  EXPECT_EQ(ZX_OK,
            mapper2.Map(*icd2, 0, 0, ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ | ZX_VM_ALLOW_FAULTS));
  EXPECT_EQ(original_value, *static_cast<uint8_t*>(mapper2.start()));
}

TEST_F(VulkanLoader, DeviceFs) {
  auto dev_fs = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_TRUE(dev_fs.is_ok()) << dev_fs.status_string();
  {
    auto response = loader()->ConnectToDeviceFs(dev_fs->server.TakeChannel());
    ASSERT_TRUE(response.ok()) << response;
  }
  ASSERT_TRUE(GetIcd(kIcdFilename).is_ok());  // Wait for idle.

  auto device = fidl::CreateEndpoints<fuchsia_gpu_magma::Device>();
  ASSERT_TRUE(device.is_ok()) << device.status_string();
  zx_status_t status = fdio_service_connect_at(dev_fs->client.channel().get(), "class/gpu/000",
                                               device->server.TakeChannel().release());
  ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);

  auto response =
      fidl::WireCall(device->client)->Query(fuchsia_gpu_magma::wire::QueryId::kVendorId);
  ASSERT_TRUE(response.ok()) << response;
  ASSERT_TRUE(response->is_ok()) << zx_status_get_string(response->error_value());
  ASSERT_TRUE((*response)->is_simple_result());
  EXPECT_EQ((*response)->simple_result(), 5u);
}

TEST_F(VulkanLoader, Features) {
  auto response = loader()->GetSupportedFeatures();
  ASSERT_TRUE(response.ok()) << response;
  constexpr fuchsia_vulkan_loader::Features kExpectedFeatures =
      fuchsia_vulkan_loader::Features::kConnectToDeviceFs |
      fuchsia_vulkan_loader::Features::kConnectToManifestFs | fuchsia_vulkan_loader::Features::kGet;
  EXPECT_EQ(response->features, kExpectedFeatures);
}

TEST_F(VulkanLoader, ManifestFs) {
  auto manifest_fs = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_TRUE(manifest_fs.is_ok()) << manifest_fs.status_string();
  {
    auto response =
        loader()->ConnectToManifestFs(fuchsia_vulkan_loader::ConnectToManifestOptions::kWaitForIdle,
                                      manifest_fs->server.TakeChannel());
    ASSERT_TRUE(response.ok()) << response;
  }

  fbl::unique_fd dir_fd;
  zx_status_t status =
      fdio_fd_create(manifest_fs->client.TakeChannel().release(), dir_fd.reset_and_get_address());
  ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);

  fbl::unique_fd manifest_fd(
      openat(dir_fd.get(), (std::string(kIcdFilename) + ".json").c_str(), O_RDONLY));
  ASSERT_TRUE(manifest_fd.is_valid()) << strerror(errno);

  constexpr int kManifestFileSize = 135;
  char manifest_data[kManifestFileSize + 1];
  ssize_t read_size = read(manifest_fd.get(), manifest_data, sizeof(manifest_data) - 1);
  EXPECT_EQ(kManifestFileSize, read_size);
}

TEST_F(VulkanLoader, GoldfishSyncDeviceFs) {
  auto dev_fs = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_TRUE(dev_fs.is_ok()) << dev_fs.status_string();
  {
    auto response = loader()->ConnectToDeviceFs(dev_fs->server.TakeChannel());
    ASSERT_TRUE(response.ok()) << response;
  }
  ASSERT_TRUE(GetIcd(kIcdFilename).is_ok());  // Wait for idle.

  const char* kDeviceClassList[] = {
      "class/goldfish-sync",
      "class/goldfish-pipe",
      "class/goldfish-address-space",
  };

  for (auto& device_class : kDeviceClassList) {
    fuchsia::io::NodeSyncPtr device_ptr;
    EXPECT_EQ(ZX_OK, fdio_service_connect_at(dev_fs->client.channel().get(), device_class,
                                             device_ptr.NewRequest().TakeChannel().release()));

    // Check that the directory is connected to something.
    std::vector<uint8_t> protocol;
    zx_status_t status = device_ptr->Query(&protocol);
    EXPECT_EQ(status, ZX_OK) << "class=" << device_class
                             << " status=" << zx_status_get_string(status);
  }
}

TEST_F(VulkanLoader, DebugFilesystems) {
  ASSERT_TRUE(GetIcd(kIcdFilename).is_ok());  // Wait for idle.

  auto client_end = realm().component().Connect<fuchsia_sys2::RealmQuery>();
  ASSERT_TRUE(client_end.is_ok()) << client_end.status_string();

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Node>();
  ASSERT_TRUE(endpoints.is_ok()) << endpoints.status_string();

  auto response = fidl::WireCall(*client_end)
                      ->Open("./vulkan_loader", fuchsia_sys2::OpenDirType::kOutgoingDir,
                             fuchsia_io::OpenFlags::kRightReadable, /*mode=*/{}, /*path=*/".",
                             std::move(endpoints->server));
  ASSERT_TRUE(response.ok()) << response;
  ASSERT_TRUE(response->is_ok()) << static_cast<uint32_t>(response->error_value());

  fdio_ns_t* ns;
  EXPECT_EQ(ZX_OK, fdio_ns_get_installed(&ns));
  EXPECT_EQ(ZX_OK, fdio_ns_bind(ns, "/loader_out", endpoints->client.TakeChannel().release()));
  auto cleanup_binding = fit::defer([&]() { fdio_ns_unbind(ns, "/loader_out"); });

  const std::string debug_path("/loader_out/debug/");

  EXPECT_TRUE(std::filesystem::exists(debug_path + "device-fs/class/gpu/000"));
  EXPECT_TRUE(std::filesystem::exists(debug_path + "manifest-fs/" + kIcdFilename + ".json"));
}
