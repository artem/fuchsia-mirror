// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "garnet/bin/sysmgr/package_updating_loader.h"

#include <fcntl.h>
#include <string>
#include <utility>

#include <lib/fit/function.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include "lib/fidl/cpp/optional.h"
#include "lib/fsl/io/fd.h"
#include "lib/fsl/vmo/file.h"
#include "lib/fxl/files/unique_fd.h"
#include "lib/fxl/logging.h"
#include "lib/fxl/strings/substitute.h"
#include "lib/pkg_url/url_resolver.h"
#include "lib/svc/cpp/services.h"

namespace sysmgr {

PackageUpdatingLoader::PackageUpdatingLoader(
    std::unordered_set<std::string> update_dependency_urls,
    fuchsia::amber::ControlPtr amber_ctl, async_dispatcher_t* dispatcher)
    : update_dependency_urls_(std::move(update_dependency_urls)),
      amber_ctl_(std::move(amber_ctl)),
      dispatcher_(dispatcher) {}

PackageUpdatingLoader::~PackageUpdatingLoader() = default;

void PackageUpdatingLoader::LoadUrl(fidl::StringPtr url,
                                    LoadUrlCallback callback) {
  // The updating loader can only update fuchsia-pkg URLs.
  component::FuchsiaPkgUrl fuchsia_url;
  bool parsed = false;
  if (component::FuchsiaPkgUrl::IsFuchsiaPkgScheme(url)) {
    parsed = fuchsia_url.Parse(url);
  } else {
    parsed =
        fuchsia_url.Parse("fuchsia-pkg://fuchsia.com/" + component::GetPathFromURL(url));
  }
  if (!parsed) {
    PackageLoader::LoadUrl(url, callback);
    return;
  }

  auto done_cb = [this, url,
                  callback = std::move(callback)](std::string error) mutable {
    if (!error.empty()) {
      FXL_LOG(ERROR) << "Package update encountered unexpected error \""
                     << error << "\": " << url;
      callback(nullptr);
      return;
    }

    PackageLoader::LoadUrl(url, callback);
  };

  if (std::find(update_dependency_urls_.begin(), update_dependency_urls_.end(),
                url) != std::end(update_dependency_urls_)) {
    // Avoid infinite reentry and cycles: Don't attempt to update the amber or
    // any dependent package. Contacting the amber service may require starting
    // its component or a dependency, which would end up back here.
    done_cb("");
    return;
  }
  StartUpdatePackage(std::move(fuchsia_url), std::move(done_cb));
  return;
}

void PackageUpdatingLoader::StartUpdatePackage(
    const component::FuchsiaPkgUrl url, DoneCallback done_cb) {
  auto cb = [this,
             done_cb = std::move(done_cb)](zx::channel reply_chan) mutable {
    ListenForPackage(std::move(reply_chan), std::move(done_cb));
  };
  // TODO(CF-???): pass variant here
  amber_ctl_->GetUpdateComplete(url.package_name(), "0", nullptr,
                                std::move(cb));
}

namespace {
constexpr int ZXSIO_DAEMON_ERROR = ZX_USER_SIGNAL_0;
}  // namespace

void PackageUpdatingLoader::ListenForPackage(zx::channel reply_chan,
                                             DoneCallback done_cb) {
  async::Wait* wait = new async::Wait(
      reply_chan.release(),
      ZX_CHANNEL_PEER_CLOSED | ZX_CHANNEL_READABLE | ZXSIO_DAEMON_ERROR,
      [done_cb = std::move(done_cb)](async_dispatcher_t* dispatcher,
                                     async::Wait* wait, zx_status_t status,
                                     const zx_packet_signal_t* signal) mutable {
        WaitForUpdateDone(dispatcher, wait, status, signal, std::move(done_cb));
      });
  zx_status_t r = wait->Begin(dispatcher_);
  if (r != ZX_OK) {
    delete wait;
    done_cb(fxl::Substitute("Failed to start waiting for package update: $0",
                            fxl::StringView(zx_status_get_string(r))));
  }
}

// static
void PackageUpdatingLoader::WaitForUpdateDone(async_dispatcher_t* dispatcher,
                                              async::Wait* wait,
                                              zx_status_t status,
                                              const zx_packet_signal_t* signal,
                                              DoneCallback done_cb) {
  if (status == ZX_OK && (signal->observed & ZXSIO_DAEMON_ERROR)) {
    // Daemon signalled an error, wait for its error message.
    const zx_handle_t reply_chan = wait->object();
    delete wait;
    wait = new async::Wait(
        reply_chan, ZX_CHANNEL_PEER_CLOSED | ZX_CHANNEL_READABLE,
        [done_cb = std::move(done_cb)](
            async_dispatcher_t* dispatcher, async::Wait* wait,
            zx_status_t status, const zx_packet_signal_t* signal) mutable {
          FinishWaitForUpdate(dispatcher, wait, status, signal, true,
                              std::move(done_cb));
        });
    zx_status_t r = wait->Begin(dispatcher);
    if (r != ZX_OK) {
      delete wait;
      done_cb(fxl::Substitute("Failed to start waiting for package update: $0",
                              fxl::StringView(zx_status_get_string(r))));
    }
    return;
  }
  FinishWaitForUpdate(dispatcher, wait, status, signal, false,
                      std::move(done_cb));
}

// static
void PackageUpdatingLoader::FinishWaitForUpdate(
    async_dispatcher_t* dispatcher, async::Wait* wait, zx_status_t status,
    const zx_packet_signal_t* signal, bool daemon_err, DoneCallback done_cb) {
  const zx_handle_t reply_chan = wait->object();
  delete wait;
  if (status != ZX_OK) {
    done_cb(fxl::Substitute("Failed waiting for package update: $0",
                            fxl::StringView(zx_status_get_string(status))));
    return;
  }
  if (status == ZX_OK && (signal->observed & ZX_CHANNEL_READABLE)) {
    // Read response from channel.
    uint8_t bytes[ZX_CHANNEL_MAX_MSG_BYTES];
    uint32_t actual_bytes, actual_handles;
    zx_status_t r = zx_channel_read(
        reply_chan, 0, bytes, /*handles=*/nullptr, ZX_CHANNEL_MAX_MSG_BYTES,
        /*num_handles=*/0, &actual_bytes, &actual_handles);
    if (r != ZX_OK) {
      done_cb(fxl::Substitute("Error reading response from channel: $0",
                              fxl::StringView(zx_status_get_string(r))));
      return;
    }
    bytes[actual_bytes] = '\0';
    if (daemon_err) {
      // If the package daemon reported an error (for example, maybe it could
      // not access the remote server), log a warning but allow the stale
      // package to be loaded.
      FXL_VLOG(1) << "Package update failed. Loading package without "
                  << "update. Error: " << bytes;
    }
    done_cb("");
  } else if (status == ZX_OK && (signal->observed & ZX_CHANNEL_PEER_CLOSED)) {
    done_cb("Update response channel closed unexpectedly.");
  } else {
    done_cb(fxl::Substitute("Waiting for update failed: $0",
                            fxl::StringView(zx_status_get_string(status))));
  }
}

}  // namespace sysmgr
