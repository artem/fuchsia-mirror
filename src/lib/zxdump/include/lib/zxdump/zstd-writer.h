// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ZXDUMP_INCLUDE_LIB_ZXDUMP_ZSTD_WRITER_H_
#define SRC_LIB_ZXDUMP_INCLUDE_LIB_ZXDUMP_ZSTD_WRITER_H_

#include <lib/fit/result.h>

#include <memory>
#include <string_view>

#include <fbl/unique_fd.h>

#include "types.h"

namespace zxdump {

// This replaces zxdump::FdWriter and does streaming ZSTD compression.
class ZstdWriter {
 public:
  using error_type = FdError;

  ZstdWriter() = default;

  ZstdWriter(ZstdWriter&& other) noexcept { *this = std::move(other); }

  ZstdWriter& operator=(ZstdWriter&& other) noexcept {
    std::swap(ctx_, other.ctx_);
    std::swap(buffer_, other.buffer_);
    std::swap(offset_, other.offset_);
    std::swap(fd_, other.fd_);
    return *this;
  }

  // The writer takes ownership of the fd.
  explicit ZstdWriter(fbl::unique_fd fd);

  ~ZstdWriter();

  // Both kinds of callbacks are handled the same way.
  auto WriteCallback() {
    return [this](size_t offset, ByteView data) -> fit::result<error_type> {
      return Write(offset, data);
    };
  }

  auto AccumulateFragmentsCallback() { return WriteCallback(); }

  fit::result<error_type, size_t> WriteFragments() { return fit::ok(offset_); }

  void ResetOffset() { offset_ = 0; }

  // Flush the compression buffers and finish writing all the output.
  fit::result<error_type> Finish();

 private:
  fit::result<error_type> Write(size_t offset, ByteView data);

  fit::result<error_type> Flush();

  void* ctx_ = nullptr;
  std::unique_ptr<std::byte[]> buffer_;
  size_t buffer_pos_ = 0;
  size_t offset_ = 0;
  fbl::unique_fd fd_;
};

}  // namespace zxdump

#endif  // SRC_LIB_ZXDUMP_INCLUDE_LIB_ZXDUMP_ZSTD_WRITER_H_
