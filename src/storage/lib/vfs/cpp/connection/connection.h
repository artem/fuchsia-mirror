// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_CONNECTION_CONNECTION_H_
#define SRC_STORAGE_LIB_VFS_CPP_CONNECTION_CONNECTION_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/wire/transaction.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>
#include <lib/zx/event.h>
#include <lib/zx/result.h>
#include <zircon/fidl.h>

#include <memory>

#include <fbl/intrusive_double_list.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs::internal {

// Connection is a base class representing an open connection to a Vnode (the server-side component
// of a file descriptor). It contains the logic to synchronize connection teardown with the vfs, as
// well as shared utilities such as connection cloning and enforcement of connection rights.
// Connections will be managed in a |fbl::DoublyLinkedList|.
//
// This class does not implement any FIDL generated C++ interfaces per se. Rather, each
// |fuchsia.io/{Node, File, Directory, ...}| protocol is handled by a separate corresponding
// subclass, potentially delegating shared functionalities back here.
//
// The Vnode's methods will be invoked in response to FIDL protocol messages received over the
// channel.
//
// This class is thread-safe.
class Connection : public fbl::DoublyLinkedListable<std::unique_ptr<Connection>> {
 public:
  // Closes the connection.
  //
  // The connection must not be destroyed if its wait handler is running concurrently on another
  // thread.
  //
  // In practice, this means the connection must have already been remotely closed, or it must be
  // destroyed on the wait handler's dispatch thread to prevent a race.
  virtual ~Connection();

  using OnUnbound = fit::function<void(Connection*)>;

  // Begins waiting for messages on the channel. |channel| is the channel on which the FIDL protocol
  // will be served. Once called, connections are responsible for closing the underlying vnode.
  //
  // Before calling this function, the connection ownership must be transferred to the Vfs through
  // |RegisterConnection|. Cannot be called more than once in the lifetime of the connection.
  void Bind(zx::channel channel, OnUnbound on_unbound) {
    ZX_DEBUG_ASSERT(channel);
    ZX_DEBUG_ASSERT(vfs()->dispatcher());
    ZX_DEBUG_ASSERT_MSG(InContainer(), "Connection must be managed by Vfs!");
    BindImpl(std::move(channel), std::move(on_unbound));
  }

  // Triggers asynchronous closure of the receiver. Will invoke the |on_unbound| callback passed to
  // |Bind| after unbinding the FIDL server from the channel. Implementations should close the vnode
  // if required once unbinding the server. If not bound or already unbound, has no effect.
  virtual zx::result<> Unbind() = 0;

  // Invokes |handler| with the Representation event for this connection. |query| specifies which
  // attributes, if any, should be included with the event.
  virtual zx::result<> WithRepresentation(
      fit::callback<void(fuchsia_io::wire::Representation)> handler,
      std::optional<fuchsia_io::NodeAttributesQuery> query) const = 0;

  // Invokes |handler| with the NodeInfoDeprecated event for this connection.
  virtual zx::result<> WithNodeInfoDeprecated(
      fit::callback<void(fuchsia_io::wire::NodeInfoDeprecated)> handler) const = 0;

  const fbl::RefPtr<fs::Vnode>& vnode() const { return vnode_; }

 protected:
  // Create a connection bound to a particular vnode.
  //
  // The VFS will be notified when remote side closes the connection.
  //
  // |vfs| is the VFS which is responsible for dispatching operations to the vnode.
  // |vnode| is the vnode which will handle I/O requests.
  // |rights| are the resulting rights for this connection.
  Connection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode, fuchsia_io::Rights rights);

  fuchsia_io::Rights rights() const { return rights_; }

  FuchsiaVfs* vfs() { return vfs_; }

  zx::event& token() { return token_; }

  // Begin waiting for messages on the channel. |channel| is the channel on which the FIDL protocol
  // will be served. Should only be called once per connection.
  virtual void BindImpl(zx::channel channel, OnUnbound on_unbound) = 0;

  // Node operations. Note that these provide the shared implementation of |fuchsia.io/Node|
  // methods, used by all connection subclasses. Use caution when working with FIDL wire types,
  // as certain wire types may reference external data.

  void NodeClone(fuchsia_io::OpenFlags flags, VnodeProtocol protocol,
                 fidl::ServerEnd<fuchsia_io::Node> server_end);
  zx::result<> NodeUpdateAttributes(const VnodeAttributesUpdate& update);
  zx::result<fuchsia_io::wire::FilesystemInfo> NodeQueryFilesystem() const;

 private:
  // The Vfs instance which owns this connection. Connections must not outlive the Vfs, hence this
  // borrowing is safe.
  fs::FuchsiaVfs* const vfs_;

  fbl::RefPtr<fs::Vnode> const vnode_;

  // Rights are hierarchical over Open/Clone. It is never allowed to derive a Connection with more
  // rights than the originating connection.
  fuchsia_io::Rights const rights_;

  // Handle to event which allows client to refer to open vnodes in multi-path operations (see:
  // link, rename). Defaults to ZX_HANDLE_INVALID. Validated on the server-side using cookies.
  zx::event token_;
};

}  // namespace fs::internal

#endif  // SRC_STORAGE_LIB_VFS_CPP_CONNECTION_CONNECTION_H_
