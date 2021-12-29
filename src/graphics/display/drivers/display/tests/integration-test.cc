// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <zircon/pixelformat.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <fbl/auto_lock.h>
#include <zxtest/zxtest.h>

// clang-format off
#include "fbl/alloc_checker.h"
#include "lib/fidl/llcpp/array.h"
#include "lib/zx/clock.h"
#include "lib/zx/time.h"
#include "base.h"
#include "fidl_client.h"

// These must be included after base.h and fidl_client.h because the Banjo bindings use #defines
// that conflict with enum names in the FIDL bindings.
#include "../../fake/fake-display.h"
#include "../controller.h"
#include "../client.h"
// clang-format on

#include "src/lib/fsl/handles/object_info.h"
namespace sysmem = fuchsia_sysmem;

namespace display {
class IntegrationTest : public TestBase, public zxtest::WithParamInterface<bool> {
 public:
  fbl::RefPtr<display::DisplayInfo> display_info(uint64_t id) __TA_REQUIRES(controller()->mtx()) {
    auto iter = controller()->displays_.find(id);
    if (iter.IsValid()) {
      return iter.CopyPointer();
    } else {
      return nullptr;
    }
  }

  bool primary_client_connected() {
    fbl::AutoLock l(controller()->mtx());
    if (!controller()->primary_client_) {
      return false;
    }
    fbl::AutoLock cl(&controller()->primary_client_->mtx_);
    return (controller()->primary_client_ == controller()->active_client_ &&
            // DC processed the EnableVsync request. We can now expect vsync events.
            controller()->primary_client_->enable_vsync_);
  }

  bool virtcon_client_connected() {
    fbl::AutoLock l(controller()->mtx());
    return (controller()->vc_client_ != nullptr &&
            controller()->vc_client_ == controller()->active_client_);
  }

  bool vsync_acknowledge_delivered(uint64_t cookie) {
    fbl::AutoLock l(controller()->mtx());
    fbl::AutoLock cl(&controller()->primary_client_->mtx_);
    return controller()->primary_client_->handler_.LatestAckedCookie() == cookie;
  }

  size_t get_gamma_table_size() {
    fbl::AutoLock l(controller()->mtx());
    fbl::AutoLock cl(&controller()->primary_client_->mtx_);
    return controller()->primary_client_->handler_.GetGammaTableSize();
  }

  void SendVsyncAfterUnbind(std::unique_ptr<TestFidlClient> client, uint64_t display_id) {
    fbl::AutoLock l(controller()->mtx());
    // Reseting client will *start* client tear down.
    client.reset();
    ClientProxy* client_ptr = controller()->active_client_;
    EXPECT_OK(sync_completion_wait(client_ptr->handler_.fidl_unbound(), zx::sec(1).get()));
    // EnableVsync(false) has not completed here, because we are still holding controller()->mtx()
    client_ptr->OnDisplayVsync(display_id, 0, nullptr, 0);
  }

  bool primary_client_dead() {
    fbl::AutoLock l(controller()->mtx());
    return controller()->primary_client_ == nullptr;
  }

  bool virtcon_client_dead() {
    fbl::AutoLock l(controller()->mtx());
    return controller()->vc_client_ == nullptr;
  }

  void client_proxy_send_vsync() {
    fbl::AutoLock l(controller()->mtx());
    controller()->active_client_->OnDisplayVsync(0, 0, nullptr, 0);
  }

  void client_proxy_send_vsync_with_handle() {
    std::unique_ptr<uint64_t> handle = std::make_unique<uint64_t>();
    fbl::AutoLock l(controller()->mtx());
    controller()->active_client_->OnDisplayVsync(0, 0, handle.get(), 1);
  }

  void SendDisplayVsync() { display()->SendVsync(); }

  // |TestBase|
  void SetUp() override {
    TestBase::SetUp();
    zx::channel client, server;
    EXPECT_OK(zx::channel::create(0, &client, &server));
    fidl::UnownedClientEnd<sysmem::DriverConnector> connector{sysmem_fidl()->get()};
    EXPECT_TRUE(fidl::WireCall(connector)->Connect(std::move(server)).ok());
    sysmem_ = fidl::WireSyncClient<sysmem::Allocator>(std::move(client));
    sysmem_->SetDebugClientInfo(fidl::StringView::FromExternal(fsl::GetCurrentProcessName()),
                                fsl::GetCurrentProcessKoid());
  }

