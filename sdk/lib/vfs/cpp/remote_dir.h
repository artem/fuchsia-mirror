// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_REMOTE_DIR_H_
#define LIB_VFS_CPP_REMOTE_DIR_H_

#include <lib/vfs/cpp/internal/directory.h>

namespace vfs {

// A remote directory holds a channel to a remotely hosted directory to
// which requests are delegated when opened.
//
// This class is designed to allow programs to publish remote filesystems
// as directories without requiring a separate "mount" step.  In effect,
// a remote directory is "mounted" at creation time.
//
// It is not possible for the client to detach the remote directory or
// to mount a new one in its place.
class RemoteDir final : public vfs::internal::Directory {
 public:
  // Binds to a remotely hosted directory using the specified
  // |fuchsia.io.Directory| client channel endpoint.The channel must be valid.
  explicit RemoteDir(zx::channel remote_dir);

  // Binds to a remotely hosted directory using the specified
  // InterfaceHandle. Handle must be valid.
  explicit RemoteDir(fidl::InterfaceHandle<fuchsia::io::Directory> dir);

  ~RemoteDir() override;

 protected:
  void OpenRemote(fuchsia::io::OpenFlags flags, fuchsia::io::ModeType mode, std::string_view,
                  fidl::InterfaceRequest<fuchsia::io::Node> request) override;

  bool IsRemote() const override;

  // |Node| implementation
  zx_status_t Connect(fuchsia::io::OpenFlags flags, zx::channel request,
                      async_dispatcher_t* dispatcher) final;

  // |Directory| implementation
  zx_status_t Readdir(uint64_t offset, void* data, uint64_t len, uint64_t* out_offset,
                      uint64_t* out_actual) final;

 private:
  fuchsia::io::DirectorySyncPtr dir_ptr_;
};

}  // namespace vfs

#endif  // LIB_VFS_CPP_REMOTE_DIR_H_
