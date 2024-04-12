// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "payload-streamer.h"

#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <optional>

#include <zxtest/zxtest.h>

namespace {

constexpr char kFileData[] = "lalalala";

TEST(PayloadStreamerTest, TrivialLifetime) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto stream = fidl::Endpoints<fuchsia_paver::PayloadStream>::Create();
  disk_pave::PayloadStreamer streamer(std::move(stream.server), fbl::unique_fd());
}

class PayloadStreamerTest : public zxtest::Test {
 public:
  PayloadStreamerTest() : loop_(&kAsyncLoopConfigAttachToCurrentThread) {
    mktemp(tempfile_name_);
    ASSERT_NE(strlen(tempfile_name_), 0);

    fbl::unique_fd src(open(tempfile_name_, O_RDWR | O_CREAT, S_IRUSR | S_IWUSR));
    ASSERT_EQ(write(src.get(), kFileData, sizeof(kFileData)), sizeof(kFileData));
    lseek(src.get(), 0, SEEK_SET);

    auto stream = fidl::CreateEndpoints<fuchsia_paver::PayloadStream>();
    ASSERT_OK(stream.status_value());
    streamer_.emplace(std::move(stream->server), std::move(src));
    client_ = fidl::WireSyncClient(std::move(stream->client));
    loop_.StartThread("payload-stream-test-loop");
  }

  ~PayloadStreamerTest() { unlink(tempfile_name_); }

 protected:
  async::Loop loop_;
  std::optional<disk_pave::PayloadStreamer> streamer_;
  fidl::WireSyncClient<fuchsia_paver::PayloadStream> client_;

 private:
  char tempfile_name_[20] = "/tmp/payload.XXXXXX";
};

TEST_F(PayloadStreamerTest, RegisterVmo) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  auto result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(result.status());
  EXPECT_OK(result.value().status);
}

TEST_F(PayloadStreamerTest, RegisterMultipleVmo) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  {
    auto result = client_->RegisterVmo(std::move(vmo));
    ASSERT_OK(result.status());
    EXPECT_OK(result.value().status);
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  }
  {
    auto result = client_->RegisterVmo(std::move(vmo));
    ASSERT_OK(result.status());
    EXPECT_EQ(result.value().status, ZX_ERR_ALREADY_BOUND);
  }
}

TEST_F(PayloadStreamerTest, RegisterInvalidVmo) {
  EXPECT_FALSE(client_->RegisterVmo(zx::vmo()).ok());
}

TEST_F(PayloadStreamerTest, ReadNoVmoRegistered) {
  auto call_result = client_->ReadData();
  ASSERT_OK(call_result.status());
  const fuchsia_paver::wire::ReadResult& result = call_result.value().result;
  ASSERT_TRUE(result.is_err());
  EXPECT_NE(result.err(), ZX_OK);
}

TEST_F(PayloadStreamerTest, ReadData) {
  zx::vmo vmo, dup;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  ASSERT_OK(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup));
  auto register_result = client_->RegisterVmo(std::move(dup));
  ASSERT_OK(register_result.status());
  EXPECT_OK(register_result.value().status);

  auto read_result = client_->ReadData();
  ASSERT_OK(read_result.status());
  ASSERT_TRUE(read_result.value().result.is_info());

  char buffer[sizeof(kFileData)] = {};
  ASSERT_EQ(read_result.value().result.info().size, sizeof(buffer));
  ASSERT_OK(vmo.read(buffer, read_result.value().result.info().offset,
                     read_result.value().result.info().size));
  ASSERT_EQ(memcmp(kFileData, buffer, sizeof(buffer)), 0);
}

TEST_F(PayloadStreamerTest, ReadEof) {
  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  auto register_result = client_->RegisterVmo(std::move(vmo));
  ASSERT_OK(register_result.status());
  EXPECT_OK(register_result.value().status);

  {
    fidl::WireResult<fuchsia_paver::PayloadStream::ReadData> call_result = client_->ReadData();
    ASSERT_OK(call_result.status());
    ASSERT_TRUE(call_result.value().result.is_info());
  }

  {
    auto call_result = client_->ReadData();
    ASSERT_OK(call_result.status());
    ASSERT_TRUE(call_result.value().result.is_eof());
  }

  {
    auto call_result = client_->ReadData();
    ASSERT_OK(call_result.status());
    ASSERT_TRUE(call_result.value().result.is_eof());
  }
}

}  // namespace