  // |TestBase|
  void TearDown() override {
    // Wait until the display core has processed all client disconnections before sending the last
    // vsync.
    EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
        [this]() { return primary_client_dead() && virtcon_client_dead(); }));

    // Send one last vsync, to make sure any blank configs take effect.
    SendDisplayVsync();
    EXPECT_EQ(0, controller()->TEST_imported_images_count());
    TestBase::TearDown();
  }

  fidl::WireSyncClient<sysmem::Allocator> sysmem_;
};

TEST_F(IntegrationTest, DISABLED_ClientsCanBail) {
  for (size_t i = 0; i < 100; i++) {
    RunLoopWithTimeoutOrUntil([this]() { return !primary_client_connected(); }, zx::sec(1));
    TestFidlClient client(sysmem_);
    ASSERT_TRUE(client.CreateChannel(display_fidl()->get(), false));
    ASSERT_TRUE(client.Bind(dispatcher()));
  }
}

TEST_F(IntegrationTest, MustUseUniqueEvenIDs) {
  TestFidlClient client(sysmem_);
  ASSERT_TRUE(client.CreateChannel(display_fidl()->get(), false));
  ASSERT_TRUE(client.Bind(dispatcher()));
  zx::event event_a, event_b, event_c;
  ASSERT_OK(zx::event::create(0, &event_a));
  ASSERT_OK(zx::event::create(0, &event_b));
  ASSERT_OK(zx::event::create(0, &event_c));
  {
    fbl::AutoLock lock(client.mtx());
    EXPECT_OK(client.dc_->ImportEvent(std::move(event_a), 123).status());
    // ImportEvent is one way. Expect the next call to fail.
    EXPECT_OK(client.dc_->ImportEvent(std::move(event_b), 123).status());
    // This test passes if it closes without deadlocking.
  }
  // TODO: Use LLCPP epitaphs when available to detect ZX_ERR_PEER_CLOSED.
}

TEST_F(IntegrationTest, SendVsyncsAfterEmptyConfig) {
  TestFidlClient vc_client(sysmem_);
  ASSERT_TRUE(vc_client.CreateChannel(display_fidl()->get(), /*is_vc=*/true));
  {
    fbl::AutoLock lock(vc_client.mtx());
    EXPECT_EQ(ZX_OK, vc_client.dc_->SetDisplayLayers(1, {}).status());
    EXPECT_EQ(ZX_OK, vc_client.dc_->ApplyConfig().status());
  }

  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // Present an image
  EXPECT_OK(primary_client->PresentImage());
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, id = primary_client->display_id()]() {
        fbl::AutoLock lock(controller()->mtx());
        auto info = display_info(id);
        return info->vsync_layer_count == 1;
      },
      zx::sec(1)));
  auto count = primary_client->vsync_count();
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get(), count]() { return p->vsync_count() > count; }, zx::sec(1)));

  // Set an empty config
  {
    fbl::AutoLock lock(primary_client->mtx());
    EXPECT_OK(primary_client->dc_->SetDisplayLayers(primary_client->display_id(), {}).status());
    EXPECT_OK(primary_client->dc_->ApplyConfig().status());
  }
  config_stamp_t empty_config_stamp = controller()->TEST_controller_stamp();
  // Wait for it to apply
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, id = primary_client->displays_[0].id_]() {
        fbl::AutoLock lock(controller()->mtx());
        auto info = display_info(id);
        return info->vsync_layer_count == 0;
      },
      zx::sec(1)));

  // The old client disconnects
  primary_client.reset();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil([this]() { return primary_client_dead(); }));

  // A new client connects
  primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }));
  // ... and presents before the previous client's empty vsync
  EXPECT_EQ(ZX_OK, primary_client->PresentImage());
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, id = primary_client->display_id()]() {
        fbl::AutoLock lock(controller()->mtx());
        auto info = display_info(id);
        return info->vsync_layer_count == 1;
      },
      zx::sec(1)));

  // Empty vsync for last client. Nothing should be sent to the new client.
  controller()->DisplayControllerInterfaceOnDisplayVsync(primary_client->display_id(), 0u,
                                                         &empty_config_stamp);

  // Send a second vsync, using the config the client applied.
  count = primary_client->vsync_count();
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [count, p = primary_client.get()]() { return p->vsync_count() > count; }, zx::sec(1)));
}

