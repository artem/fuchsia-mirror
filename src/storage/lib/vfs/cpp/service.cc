// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/service.h"

#include <fidl/fuchsia.io/cpp/wire.h>

#include <utility>

#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace fio = fuchsia_io;

namespace fs {

Service::Service(Connector connector) : connector_(std::move(connector)) {}

Service::~Service() = default;

fuchsia_io::NodeProtocolKinds Service::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kConnector;
}

zx_status_t Service::GetAttributes(VnodeAttributes* attr) {
  // TODO(https://fxbug.dev/42106031): V_TYPE_FILE isn't right, we should use a type for services
  *attr = VnodeAttributes();
  attr->mode = V_TYPE_FILE;
  attr->inode = fio::wire::kInoUnknown;
  attr->link_count = 1;
  return ZX_OK;
}

zx_status_t Service::ConnectService(zx::channel channel) {
  if (!connector_) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  return connector_(std::move(channel));
}

}  // namespace fs
