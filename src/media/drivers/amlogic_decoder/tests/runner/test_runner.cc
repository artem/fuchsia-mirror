// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.mediacodec/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/watcher.h>
#include <lib/zx/channel.h>
#include <zircon/errors.h>

#include <iostream>

#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

#include "lib/stdcompat/string_view.h"

namespace amlogic_decoder {
namespace test {

// TODO(fxbug.dev/104928) Remove the calls to fuchsia.device/Controller once upgraded to DFv2
class TestDeviceBase {
 public:
  static zx::result<TestDeviceBase> CreateFromFileName(std::string_view path) {
    auto controller_path = std::string(path) + "/device_controller";
    zx::result client_end = component::Connect<fuchsia_device::Controller>(controller_path.c_str());
    if (client_end.is_error()) {
      return client_end.take_error();
    }
    return zx::ok(TestDeviceBase(std::move(client_end.value())));
  }

  static zx::result<TestDeviceBase> CreateFromTopologicalPathSuffix(const fbl::unique_fd& fd,
                                                                    std::string_view suffix) {
    std::optional<TestDeviceBase> out;
    std::pair<std::reference_wrapper<decltype(out)>, std::string_view> pair{out, suffix};
    zx_status_t status = fdio_watch_directory(
        fd.get(),
        [](int dirfd, int event, const char* fn, void* cookie) {
          if (std::string_view{fn} == ".") {
            return ZX_OK;
          }

          auto& [out, suffix] = *static_cast<std::add_pointer<decltype(pair)>::type>(cookie);

          fdio_cpp::UnownedFdioCaller caller(dirfd);
          std::string controller_path = std::string(fn).append("/device_controller");
          zx::result controller_client_end = component::ConnectAt<fuchsia_device::Controller>(
              caller.directory(), controller_path.c_str());
          if (controller_client_end.is_error()) {
            return controller_client_end.error_value();
          }
          const fidl::WireResult result =
              fidl::WireCall(controller_client_end.value())->GetTopologicalPath();
          if (!result.ok()) {
            return result.status();
          }
          const fit::result response = result.value();
          if (response.is_error()) {
            return response.error_value();
          }
          std::string_view path = (*response.value()).path.get();
          if (!cpp20::ends_with(path, suffix)) {
            return ZX_OK;
          }

          zx::result device_client_end =
              component::ConnectAt<fuchsia_hardware_mediacodec::Tester>(caller.directory(), fn);
          if (device_client_end.is_error()) {
            return device_client_end.error_value();
          }

          out.get() = TestDeviceBase(std::move(controller_client_end.value()),
                                     std::move(device_client_end.value()));
          return ZX_ERR_STOP;
        },
        zx::time::infinite().get(), &pair);
    if (status == ZX_OK) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    if (status != ZX_ERR_STOP) {
      return zx::error(status);
    }
    if (!out.has_value()) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    return zx::ok(std::move(out.value()));
  }

  // Get a channel to the parent device, so we can rebind the driver to it. This
  // can require sandbox access to /dev/sys.
  zx::result<TestDeviceBase> GetParentDevice() const {
    const fidl::WireResult result = fidl::WireCall(controller_client_end_)->GetTopologicalPath();
    if (!result.ok()) {
      return zx::error(result.status());
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return zx::error(response.error_value());
    }
    std::string_view path = (*response.value()).path.get();
    std::cout << "device path: " << path << std::endl;
    // Remove everything after the final slash.
    if (const size_t index = path.rfind('/'); index != std::string::npos) {
      path = path.substr(0, index);
    }
    std::cout << "parent device path: " << path << std::endl;
    return CreateFromFileName(path);
  }

  zx::result<> UnbindChildren() const {
    const fidl::WireResult result = fidl::WireCall(controller_client_end_)->UnbindChildren();
    if (!result.ok()) {
      return zx::error(result.status());
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return zx::error(response.error_value());
    }
    return zx::ok();
  }

