// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/remote_file.h"

#include <fidl/fuchsia.io/cpp/wire.h>

#include <utility>

#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace fio = fuchsia_io;

namespace fs {

RemoteFile::RemoteFile(fidl::ClientEnd<fio::Directory> remote_client)
    : remote_client_(std::move(remote_client)) {
  ZX_DEBUG_ASSERT(remote_client_);
}

RemoteFile::~RemoteFile() = default;

fuchsia_io::NodeProtocolKinds RemoteFile::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kFile;
}

zx_status_t RemoteFile::GetAttributes(VnodeAttributes* attr) {
  *attr = VnodeAttributes();
  attr->mode = V_TYPE_FILE | V_IRUSR;
  attr->inode = fio::wire::kInoUnknown;
  attr->link_count = 1;
  return ZX_OK;
}

bool RemoteFile::IsRemote() const { return true; }

zx_status_t RemoteFile::OpenRemote(fio::OpenFlags flags, fio::ModeType mode, fidl::StringView path,
                                   fidl::ServerEnd<fio::Node> object) const {
  return fidl::WireCall(remote_client_)->Open(flags, mode, path, std::move(object)).status();
}
}  // namespace fs
