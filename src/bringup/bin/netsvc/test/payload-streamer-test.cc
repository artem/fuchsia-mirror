// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/bringup/bin/netsvc/payload-streamer.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <zircon/errors.h>

#include <optional>

#include <zxtest/zxtest.h>

class PayloadStreamerTest : public zxtest::Test {
 protected:
  PayloadStreamerTest() : loop_(&kAsyncLoopConfigAttachToCurrentThread) {}

  static zx_status_t DefaultCallback(void* buf, size_t offset, size_t size, size_t* actual) {
    *actual = size;
    return ZX_OK;
  }

  void StartStreamer(netsvc::ReadCallback callback = DefaultCallback) {
    auto endpoints = fidl::Endpoints<fuchsia_paver::PayloadStream>::Create();

    client_ = fidl::WireSyncClient(std::move(endpoints.client));
    payload_streamer_.emplace(std::move(endpoints.server), std::move(callback));
    loop_.StartThread("payload-streamer-test-loop");
  }

  async::Loop loop_;
  fidl::WireSyncClient<fuchsia_paver::PayloadStream> client_;
  std::optional<netsvc::PayloadStreamer> payload_streamer_;
};

TEST_F(PayloadStreamerTest, RegisterVmo) {
  StartStreamer();

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  auto result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);
}

TEST_F(PayloadStreamerTest, RegisterVmoTwice) {
  StartStreamer();

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  auto result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  auto result2 = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result2.status());
  ASSERT_EQ(result2.value().status, ZX_ERR_ALREADY_BOUND);
}

TEST_F(PayloadStreamerTest, ReadData) {
  StartStreamer();

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  auto result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  auto result2 = client_->ReadData();
  ASSERT_OK(result2.status());
  ASSERT_TRUE(result2.value().result.is_info());
  ASSERT_EQ(result2.value().result.info().offset, 0);
  ASSERT_EQ(result2.value().result.info().size, zx_system_get_page_size());
}

TEST_F(PayloadStreamerTest, ReadDataWithoutRegisterVmo) {
  StartStreamer();

  auto result = client_->ReadData();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().result.is_err());
  ASSERT_NE(result.value().result.err(), ZX_OK);
}

TEST_F(PayloadStreamerTest, ReadDataHalfFull) {
  StartStreamer([](void* buf, size_t offset, size_t size, size_t* actual) {
    *actual = size / 2;
    return ZX_OK;
  });

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  auto result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result.status());

  auto result2 = client_->ReadData();
  ASSERT_OK(result2.status());
  ASSERT_TRUE(result2.value().result.is_info());
  ASSERT_EQ(result2.value().result.info().offset, 0);
  ASSERT_EQ(result2.value().result.info().size, zx_system_get_page_size() / 2);
}

TEST_F(PayloadStreamerTest, ReadEof) {
  StartStreamer([](void* buf, size_t offset, size_t size, size_t* actual) {
    *actual = 0;
    return ZX_OK;
  });

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  auto result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  auto result2 = client_->ReadData();
  ASSERT_OK(result2.status());
  ASSERT_TRUE(result2.value().result.is_eof());
}

TEST_F(PayloadStreamerTest, ReadFailure) {
  StartStreamer(
      [](void* buf, size_t offset, size_t size, size_t* actual) { return ZX_ERR_INTERNAL; });

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  auto result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result.status());
  ASSERT_OK(result.value().status);

  auto result2 = client_->ReadData();
  ASSERT_OK(result2.status());
  ASSERT_TRUE(result2.value().result.is_err());
  ASSERT_NE(result2.value().result.err(), ZX_OK);
}
