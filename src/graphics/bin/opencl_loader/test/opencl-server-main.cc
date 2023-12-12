// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.magma/cpp/test_base.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.process.lifecycle/cpp/test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

class FakeMagmaDevice : public fidl::testing::TestBase<fuchsia_gpu_magma::CombinedDevice> {
 public:
  explicit FakeMagmaDevice(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void CloseAll() { bindings_.CloseAll(ZX_OK); }

  auto ProtocolConnector() {
    return [this](fidl::ServerEnd<fuchsia_gpu_magma::CombinedDevice> server_end) -> zx_status_t {
      bindings_.AddBinding(dispatcher_, std::move(server_end), this, fidl::kIgnoreBindingClosure);
      return ZX_OK;
    };
  }

 private:
  void GetIcdList(GetIcdListCompleter::Sync& completer) override {
    fuchsia_gpu_magma::IcdInfo info;
    info.component_url() =
        "fuchsia-pkg://fuchsia.com/opencl_loader_tests#meta/test_opencl_driver.cm";
    info.flags() = fuchsia_gpu_magma::IcdFlags::kSupportsOpencl;
    completer.Reply(std::vector{std::move(info)});
  }

  void Query(QueryRequest& request, QueryCompleter::Sync& completer) override {
    completer.Reply(fit::ok(fuchsia_gpu_magma::DeviceQueryResponse::WithSimpleResult(5)));
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("unexpected call to %s", name.c_str());
  }

  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<fuchsia_gpu_magma::CombinedDevice> bindings_;
};

class LifecycleHandler : public fidl::testing::TestBase<fuchsia_process_lifecycle::Lifecycle> {
 public:
  static LifecycleHandler Create(async::Loop* loop) {
    fidl::ServerEnd server_end = fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle>{
        zx::channel(zx_take_startup_handle(PA_LIFECYCLE))};
    ZX_ASSERT_MSG(server_end.is_valid(), "Invalid handle for PA_LIFECYCLE!");
    return LifecycleHandler(loop, std::move(server_end));
  }

 private:
  explicit LifecycleHandler(async::Loop* loop,
                            fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> server_end)
      : loop_(loop),
        binding_(loop->dispatcher(), std::move(server_end), this, fidl::kIgnoreBindingClosure) {}

  void Stop(StopCompleter::Sync& completer) override {
    loop_->Quit();
    binding_.Close(ZX_OK);
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("unexpected call to %s", name.c_str());
  }

  async::Loop* loop_;
  fidl::ServerBinding<fuchsia_process_lifecycle::Lifecycle> binding_;
};

int main(int argc, const char* const* argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  LifecycleHandler handler = LifecycleHandler::Create(&loop);
  fxl::SetLogSettingsFromCommandLine(fxl::CommandLineFromArgcArgv(argc, argv));

  // Use fs:: instead of vfs:: because vfs doesn't support executable directories.
  fs::SynchronousVfs vfs(loop.dispatcher());
  auto root = fbl::MakeRefCounted<fs::PseudoDir>();

  // Add a dev directory that the loader can watch for devices to be added.
  auto dev_gpu_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  root->AddEntry("dev-gpu", dev_gpu_dir);
  FakeMagmaDevice magma_device(loop.dispatcher());
  zx_status_t status = dev_gpu_dir->AddEntry(
      "000", fbl::MakeRefCounted<fs::Service>(magma_device.ProtocolConnector()));
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to add device: %s", zx_status_get_string(status));

  auto dev_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  status = root->AddEntry("dev", dev_gpu_dir);
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to add /dev: %s", zx_status_get_string(status));

  fidl::ServerEnd<fuchsia_io::Directory> dir_request{
      zx::channel(zx_take_startup_handle(PA_DIRECTORY_REQUEST))};
  status = vfs.ServeDirectory(root, std::move(dir_request), fs::Rights::ReadOnly());
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to serve outgoing: %s", zx_status_get_string(status));

  loop.Run();
  return 0;
}