TEST_F(IntegrationTest, DISABLED_SendVsyncsAfterClientsBail) {
  TestFidlClient vc_client(sysmem_);
  ASSERT_TRUE(vc_client.CreateChannel(display_fidl()->get(), /*is_vc=*/true));
  {
    fbl::AutoLock lock(vc_client.mtx());
    EXPECT_EQ(ZX_OK, vc_client.dc_->SetDisplayLayers(1, {}).status());
    EXPECT_EQ(ZX_OK, vc_client.dc_->ApplyConfig().status());
  }

  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // Present an image
  EXPECT_OK(primary_client->PresentImage());
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, id = primary_client->display_id()]() {
        fbl::AutoLock lock(controller()->mtx());
        auto info = display_info(id);
        return info->vsync_layer_count == 1;
      },
      zx::sec(1)));

  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return p->vsync_count() == 1; }, zx::sec(1)));
  // Send the controller a vsync for an image / a config it won't recognize anymore.
  config_stamp_t invalid_config_stamp = {.value = controller()->TEST_controller_stamp().value - 1};
  controller()->DisplayControllerInterfaceOnDisplayVsync(primary_client->display_id(), 0u,
                                                         &invalid_config_stamp);

  // Send a second vsync, using the config the client applied.
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return p->vsync_count() == 2; }, zx::sec(1)));
  EXPECT_EQ(2, primary_client->vsync_count());
}

TEST_F(IntegrationTest, SendVsyncsAfterClientDies) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));
  auto id = primary_client->display_id();
  SendVsyncAfterUnbind(std::move(primary_client), id);
}

TEST_F(IntegrationTest, AcknowledgeVsync) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));
  EXPECT_EQ(0, primary_client->vsync_count());
  EXPECT_EQ(0, primary_client->get_cookie());

  // send vsyncs upto watermark level
  for (uint32_t i = 0; i < ClientProxy::kVsyncMessagesWatermark; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return (p->get_cookie() != 0); }, zx::sec(3)));
  EXPECT_EQ(ClientProxy::kVsyncMessagesWatermark, primary_client->vsync_count());

  // acknowledge
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(primary_client->get_cookie());
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));
}

TEST_F(IntegrationTest, AcknowledgeVsyncAfterQueueFull) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // send vsyncs until max vsync
  uint32_t vsync_count = ClientProxy::kMaxVsyncMessages;
  while (vsync_count--) {
    client_proxy_send_vsync();
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return (p->vsync_count() == ClientProxy::kMaxVsyncMessages); },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
  EXPECT_NE(0, primary_client->get_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(primary_client->get_cookie());
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  client_proxy_send_vsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() == ClientProxy::kMaxVsyncMessages + kNumVsync + 1);
      },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages + kNumVsync + 1, primary_client->vsync_count());
}

TEST_F(IntegrationTest, AcknowledgeVsyncAfterLongTime) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // send vsyncs until max vsyncs
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return (p->vsync_count() == ClientProxy::kMaxVsyncMessages); },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
  EXPECT_NE(0, primary_client->get_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a lot
  constexpr uint32_t kNumVsync = ClientProxy::kVsyncBufferSize * 10;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(primary_client->get_cookie());
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  client_proxy_send_vsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() ==
                ClientProxy::kMaxVsyncMessages + ClientProxy::kVsyncBufferSize + 1);
      },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages + ClientProxy::kVsyncBufferSize + 1,
            primary_client->vsync_count());
}

TEST_F(IntegrationTest, InvalidVSyncCookie) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return (p->vsync_count() == ClientProxy::kMaxVsyncMessages); },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
  EXPECT_NE(0, primary_client->get_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync with invalid cookie
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(0xdeadbeef);
  }
  EXPECT_FALSE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));

  // We should still not receive vsync events since acknowledge did not use valid cookie
  client_proxy_send_vsync();
  EXPECT_FALSE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() == ClientProxy::kMaxVsyncMessages + kNumVsync + 1);
      },
      zx::sec(1)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
}