  zx::result<> RebindDriver(std::string_view path) const {
    const fidl::WireResult result =
        fidl::WireCall(controller_client_end_)->Rebind(fidl::StringView::FromExternal(path));
    if (!result.ok()) {
      return zx::error(result.status());
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return zx::error(response.error_value());
    }
    return zx::ok();
  }

  zx::result<> Unbind() const {
    const fidl::WireResult result = fidl::WireCall(controller_client_end_)->ScheduleUnbind();
    if (!result.ok()) {
      return zx::error(result.status());
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return zx::error(response.error_value());
    }
    return zx::ok();
  }

  fidl::UnownedClientEnd<fuchsia_hardware_mediacodec::Tester> tester() const {
    return device_client_end_.borrow();
  }

 private:
  explicit TestDeviceBase(fidl::ClientEnd<fuchsia_device::Controller> controller_client_end)
      : controller_client_end_(std::move(controller_client_end)) {}

  TestDeviceBase(fidl::ClientEnd<fuchsia_device::Controller> controller_client_end,
                 fidl::ClientEnd<fuchsia_hardware_mediacodec::Tester> device_client_end)
      : controller_client_end_(std::move(controller_client_end)),
        device_client_end_(std::move(device_client_end)) {}

  fidl::ClientEnd<fuchsia_device::Controller> controller_client_end_;
  fidl::ClientEnd<fuchsia_hardware_mediacodec::Tester> device_client_end_;
};

constexpr char kMediaCodecPath[] = "/dev/class/media-codec";
constexpr std::string_view kTopologicalPathSuffix = "/aml-video/amlogic_video";
constexpr std::string_view kTestTopologicalPathSuffix = "/aml-video/test_amlogic_video";

// Requires the driver to be in the system image, so disabled by default.
TEST(TestRunner, DISABLED_RunTests) {
  fbl::unique_fd media_codec(open(kMediaCodecPath, O_RDONLY));
  ASSERT_TRUE(media_codec.is_valid(), "%s", strerror(errno));

  zx::result test_device1 =
      TestDeviceBase::CreateFromTopologicalPathSuffix(media_codec, kTopologicalPathSuffix);
  ASSERT_OK(test_device1.status_value());

  zx::result parent_device = test_device1.value().GetParentDevice();
  ASSERT_OK(parent_device.status_value());

  ASSERT_OK(parent_device.value()
                .RebindDriver("/system/driver/amlogic_video_decoder_test.so")
                .status_value());

  zx::result test_device2 =
      TestDeviceBase::CreateFromTopologicalPathSuffix(media_codec, kTestTopologicalPathSuffix);
  ASSERT_OK(test_device2.status_value());

  zx::channel client, server;
  ASSERT_OK(zx::channel::create(0, &client, &server));
  ASSERT_OK(fdio_open("/tmp", static_cast<uint32_t>(fuchsia_io::OpenFlags::kRightWritable),
                      server.release()));
  ASSERT_OK(fidl::WireCall(test_device2.value().tester())
                ->SetOutputDirectoryHandle(std::move(client))
                .status());

  const fidl::WireResult result = fidl::WireCall(test_device2.value().tester())->RunTests();
  ASSERT_OK(result.status());
  const fidl::WireResponse response = result.value();
  ASSERT_OK(response.result);

  // Try to rebind the correct driver.
  ASSERT_OK(parent_device.value().RebindDriver({}).status_value());
}

// Test that unbinding and rebinding the driver works.
TEST(TestRunner, Rebind) {
  fbl::unique_fd media_codec(open(kMediaCodecPath, O_RDONLY));
  ASSERT_TRUE(media_codec.is_valid(), "%s", strerror(errno));

  zx::result test_device1 =
      TestDeviceBase::CreateFromTopologicalPathSuffix(media_codec, kTopologicalPathSuffix);
  ASSERT_OK(test_device1.status_value());

  zx::result parent_device = test_device1.value().GetParentDevice();
  ASSERT_OK(parent_device.status_value());

  // Use autobind to bind same driver.
  ASSERT_OK(parent_device.value().RebindDriver({}).status_value());
}

}  // namespace test
}  // namespace amlogic_decoder
