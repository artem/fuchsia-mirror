// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/connection/file_connection.h"

#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/zx/handle.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <memory>
#include <utility>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/connection/advisory_lock.h"
#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs::internal {

FileConnection::FileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                               fuchsia_io::Rights rights, bool append, zx_koid_t koid)
    : Connection(vfs, std::move(vnode), rights), koid_(koid), append_(append) {
  // Ensure the VFS does not create connections that have privileges which cannot be used.
  ZX_DEBUG_ASSERT(internal::DownscopeRights(rights, VnodeProtocol::kFile) == rights);
}

FileConnection::~FileConnection() {
  [[maybe_unused]] zx::result result = Unbind();
  vnode()->DeleteFileLockInTeardown(koid_);
}

void FileConnection::BindImpl(zx::channel channel, OnUnbound on_unbound) {
  ZX_DEBUG_ASSERT(!binding_);
  binding_.emplace(fidl::BindServer(vfs()->dispatcher(),
                                    fidl::ServerEnd<fuchsia_io::File>{std::move(channel)}, this,
                                    [on_unbound = std::move(on_unbound)](
                                        FileConnection* self, fidl::UnbindInfo,
                                        fidl::ServerEnd<fuchsia_io::File>) { on_unbound(self); }));
}

zx::result<> FileConnection::Unbind() {
  if (std::optional binding = std::exchange(binding_, std::nullopt); binding) {
    binding->Unbind();
    return zx::make_result(vnode()->Close());
  }
  return zx::ok();
}

void FileConnection::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  fio::OpenFlags inherited_flags = {};
  // The APPEND flag should be preserved when cloning a file connection.
  if (append()) {
    inherited_flags |= fio::OpenFlags::kAppend;
  }
  Connection::NodeClone(request->flags | inherited_flags, VnodeProtocol::kFile,
                        std::move(request->object));
}

void FileConnection::Close(CloseCompleter::Sync& completer) { completer.Reply(Unbind()); }

void FileConnection::Query(QueryCompleter::Sync& completer) {
  std::string_view protocol = fio::kFileProtocolName;
  // TODO(https://fxbug.dev/42052765): avoid the const cast.
  uint8_t* data = reinterpret_cast<uint8_t*>(const_cast<char*>(protocol.data()));
  completer.Reply(fidl::VectorView<uint8_t>::FromExternal(data, protocol.size()));
}

zx::result<> FileConnection::WithNodeInfoDeprecated(
    fit::callback<void(fuchsia_io::wire::NodeInfoDeprecated)> handler) const {
  fio::wire::FileObject file_object;
  zx::result<zx::event> observer = vnode()->GetObserver();
  if (observer.is_ok()) {
    file_object.event = std::move(*observer);
  } else if (observer.error_value() != ZX_ERR_NOT_SUPPORTED) {
    return observer.take_error();
  }
  if (stream()) {
    if (zx_status_t status = stream()->duplicate(ZX_RIGHT_SAME_RIGHTS, &file_object.stream);
        status != ZX_OK) {
      return zx::error(status);
    }
  }
  handler(fuchsia_io::wire::NodeInfoDeprecated::WithFile(
      fidl::ObjectView<fio::wire::FileObject>::FromExternal(&file_object)));
  return zx::ok();
}

zx::result<> FileConnection::WithRepresentation(
    fit::callback<void(fuchsia_io::wire::Representation)> handler,
    std::optional<fuchsia_io::NodeAttributesQuery> query) const {
  // TODO(https://fxbug.dev/324112857): Handle |query|.
  if (query) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  fidl::WireTableFrame<fuchsia_io::wire::FileInfo> frame;
  auto builder = fuchsia_io::wire::FileInfo::ExternalBuilder(
      fidl::ObjectView<fidl::WireTableFrame<fuchsia_io::wire::FileInfo>>::FromExternal(&frame));
  builder.is_append(append());
  zx::result<zx::event> observer = vnode()->GetObserver();
  if (observer.is_ok()) {
    builder.observer(std::move(*observer));
  } else if (observer.error_value() != ZX_ERR_NOT_SUPPORTED) {
    return observer.take_error();
  }
  if (stream()) {
    zx::stream stream;
    if (zx_status_t status = this->stream()->duplicate(ZX_RIGHT_SAME_RIGHTS, &stream);
        status != ZX_OK) {
      return zx::error(status);
    }
    builder.stream(std::move(stream));
  }
  auto info = builder.Build();
  auto representation = fuchsia_io::wire::Representation::WithFile(
      fidl::ObjectView<fuchsia_io::wire::FileInfo>::FromExternal(&info));
  handler(std::move(representation));
  return zx::ok();
}

void FileConnection::Describe(DescribeCompleter::Sync& completer) {
  zx::result sent_describe = WithRepresentation(
      [&](fio::wire::Representation representation) {
        ZX_DEBUG_ASSERT(representation.is_file() && representation.file().has_is_append());
        completer.Reply(representation.file());
      },
      std::nullopt);
  if (sent_describe.is_error()) {
    completer.Close(sent_describe.error_value());
    return;
  }
}