TEST_F(IntegrationTest, AcknowledgeVsyncWithOldCookie) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return (p->vsync_count() == ClientProxy::kMaxVsyncMessages); },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
  EXPECT_NE(0, primary_client->get_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(primary_client->get_cookie());
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  client_proxy_send_vsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() == ClientProxy::kMaxVsyncMessages + kNumVsync + 1);
      },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages + kNumVsync + 1, primary_client->vsync_count());

  // save old cookie
  uint64_t old_cookie = primary_client->get_cookie();

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    client_proxy_send_vsync();
  }

  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() == ClientProxy::kMaxVsyncMessages * 2);
      },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages * 2, primary_client->vsync_count());
  EXPECT_NE(0, primary_client->get_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  for (uint32_t i = 0; i < ClientProxy::kVsyncBufferSize; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages * 2, primary_client->vsync_count());

  // now let's acknowledge vsync with old cookie
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(old_cookie);
  }
  EXPECT_FALSE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));

  // Since we did not acknowledge with most recent cookie, we should not get any vsync events back
  client_proxy_send_vsync();
  EXPECT_FALSE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() == (ClientProxy::kMaxVsyncMessages * 2) + kNumVsync + 1);
      },
      zx::sec(1)));
  // count should still remain the same
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages * 2, primary_client->vsync_count());

  // now let's acknowledge with valid cookie
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(primary_client->get_cookie());
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  client_proxy_send_vsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() ==
                (ClientProxy::kMaxVsyncMessages * 2) + ClientProxy::kVsyncBufferSize + 1);
      },
      zx::sec(3)));
  EXPECT_EQ((ClientProxy::kMaxVsyncMessages * 2) + ClientProxy::kVsyncBufferSize + 1,
            primary_client->vsync_count());
}

TEST_F(IntegrationTest, InvalidImageHandleAfterSave) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    client_proxy_send_vsync();
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return (p->vsync_count() == ClientProxy::kMaxVsyncMessages); },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
  EXPECT_NE(0, primary_client->get_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  // this will get stored
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    client_proxy_send_vsync_with_handle();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->AcknowledgeVsync(primary_client->get_cookie());
  }
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, p = primary_client.get()]() { return vsync_acknowledge_delivered(p->get_cookie()); },
      zx::sec(1)));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  client_proxy_send_vsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() {
        return (p->vsync_count() == ClientProxy::kMaxVsyncMessages + kNumVsync + 1);
      },
      zx::sec(3)));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages + kNumVsync + 1, primary_client->vsync_count());
}

TEST_F(IntegrationTest, ImportGammaTable) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  uint64_t gamma_table_id = 3;
  ::fidl::Array<float, 256> gamma_red = {{0.1f}};
  ::fidl::Array<float, 256> gamma_green = {{0.2f}};
  ::fidl::Array<float, 256> gamma_blue = {{0.3f}};
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->ImportGammaTable(gamma_table_id, gamma_red, gamma_green, gamma_blue);
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return get_gamma_table_size() == 1; }, zx::sec(1)));
  }
}

TEST_F(IntegrationTest, ReleaseGammaTable) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  uint64_t gamma_table_id = 3;
  ::fidl::Array<float, 256> gamma_red = {{0.1f}};
  ::fidl::Array<float, 256> gamma_green = {{0.2f}};
  ::fidl::Array<float, 256> gamma_blue = {{0.3f}};
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->ImportGammaTable(gamma_table_id, gamma_red, gamma_green, gamma_blue);
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return get_gamma_table_size() == 1; }, zx::sec(1)));
    primary_client->dc_->ReleaseGammaTable(gamma_table_id);
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return get_gamma_table_size() == 0; }, zx::sec(1)));
  }
}

TEST_F(IntegrationTest, ReleaseInvalidGammaTable) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  uint64_t gamma_table_id = 3;
  ::fidl::Array<float, 256> gamma_red = {{0.1f}};
  ::fidl::Array<float, 256> gamma_green = {{0.2f}};
  ::fidl::Array<float, 256> gamma_blue = {{0.3f}};
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->ImportGammaTable(gamma_table_id, gamma_red, gamma_green, gamma_blue);
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return get_gamma_table_size() == 1; }, zx::sec(1)));
    primary_client->dc_->ReleaseGammaTable(gamma_table_id + 5);
    EXPECT_FALSE(
        RunLoopWithTimeoutOrUntil([this]() { return get_gamma_table_size() == 0; }, zx::sec(1)));
  }
}

TEST_F(IntegrationTest, SetGammaTable) {
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  uint64_t gamma_table_id = 3;
  ::fidl::Array<float, 256> gamma_red = {{0.1f}};
  ::fidl::Array<float, 256> gamma_green = {{0.2f}};
  ::fidl::Array<float, 256> gamma_blue = {{0.3f}};
  {
    fbl::AutoLock lock(primary_client->mtx());
    primary_client->dc_->ImportGammaTable(gamma_table_id, gamma_red, gamma_green, gamma_blue);
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return get_gamma_table_size() == 1; }, zx::sec(1)));
    primary_client->dc_->SetDisplayGammaTable(primary_client->display_id(), gamma_table_id);
  }
}

