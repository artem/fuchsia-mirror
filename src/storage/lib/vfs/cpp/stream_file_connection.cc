// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/stream_file_connection.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zx/handle.h>
#include <lib/zx/result.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <zircon/assert.h>
#include <zircon/rights.h>

#include <utility>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/connection.h"
#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/file_connection.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {

namespace internal {

StreamFileConnection::StreamFileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                                           fuchsia_io::Rights rights, bool append,
                                           zx::stream stream, zx_koid_t koid)
    : FileConnection(vfs, std::move(vnode), rights, append, koid), stream_(std::move(stream)) {}

zx_status_t StreamFileConnection::ReadInternal(void* data, size_t len, size_t* out_actual) {
  FS_PRETTY_TRACE_DEBUG("[FileRead] options: ", options());
  if (!(rights() & fuchsia_io::Rights::kReadBytes)) {
    return ZX_ERR_BAD_HANDLE;
  }
  if (len > fio::wire::kMaxBuf) {
    return ZX_ERR_INVALID_ARGS;
  }
  zx_iovec_t vector = {
      .buffer = data,
      .capacity = len,
  };
  zx_status_t status = stream_.readv(0, &vector, 1, out_actual);
  if (status == ZX_OK) {
    ZX_DEBUG_ASSERT(*out_actual <= len);
  }
  return status;
}

void StreamFileConnection::Read(ReadRequestView request, ReadCompleter::Sync& completer) {
  uint8_t data[fio::wire::kMaxBuf];
  size_t actual = 0;
  zx_status_t status = ReadInternal(data, request->count, &actual);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(data, actual));
  }
}

zx_status_t StreamFileConnection::ReadAtInternal(void* data, size_t len, size_t offset,
                                                 size_t* out_actual) {
  FS_PRETTY_TRACE_DEBUG("[FileReadAt] options: ", options());
  if (!(rights() & fuchsia_io::Rights::kReadBytes)) {
    return ZX_ERR_BAD_HANDLE;
  }
  if (len > fio::wire::kMaxBuf) {
    return ZX_ERR_INVALID_ARGS;
  }
  zx_iovec_t vector = {
      .buffer = data,
      .capacity = len,
  };
  zx_status_t status = stream_.readv_at(0, offset, &vector, 1, out_actual);
  if (status == ZX_OK) {
    ZX_DEBUG_ASSERT(*out_actual <= len);
  }
  return status;
}

void StreamFileConnection::ReadAt(ReadAtRequestView request, ReadAtCompleter::Sync& completer) {
  uint8_t data[fio::wire::kMaxBuf];
  size_t actual = 0;
  zx_status_t status = ReadAtInternal(data, request->count, request->offset, &actual);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(data, actual));
  }
}

zx_status_t StreamFileConnection::WriteInternal(const void* data, size_t len, size_t* out_actual) {
  FS_PRETTY_TRACE_DEBUG("[FileWrite] options: ", options());
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
    return ZX_ERR_BAD_HANDLE;
  }
  zx_iovec_t vector = {
      .buffer = const_cast<void*>(data),
      .capacity = len,
  };
  zx_status_t status = stream_.writev(0, &vector, 1, out_actual);
  if (status == ZX_OK) {
    ZX_DEBUG_ASSERT(*out_actual <= len);
    vnode()->DidModifyStream();
  }
  return status;
}

void StreamFileConnection::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  size_t actual = 0u;
  zx_status_t status = WriteInternal(request->data.data(), request->data.count(), &actual);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(actual);
  }
}

zx_status_t StreamFileConnection::WriteAtInternal(const void* data, size_t len, size_t offset,
                                                  size_t* out_actual) {
  FS_PRETTY_TRACE_DEBUG("[FileWriteAt] options: ", options());
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
    return ZX_ERR_BAD_HANDLE;
  }
  zx_iovec_t vector = {
      .buffer = const_cast<void*>(data),
      .capacity = len,
  };
  zx_status_t status = stream_.writev_at(0, offset, &vector, 1, out_actual);
  if (status == ZX_OK) {
    ZX_DEBUG_ASSERT(*out_actual <= len);
    vnode()->DidModifyStream();
  }
  return status;
}

void StreamFileConnection::WriteAt(WriteAtRequestView request, WriteAtCompleter::Sync& completer) {
  size_t actual = 0;
  zx_status_t status =
      WriteAtInternal(request->data.data(), request->data.count(), request->offset, &actual);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(actual);
  }
}

void StreamFileConnection::Seek(SeekRequestView request, SeekCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[FileSeek] options: ", options());
  zx_off_t seek = 0u;
  zx_status_t status =
      stream_.seek(static_cast<zx_stream_seek_origin_t>(request->origin), request->offset, &seek);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(seek);
  }
}

void StreamFileConnection::GetFlags(GetFlagsCompleter::Sync& completer) {
  if constexpr (ZX_DEBUG_ASSERT_IMPLEMENTED) {
    // Validate that the connection's append mode and the stream's append mode match.
    uint8_t mode_append;
    if (zx_status_t status = stream_.get_prop_mode_append(&mode_append); status != ZX_OK) {
      completer.Reply(status, {});
      return;
    }
    bool stream_append = static_cast<bool>(mode_append);
    ZX_ASSERT_MSG(stream_append == append(), "stream append: %d flags append: %d", stream_append,
                  append());
  }
  FileConnection::GetFlags(completer);
}

void StreamFileConnection::SetFlags(SetFlagsRequestView request,
                                    SetFlagsCompleter::Sync& completer) {
  bool set_append = static_cast<bool>(request->flags & fio::OpenFlags::kAppend);
  if (set_append != append()) {
    if (zx_status_t status = stream_.set_prop_mode_append(set_append); status != ZX_OK) {
      completer.Reply(status);
      return;
    }
    append() = set_append;
  }
  completer.Reply(ZX_OK);
}

}  // namespace internal

}  // namespace fs
