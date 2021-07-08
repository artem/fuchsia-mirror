// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <lib/debugdata/debugdata.h>
#include <lib/fdio/io.h>
#include <lib/zx/channel.h>
#include <lib/zx/job.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/status.h>

#include <fbl/string.h>
#include <fbl/unique_fd.h>
#include <fbl/vector.h>

namespace debugdata {

DebugData::DebugData(fbl::unique_fd root_dir_fd) : root_dir_fd_(std::move(root_dir_fd)) {}

void DebugData::Publish(PublishRequestView request, PublishCompleter::Sync&) {
  std::lock_guard<std::mutex> lock(lock_);
  std::string name(request->data_sink.data(), request->data_sink.size());
  data_[name].push_back(std::move(request->data));
  vmo_token_channels_.push_back(request->vmo_token.TakeChannel());
}

std::unordered_map<std::string, std::vector<zx::vmo>> DebugData::TakeData() {
  std::lock_guard<std::mutex> lock(lock_);
  for (zx::channel& channel : vmo_token_channels_) {
    zx_status_t status = channel.wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), nullptr);
    if (status != ZX_OK) {
      fprintf(stderr, "debugdata: error: Unable to wait for token channel to close");
    }
  }
  vmo_token_channels_.clear();
  auto temp = std::move(data_);
  data_ = std::unordered_map<std::string, std::vector<zx::vmo>>();
  return temp;
}

void DebugData::LoadConfig(LoadConfigRequestView request, LoadConfigCompleter::Sync& completer) {
  // When loading debug configuration file, we expect an absolute path.
  const fidl::StringView& config_name = request->config_name;
  if (config_name[0] != '/') {
    // TODO(phosek): Use proper logging mechanism.
    fprintf(stderr, "debugdata: error: LoadConfig: '%.*s' is not an absolute path\n",
            static_cast<int>(config_name.size()), config_name.data());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  fbl::unique_fd fd(openat(root_dir_fd_.get(), config_name.data(), O_RDONLY));
  if (!fd) {
    fprintf(stderr, "debugdata: error: LoadConfig: failed to open '%.*s': %s\n",
            static_cast<int>(config_name.size()), config_name.data(), strerror(errno));
    completer.Close(ZX_ERR_NOT_FOUND);
    return;
  }
  zx::vmo vmo;
  zx_status_t status = fdio_get_vmo_clone(fd.get(), vmo.reset_and_get_address());
  if (status != ZX_OK) {
    fprintf(stderr, "debugdata: error: LoadConfig: failed to load VMO: %s\n",
            zx_status_get_string(status));
    completer.Close(status);
    return;
  }
  vmo.set_property(ZX_PROP_NAME, config_name.data(), config_name.size());
  completer.Reply(std::move(vmo));
}

}  // namespace debugdata