void FileConnection::GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) {
  fidl::Arena arena;
  completer.Reply(fio::wire::ConnectionInfo::Builder(arena).rights(rights()).Build());
}

void FileConnection::Sync(SyncCompleter::Sync& completer) {
  vnode()->Sync([completer = completer.ToAsync()](zx_status_t sync_status) mutable {
    if (sync_status != ZX_OK) {
      completer.ReplyError(sync_status);
    } else {
      completer.ReplySuccess();
    }
  });
}

void FileConnection::GetAttr(GetAttrCompleter::Sync& completer) {
  zx::result attrs = vnode()->GetAttributes();
  if (attrs.is_ok()) {
    completer.Reply(ZX_OK, attrs->ToIoV1NodeAttributes(*vnode()));
  } else {
    completer.Reply(attrs.error_value(), fio::wire::NodeAttributes());
  }
}

void FileConnection::SetAttr(SetAttrRequestView request, SetAttrCompleter::Sync& completer) {
  VnodeAttributesUpdate update =
      VnodeAttributesUpdate::FromIo1(request->attributes, request->flags);
  completer.Reply(Connection::NodeUpdateAttributes(update).status_value());
}

void FileConnection::GetAttributes(fio::wire::Node2GetAttributesRequest* request,
                                   GetAttributesCompleter::Sync& completer) {
  internal::NodeAttributeBuilder builder;
  zx::result attrs = builder.Build(*vnode(), request->query);
  completer.Reply(zx::make_result(attrs.status_value(), attrs.is_ok() ? &*attrs : nullptr));
}

void FileConnection::UpdateAttributes(fio::wire::MutableNodeAttributes* request,
                                      UpdateAttributesCompleter::Sync& completer) {
  VnodeAttributesUpdate update = VnodeAttributesUpdate::FromIo2(*request);
  completer.Reply(Connection::NodeUpdateAttributes(update));
}

void FileConnection::GetFlags(GetFlagsCompleter::Sync& completer) {
  fio::OpenFlags flags = {};
  if (rights() & fio::Rights::kReadBytes) {
    flags |= fio::OpenFlags::kRightReadable;
  }
  if (rights() & fio::Rights::kWriteBytes) {
    flags |= fio::OpenFlags::kRightWritable;
  }
  if (rights() & fio::Rights::kExecute) {
    flags |= fio::OpenFlags::kRightExecutable;
  }
  if (append()) {
    flags |= fio::OpenFlags::kAppend;
  }
  completer.Reply(ZX_OK, flags);
}

void FileConnection::SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) {
  append() = static_cast<bool>(request->flags & fio::OpenFlags::kAppend);
  completer.Reply(ZX_OK);
}

void FileConnection::QueryFilesystem(QueryFilesystemCompleter::Sync& completer) {
  zx::result result = Connection::NodeQueryFilesystem();
  completer.Reply(result.status_value(),
                  result.is_ok()
                      ? fidl::ObjectView<fio::wire::FilesystemInfo>::FromExternal(&result.value())
                      : nullptr);
}

zx_status_t FileConnection::ResizeInternal(uint64_t length) {
  FS_PRETTY_TRACE_DEBUG("[FileTruncate] rights: ", rights(), ", append: ", append());
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
    return ZX_ERR_BAD_HANDLE;
  }
  return vnode()->Truncate(length);
}

void FileConnection::Resize(ResizeRequestView request, ResizeCompleter::Sync& completer) {
  zx_status_t result = ResizeInternal(request->length);
  if (result != ZX_OK) {
    completer.ReplyError(result);
  } else {
    completer.ReplySuccess();
  }
}

zx_status_t FileConnection::GetBackingMemoryInternal(fio::wire::VmoFlags flags, zx::vmo* out_vmo) {
  if ((flags & fio::VmoFlags::kPrivateClone) && (flags & fio::VmoFlags::kSharedBuffer)) {
    return ZX_ERR_INVALID_ARGS;
  }
  if ((flags & fio::VmoFlags::kRead) && !(rights() & fio::Rights::kReadBytes)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((flags & fio::VmoFlags::kWrite) && !(rights() & fio::Rights::kWriteBytes)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((flags & fio::VmoFlags::kExecute) && !(rights() & fio::Rights::kExecute)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  return vnode()->GetVmo(flags, out_vmo);
}

void FileConnection::GetBackingMemory(GetBackingMemoryRequestView request,
                                      GetBackingMemoryCompleter::Sync& completer) {
  zx::vmo vmo;
  zx_status_t status = GetBackingMemoryInternal(request->flags, &vmo);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(std::move(vmo));
  }
}

void FileConnection::AdvisoryLock(fidl::WireServer<fio::File>::AdvisoryLockRequestView request,
                                  AdvisoryLockCompleter::Sync& completer) {
  // advisory_lock replies to the completer
  auto async_completer = completer.ToAsync();
  fit::callback<void(zx_status_t)> callback = file_lock::lock_completer_t(
      [lock_completer = std::move(async_completer)](zx_status_t status) mutable {
        lock_completer.ReplyError(status);
      });

  advisory_lock(koid_, vnode(), true, request->request, std::move(callback));
}

}  // namespace fs::internal
