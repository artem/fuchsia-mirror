// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fan-controller.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/namespace.h>

#include <queue>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

namespace {

class FakeWatcher : public fidl::Server<fuchsia_thermal::ClientStateWatcher> {
 public:
  FakeWatcher(async_dispatcher_t* dispatcher,
              fidl::ServerEnd<fuchsia_thermal::ClientStateWatcher> server)
      : binding_(fidl::BindServer<fuchsia_thermal::ClientStateWatcher>(dispatcher,
                                                                       std::move(server), this)) {}
  ~FakeWatcher() {
    if (completer_) {
      completer_->Close(ZX_ERR_CANCELED);
      completer_.reset();
    }
  }

  void Watch(WatchCompleter::Sync& completer) override {
    {
      fbl::AutoLock _(&lock_);
      completer_ = completer.ToAsync();
    }
    sync_completion_signal(&watch_called_);
  }

  void WaitForWatch() { sync_completion_wait(&watch_called_, ZX_TIME_INFINITE); }
  void ReplyToWatch(uint64_t state) {
    WaitForWatch();
    sync_completion_reset(&watch_called_);

    fbl::AutoLock _(&lock_);
    ASSERT_TRUE(completer_.has_value());
    completer_->Reply(state);
    completer_.reset();
  }

 private:
  fidl::ServerBindingRef<fuchsia_thermal::ClientStateWatcher> binding_;

  fbl::Mutex lock_;
  std::optional<WatchCompleter::Async> completer_ __TA_GUARDED(lock_);
  sync_completion_t watch_called_;
};

class FakeClientStateServer : public fidl::Server<fuchsia_thermal::ClientStateConnector> {
 public:
  explicit FakeClientStateServer(fidl::ServerEnd<fuchsia_thermal::ClientStateConnector> server) {
    loop_.StartThread("fan-controller-test-fake-client-state-thread");
    binding_ = fidl::BindServer(loop_.dispatcher(), std::move(server), this);
  }
  ~FakeClientStateServer() override {
    EXPECT_TRUE(expected_connect_.empty());
    loop_.Shutdown();
  }

  void ExpectConnect(const std::string& client_type) {
    expected_connect_.emplace(client_type);
    watcher_connected_.emplace(client_type, sync_completion_t{});
  }
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    EXPECT_FALSE(expected_connect_.empty());
    EXPECT_STREQ(expected_connect_.front(), request.client_type());
    expected_connect_.pop();
    watchers_.emplace(std::piecewise_construct, std::forward_as_tuple(request.client_type()),
                      std::forward_as_tuple(loop_.dispatcher(), std::move(request.watcher())));
    sync_completion_signal(&watcher_connected_.at(request.client_type()));
  }

  FakeWatcher& watcher(const std::string& client_type) {
    sync_completion_wait(&watcher_connected_.at(client_type), ZX_TIME_INFINITE);
    return watchers_.at(client_type);
  }
  void WaitForWatch() {
    for (auto& [_, w] : watchers_) {
      w.WaitForWatch();
    }
  }

 private:
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<fidl::ServerBindingRef<fuchsia_thermal::ClientStateConnector>> binding_;

  std::queue<const std::string> expected_connect_;
  std::map<std::string, FakeWatcher> watchers_;
  std::map<std::string, sync_completion_t> watcher_connected_;
};

class FakeFanDevice : public fidl::Server<fuchsia_hardware_fan::Device> {
 public:
  explicit FakeFanDevice(std::string client_type) : client_type_(std::move(client_type)) {
    loop_.StartThread("fan-controller-test-fake-fan-device-loop");
  }
  ~FakeFanDevice() override {
    EXPECT_TRUE(expected_set_fan_level_.empty());
    loop_.Shutdown();
  }

  // fuchsia_hardware_fan.Device protocol implementation.
  void GetFanLevel(GetFanLevelCompleter::Sync& completer) override {
    completer.Reply({ZX_ERR_NOT_SUPPORTED, 0});
  }
  void SetFanLevel(SetFanLevelRequest& request, SetFanLevelCompleter::Sync& completer) override {
    EXPECT_FALSE(expected_set_fan_level_.empty());
    EXPECT_EQ(expected_set_fan_level_.front(), request.fan_level());
    expected_set_fan_level_.pop();
    completer.Reply(ZX_OK);
  }
  void GetClientType(GetClientTypeCompleter::Sync& completer) override {
    completer.Reply({client_type_});
  }

