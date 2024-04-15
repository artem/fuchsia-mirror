// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Tests for the debuglog.

#include <lib/standalone-test/standalone.h>
#include <zircon/syscalls/log.h>

#include <zxtest/zxtest.h>

namespace {

TEST(DebugLogTest, WriteRead) {
  zx_handle_t log_handle = 0;
  zx::unowned_resource system_resource = standalone::GetSystemResource();

  zx::result<zx::resource> result =
      standalone::GetSystemResourceWithBase(system_resource, ZX_RSRC_SYSTEM_DEBUGLOG_BASE);
  ASSERT_OK(result.status_value());
  zx::resource debuglog_resource = std::move(result.value());

  ASSERT_OK(zx_debuglog_create(debuglog_resource.get(), ZX_LOG_FLAG_READABLE, &log_handle));

  // Ensure something is written.
  static const char kTestMsg[] = "Debuglog test message.\n";
  ASSERT_OK(zx_debuglog_write(log_handle, 0, kTestMsg, sizeof(kTestMsg)));

  // In-case the read bound isn't respected create a buffer large enough to hopefully
  // prevent corruption.
  char buf[10240]{0};

  // But only report a smaller size for the buffer.
  const size_t read_len = 3;
  const auto status_or_size = zx_debuglog_read(log_handle, 0, buf, read_len);
  ASSERT_EQ(read_len, status_or_size);

  // Ensure that only read_len bytes were written to our buffer.
  const char empty[10]{0};
  ASSERT_EQ(0, memcmp(buf + read_len, empty, sizeof(empty)));

  ASSERT_OK(zx_handle_close(log_handle));
}

TEST(DebugLogTest, InvalidOptions) {
  zx_handle_t log_handle = 0;
  zx::unowned_resource system_resource = standalone::GetSystemResource();

  zx::result<zx::resource> result =
      standalone::GetSystemResourceWithBase(system_resource, ZX_RSRC_SYSTEM_DEBUGLOG_BASE);
  ASSERT_OK(result.status_value());
  zx::resource debuglog_resource = std::move(result.value());

  // Ensure giving invalid options returns an error.
  EXPECT_EQ(zx_debuglog_create(debuglog_resource.get(), 1, &log_handle), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(log_handle, 0);

  EXPECT_EQ(zx_debuglog_create(debuglog_resource.get(), 1 | ZX_LOG_FLAG_READABLE, &log_handle),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(log_handle, 0);
}

TEST(DebugLogTest, InvalidHandleAllowedOnlyForWrite) {
  zx_handle_t log_handle = 0;
  zx::unowned_resource system_resource = standalone::GetSystemResource();

  zx::result<zx::resource> result =
      standalone::GetSystemResourceWithBase(system_resource, ZX_RSRC_SYSTEM_DEBUGLOG_BASE);
  ASSERT_OK(result.status_value());
  zx::resource debuglog_resource = std::move(result.value());

  // Requesting a read-write handle with a valid debuglog resource should work
  ASSERT_OK(zx_debuglog_create(debuglog_resource.get(), ZX_LOG_FLAG_READABLE, &log_handle));
  ASSERT_OK(zx_handle_close(log_handle));
  log_handle = ZX_HANDLE_INVALID;

  // Requesting a write-only handle with an invalid resource should work,
  // since the dynamic linker needs to be able to log something somewhere when
  // it can't bootstrap a process
  ASSERT_OK(zx_debuglog_create(ZX_HANDLE_INVALID, 0, &log_handle));
  ASSERT_OK(zx_handle_close(log_handle));
  log_handle = ZX_HANDLE_INVALID;

  // But requesting a read-write handle with an invalid resource represents
  // an information leak, so the kernel checks the first arg, and if it's not a
  // valid handle, then we get an error
  EXPECT_EQ(zx_debuglog_create(ZX_HANDLE_INVALID, ZX_LOG_FLAG_READABLE, &log_handle),
            ZX_ERR_BAD_HANDLE);
}

TEST(DebugLogTest, MaxMessageSize) {
  zx_handle_t log_handle = ZX_HANDLE_INVALID;
  zx::unowned_resource system_resource = standalone::GetSystemResource();

  zx::result<zx::resource> result =
      standalone::GetSystemResourceWithBase(system_resource, ZX_RSRC_SYSTEM_DEBUGLOG_BASE);
  ASSERT_OK(result.status_value());
  zx::resource debuglog_resource = std::move(result.value());

  ASSERT_OK(zx_debuglog_create(debuglog_resource.get(), ZX_LOG_FLAG_READABLE, &log_handle));

  // msg is too large and should be truncated.
  char msg[ZX_LOG_RECORD_DATA_MAX + 1];
  memset(msg, 'A', sizeof(msg));
  ASSERT_OK(zx_debuglog_write(log_handle, 0, msg, sizeof(msg)));

  // Use an oversized buffer to ensure that trunction is not caused by our buffer size.
  alignas(zx_log_record_t) char buf[2 * ZX_LOG_RECORD_MAX]{0};
  zx_log_record_t* const record = reinterpret_cast<zx_log_record_t*>(buf);

  // Read until we find our message.
  size_t size;
  do {
    zx_status_t status_or_size = zx_debuglog_read(log_handle, 0, buf, sizeof(buf));
    if (status_or_size < 0) {
      ASSERT_EQ(ZX_ERR_SHOULD_WAIT, status_or_size);
      continue;
    }
    ASSERT_GT(status_or_size, 0);
    size = status_or_size;
    ASSERT_LE(size, ZX_LOG_RECORD_MAX);
    ASSERT_LE(record->datalen, ZX_LOG_RECORD_DATA_MAX);
  } while (memcmp(record->data, msg, record->datalen) != 0);

  // See that the message was truncated to exactly the maximum size.
  ASSERT_EQ(size, ZX_LOG_RECORD_MAX);
  ASSERT_EQ(record->datalen, ZX_LOG_RECORD_DATA_MAX);

  ASSERT_OK(zx_handle_close(log_handle));
}

}  // namespace
