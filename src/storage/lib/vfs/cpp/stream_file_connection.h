// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_STREAM_FILE_CONNECTION_H_
#define SRC_STORAGE_LIB_VFS_CPP_STREAM_FILE_CONNECTION_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zx/result.h>

#include "src/storage/lib/vfs/cpp/file_connection.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs::internal {

class StreamFileConnection final : public FileConnection {
 public:
  // Refer to documentation for |Connection::Connection|.
  StreamFileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode, fuchsia_io::Rights rights,
                       bool append, zx::stream stream, zx_koid_t koid);

  ~StreamFileConnection() final = default;

 private:
  const zx::stream* stream() const final { return &stream_; }

  //
  // |fuchsia.io/File| operations.
  //

  void Read(ReadRequestView request, ReadCompleter::Sync& completer) final;
  void ReadAt(ReadAtRequestView request, ReadAtCompleter::Sync& completer) final;
  void Write(WriteRequestView request, WriteCompleter::Sync& completer) final;
  void WriteAt(WriteAtRequestView request, WriteAtCompleter::Sync& completer) final;
  void Seek(SeekRequestView request, SeekCompleter::Sync& completer) final;

  //
  // |fuchsia.io/Node| operations.
  //

  void GetFlags(GetFlagsCompleter::Sync& completer) final;
  void SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) final;

  zx_status_t ReadInternal(void* data, size_t len, size_t* out_actual);
  zx_status_t ReadAtInternal(void* data, size_t len, size_t offset, size_t* out_actual);
  zx_status_t WriteInternal(const void* data, size_t len, size_t* out_actual);
  zx_status_t WriteAtInternal(const void* data, size_t len, size_t offset, size_t* out_actual);

  zx::stream stream_;
};

}  // namespace fs::internal

#endif  // SRC_STORAGE_LIB_VFS_CPP_STREAM_FILE_CONNECTION_H_