  fbl::RefPtr<fs::Service> AsService() {
    return fbl::MakeRefCounted<fs::Service>(
        [this](fidl::ServerEnd<fuchsia_hardware_fan::Device> server) {
          binding_ = fidl::BindServer(loop_.dispatcher(), std::move(server), this);
          return ZX_OK;
        });
  }

  void ExpectSetFanLevel(uint32_t level) { expected_set_fan_level_.emplace(level); }

 private:
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_fan::Device>> binding_;
  const std::string client_type_;

  std::queue<uint32_t> expected_set_fan_level_;
};

class FanControllerTest : public zxtest::Test {
 public:
  void SetUp() override {
    ASSERT_OK(fs_loop_.StartThread("fan-controller-test-fs-loop"));
    ASSERT_TRUE(dir_ != nullptr);

    ASSERT_EQ(fdio_ns_get_installed(&ns_), ZX_OK);
    zx::channel channel0, channel1;

    // Serve up the emulated fan directory
    ASSERT_EQ(zx::channel::create(0, &channel0, &channel1), ZX_OK);
    ASSERT_EQ(vfs_.Serve(dir_, std::move(channel0), fs::VnodeConnectionOptions::ReadOnly()), ZX_OK);
    ASSERT_EQ(fdio_ns_bind(ns_, fan_controller::kFanDirectory, channel1.release()), ZX_OK);

    auto endpoints = fidl::CreateEndpoints<fuchsia_thermal::ClientStateConnector>();
    EXPECT_OK(endpoints);
    client_state_ = std::make_unique<FakeClientStateServer>(std::move(endpoints->server));
    client_end_ = std::move(endpoints->client);
  }

  void TearDown() override {
    client_state_->WaitForWatch();

    // Scoped directory entries have gone out of scope, but to avoid races we remove all entries.
    sync_completion_t wait;
    async::PostTask(fs_loop_.dispatcher(), [this, &wait]() {
      dir_->RemoveAllEntries();
      sync_completion_signal(&wait);
    });
    sync_completion_wait(&wait, ZX_TIME_INFINITE);
    ASSERT_TRUE(dir_->IsEmpty());

    ASSERT_NE(ns_, nullptr);
    ASSERT_EQ(fdio_ns_unbind(ns_, fan_controller::kFanDirectory), ZX_OK);

    fan_controller_loop_.Shutdown();
    fs_loop_.Shutdown();
  }

  void StartFanController() {
    fan_controller_ = std::make_unique<fan_controller::FanController>(
        fan_controller_loop_.dispatcher(), std::move(client_end_));
  }

  // Holds a ref to a pseudo dir entry that removes the entry when this object goes out of scope.
  struct ScopedDirent {
    std::string name;
    fbl::RefPtr<fs::PseudoDir> dir;
    async_dispatcher_t* dispatcher;
    ~ScopedDirent() {
      async::PostTask(dispatcher, [n = name, d = dir]() { d->RemoveEntry(n); });
    }
  };

  ScopedDirent AddDevice(std::shared_ptr<FakeFanDevice> device) {
    auto name = std::to_string(next_device_number_++);
    sync_completion_t wait;
    async::PostTask(fs_loop_.dispatcher(), [this, name, device = std::move(device), &wait]() {
      ASSERT_OK(dir_->AddEntry(name, device->AsService()));
      sync_completion_signal(&wait);
    });
    sync_completion_wait(&wait, ZX_TIME_INFINITE);
    return {name, dir_, fs_loop_.dispatcher()};
  }

  auto ExpectFanCount(const std::string& client_type, size_t fan_count) {
    return [this, client_type, fan_count] {
      return fan_controller_->controller_fan_count(client_type) != fan_count;
    };
  }

 protected:
  async::Loop fan_controller_loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::unique_ptr<FakeClientStateServer> client_state_;

 private:
  async::Loop fs_loop_{&kAsyncLoopConfigNeverAttachToThread};

  std::unique_ptr<fan_controller::FanController> fan_controller_;
  fidl::ClientEnd<fuchsia_thermal::ClientStateConnector> client_end_;

  fdio_ns_t* ns_ = nullptr;
  uint32_t next_device_number_ = 0;
  fs::SynchronousVfs vfs_{fs_loop_.dispatcher()};
  // Note this _must_ be RefPtrs since vfs_ will try to AdoptRef on the raw pointer passed to it.
  fbl::RefPtr<fs::PseudoDir> dir_{fbl::MakeRefCounted<fs::PseudoDir>()};
};