TEST_F(IntegrationTest, ImportImage_InvalidCollection) {
  TestFidlClient client(sysmem_);
  ASSERT_TRUE(client.CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(client.Bind(dispatcher()));

  fbl::AutoLock lock(client.mtx());
  auto cl_reply = client.dc_->CreateLayer();
  ASSERT_TRUE(cl_reply.ok());
  ASSERT_OK(cl_reply->res);
  // Importing an image from a non-existent collection should fail.
  auto ii_reply = client.dc_->ImportImage(client.displays_[0].image_config_, 0xffeeeedd, 0);
  ASSERT_NE(ii_reply->res, ZX_OK);
}

TEST_F(IntegrationTest, ClampRgb) {
  // Create vc client
  TestFidlClient vc_client(sysmem_);
  ASSERT_TRUE(vc_client.CreateChannel(display_fidl()->get(), /*is_vc=*/true));
  {
    fbl::AutoLock lock(vc_client.mtx());
    // set mode to Fallback
    vc_client.dc_->SetVirtconMode(1);
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return virtcon_client_connected(); }, zx::sec(1)));
    // Clamp RGB to a minimum value
    vc_client.dc_->SetMinimumRgb(32);
    EXPECT_TRUE(RunLoopWithTimeoutOrUntil([this]() { return display()->GetClampRgbValue() == 32; },
                                          zx::sec(1)));
  }

  // Create a primary client
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));
  {
    fbl::AutoLock lock(primary_client->mtx());
    // Clamp RGB to a new value
    primary_client->dc_->SetMinimumRgb(1);
    EXPECT_TRUE(RunLoopWithTimeoutOrUntil([this]() { return display()->GetClampRgbValue() == 1; },
                                          zx::sec(1)));
  }
  // close client and wait for virtcon to become active again
  primary_client.reset(nullptr);
  // Apply a config for virtcon client to become active.
  {
    fbl::AutoLock lock(vc_client.mtx());
    EXPECT_EQ(ZX_OK, vc_client.dc_->SetDisplayLayers(1, {}).status());
    EXPECT_EQ(ZX_OK, vc_client.dc_->ApplyConfig().status());
  }
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return virtcon_client_connected(); }, zx::sec(1)));
  SendDisplayVsync();
  // make sure clamp value was restored
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil([this]() { return display()->GetClampRgbValue() == 32; },
                                        zx::sec(1)));
}

TEST_F(IntegrationTest, VsyncImagesHiddenIfNotFromActiveClient_PrimaryToVirtcon) {
  // Create and bind virtcon client.
  TestFidlClient vc_client(sysmem_);
  ASSERT_TRUE(vc_client.CreateChannel(display_fidl()->get(), /*is_vc=*/true));
  {
    fbl::AutoLock lock(vc_client.mtx());
    EXPECT_EQ(ZX_OK, vc_client.dc_
                         ->SetVirtconMode(static_cast<uint8_t>(
                             fuchsia_hardware_display::wire::VirtconMode::kFallback))
                         .status());
  }
  ASSERT_TRUE(vc_client.Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return virtcon_client_connected(); }, zx::sec(1)));

  // Present an image from virtcon client
  EXPECT_OK(vc_client.PresentImage());
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, id = vc_client.display_id()]() {
        fbl::AutoLock lock(controller()->mtx());
        auto info = display_info(id);
        return info->vsync_layer_count == 1;
      },
      zx::sec(1)));
  auto vc_vsync_count = vc_client.vsync_count();
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = &vc_client, vc_vsync_count]() { return p->vsync_count() > vc_vsync_count; },
      zx::sec(1)));
  EXPECT_EQ(1u, vc_client.recent_vsync_images().size());

  // Create and bind primary client.
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  // Apply a config for client to become active.
  {
    fbl::AutoLock lock(primary_client->mtx());
    EXPECT_EQ(ZX_OK, primary_client->dc_->SetDisplayLayers(1, {}).status());
    EXPECT_EQ(ZX_OK, primary_client->dc_->ApplyConfig().status());
  }
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // Since at this moment the displayed images are all from virtcon clients,
  // the Vsync event received by primary client should not contain any image
  // handles.
  auto primary_vsync_count = primary_client->vsync_count();
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get(), primary_vsync_count]() {
        return p->vsync_count() > primary_vsync_count;
      },
      zx::sec(1)));
  EXPECT_EQ(0u, primary_client->recent_vsync_images().size());
}

