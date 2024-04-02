// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/volume_image/adapter/commands/file_client.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fdio/directory.h>

zx::result<fidl::ClientEnd<fuchsia_io::File>> OpenFile(const char* path) {
  auto [client, server] = fidl::Endpoints<fuchsia_io::File>::Create();
  return zx::make_result(
      fdio_open(path,
                static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kRightReadable |
                                      fuchsia_io::wire::OpenFlags::kNotDirectory),
                server.TakeChannel().release()),
      std::move(client));
}