// condition() returns false when we want to stop running the loop.
void RunLoopUntil(async::Loop& loop, std::function<bool()>&& condition) {
  do {
    loop.RunUntilIdle();
  } while (condition() && !usleep(100));
}

TEST_F(FanControllerTest, DeviceBeforeStart) {
  const std::string kClientType = "fan";
  auto fan = std::make_shared<FakeFanDevice>(kClientType);
  client_state_->ExpectConnect(kClientType);
  [[maybe_unused]] auto dev = AddDevice(fan);

  StartFanController();
  RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType, 1));

  fan->ExpectSetFanLevel(3);
  client_state_->watcher(kClientType).ReplyToWatch(3);
  fan_controller_loop_.RunUntilIdle();
}

TEST_F(FanControllerTest, DeviceAfterStart) {
  StartFanController();
  fan_controller_loop_.RunUntilIdle();

  const std::string kClientType = "fan";
  auto fan = std::make_shared<FakeFanDevice>(kClientType);
  client_state_->ExpectConnect(kClientType);
  [[maybe_unused]] auto dev = AddDevice(fan);

  RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType, 1));

  fan->ExpectSetFanLevel(3);
  client_state_->watcher(kClientType).ReplyToWatch(3);
  fan_controller_loop_.RunUntilIdle();
}

TEST_F(FanControllerTest, MultipleDevicesSameClientType) {
  StartFanController();
  fan_controller_loop_.RunUntilIdle();

  const std::string kClientType = "fan";
  auto fan0 = std::make_shared<FakeFanDevice>(kClientType);
  client_state_->ExpectConnect(kClientType);
  [[maybe_unused]] auto dev0 = AddDevice(fan0);

  RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType, 1));

  auto fan1 = std::make_shared<FakeFanDevice>(kClientType);
  [[maybe_unused]] auto dev1 = AddDevice(fan1);
  RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType, 2));

  fan0->ExpectSetFanLevel(3);
  fan1->ExpectSetFanLevel(3);
  client_state_->watcher(kClientType).ReplyToWatch(3);
  fan_controller_loop_.RunUntilIdle();
}

TEST_F(FanControllerTest, MultipleDevicesDifferentClientTypes) {
  StartFanController();
  fan_controller_loop_.RunUntilIdle();

  const std::string kClientType0 = "fan0";
  auto fan0 = std::make_shared<FakeFanDevice>(kClientType0);
  client_state_->ExpectConnect(kClientType0);
  [[maybe_unused]] auto dev0 = AddDevice(fan0);

  RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType0, 1));

  const std::string kClientType1 = "fan1";
  auto fan1 = std::make_shared<FakeFanDevice>(kClientType1);
  client_state_->ExpectConnect(kClientType1);
  [[maybe_unused]] auto dev1 = AddDevice(fan1);

  RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType1, 1));

  fan0->ExpectSetFanLevel(3);
  client_state_->watcher(kClientType0).ReplyToWatch(3);
  fan_controller_loop_.RunUntilIdle();

  fan1->ExpectSetFanLevel(2);
  client_state_->watcher(kClientType1).ReplyToWatch(2);
  fan_controller_loop_.RunUntilIdle();
}

TEST_F(FanControllerTest, DeviceRemoval) {
  const std::string kClientType = "fan";
  auto fan0 = std::make_shared<FakeFanDevice>(kClientType);
  client_state_->ExpectConnect(kClientType);
  [[maybe_unused]] auto dev0 = AddDevice(fan0);

  StartFanController();
  RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType, 1));

  {
    auto fan1 = std::make_shared<FakeFanDevice>(kClientType);
    [[maybe_unused]] auto dev1 = AddDevice(fan1);
    RunLoopUntil(fan_controller_loop_, ExpectFanCount(kClientType, 2));

    fan0->ExpectSetFanLevel(3);
    fan1->ExpectSetFanLevel(3);
    client_state_->watcher(kClientType).ReplyToWatch(3);
    fan_controller_loop_.RunUntilIdle();

    // Remove fan1 by letting it go out of scope. This expects an error log.
  }

  fan0->ExpectSetFanLevel(6);
  client_state_->watcher(kClientType).ReplyToWatch(6);
  fan_controller_loop_.RunUntilIdle();
}

}  // namespace