TEST_F(IntegrationTest, VsyncImagesHiddenIfNotFromActiveClient_PrimaryToPrimary) {
  {
    // Create and bind the first primary client.
    TestFidlClient primary_client1(sysmem_);
    ASSERT_TRUE(primary_client1.CreateChannel(display_fidl()->get(), /*is_vc=*/false));
    ASSERT_TRUE(primary_client1.Bind(dispatcher()));
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

    // Present an image from the first primary client
    EXPECT_OK(primary_client1.PresentImage());
    EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
        [this, id = primary_client1.display_id()]() {
          fbl::AutoLock lock(controller()->mtx());
          auto info = display_info(id);
          return info->vsync_layer_count == 1;
        },
        zx::sec(1)));
    auto vc_vsync_count = primary_client1.vsync_count();
    SendDisplayVsync();
    EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
        [p = &primary_client1, vc_vsync_count]() { return p->vsync_count() > vc_vsync_count; },
        zx::sec(1)));
    EXPECT_EQ(1u, primary_client1.recent_vsync_images().size());
  }

  // Wait until the first primary client is disconnected.
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return !primary_client_connected(); }, zx::sec(1)));

  {
    // Create and bind the second primary client.
    auto primary_client2 = std::make_unique<TestFidlClient>(sysmem_);
    ASSERT_TRUE(primary_client2->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
    ASSERT_TRUE(primary_client2->Bind(dispatcher()));
    // Apply a config for client to become active.
    {
      fbl::AutoLock lock(primary_client2->mtx());
      EXPECT_EQ(ZX_OK, primary_client2->dc_->SetDisplayLayers(1, {}).status());
      EXPECT_EQ(ZX_OK, primary_client2->dc_->ApplyConfig().status());
    }
    EXPECT_TRUE(
        RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

    // Since at this moment the displayed images are all from virtcon clients,
    // the Vsync event received by primary client should not contain any image
    // handles.
    auto primary_vsync_count = primary_client2->vsync_count();
    SendDisplayVsync();
    EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
        [p = primary_client2.get(), primary_vsync_count]() {
          return p->vsync_count() > primary_vsync_count;
        },
        zx::sec(1)));
    EXPECT_EQ(0u, primary_client2->recent_vsync_images().size());
  }
}

TEST_F(IntegrationTest, EmptyConfigIsNotApplied) {
  // Create and bind virtcon client.
  TestFidlClient vc_client(sysmem_);
  ASSERT_TRUE(vc_client.CreateChannel(display_fidl()->get(), /*is_vc=*/true));
  {
    fbl::AutoLock lock(vc_client.mtx());
    EXPECT_EQ(ZX_OK, vc_client.dc_
                         ->SetVirtconMode(static_cast<uint8_t>(
                             fuchsia_hardware_display::wire::VirtconMode::kFallback))
                         .status());
  }
  ASSERT_TRUE(vc_client.Bind(dispatcher()));
  {
    fbl::AutoLock lock(vc_client.mtx());
    EXPECT_EQ(ZX_OK, vc_client.dc_->SetDisplayLayers(1, {}).status());
    EXPECT_EQ(ZX_OK, vc_client.dc_->ApplyConfig().status());
  }
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return virtcon_client_connected(); }, zx::sec(1)));

  // Create and bind primary client.
  auto primary_client = std::make_unique<TestFidlClient>(sysmem_);
  ASSERT_TRUE(primary_client->CreateChannel(display_fidl()->get(), /*is_vc=*/false));
  ASSERT_TRUE(primary_client->Bind(dispatcher()));
  EXPECT_TRUE(
      RunLoopWithTimeoutOrUntil([this]() { return primary_client_connected(); }, zx::sec(1)));

  // Virtcon client should remain active until primary client has set a config.
  auto vc_vsync_count = vc_client.vsync_count();
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = &vc_client, vc_vsync_count]() { return p->vsync_count() > vc_vsync_count; },
      zx::sec(1)));
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get()]() { return p->vsync_count() == 0; }, zx::sec(1)));

  // Present an image from the primary client.
  EXPECT_OK(primary_client->PresentImage());
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [this, id = primary_client->display_id()]() {
        fbl::AutoLock lock(controller()->mtx());
        auto info = display_info(id);
        return info->vsync_layer_count == 1;
      },
      zx::sec(1)));

  // Primary client should have become active after a config was set.
  auto primary_vsync_count = primary_client->vsync_count();
  SendDisplayVsync();
  EXPECT_TRUE(RunLoopWithTimeoutOrUntil(
      [p = primary_client.get(), primary_vsync_count]() {
        return p->vsync_count() > primary_vsync_count;
      },
      zx::sec(1)));
}

}  // namespace display
