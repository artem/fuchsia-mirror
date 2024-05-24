// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_CONNECTION_FILE_CONNECTION_H_
#define SRC_STORAGE_LIB_VFS_CPP_CONNECTION_FILE_CONNECTION_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include <fidl/fuchsia.io/cpp/wire.h>
#include <zircon/availability.h>

#include <cstdint>

#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/connection/connection.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs::internal {

class FileConnection : public Connection, public fidl::WireServer<fuchsia_io::File> {
 public:
  // Refer to documentation for |Connection::Connection|.
  FileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode, fuchsia_io::Rights rights,
                 bool append, zx_koid_t koid);

  ~FileConnection() override;

 protected:
  bool& append() { return append_; }
  bool append() const { return append_; }
  virtual const zx::stream* stream() const { return nullptr; }

  //
  // |fs::Connection| Implementation
  //

  void BindImpl(zx::channel channel, OnUnbound on_unbound) final;
  zx::result<> Unbind() final;
  zx::result<> WithRepresentation(fit::callback<void(fuchsia_io::wire::Representation)> handler,
                                  std::optional<fuchsia_io::NodeAttributesQuery> query) const final;
  zx::result<> WithNodeInfoDeprecated(
      fit::callback<void(fuchsia_io::wire::NodeInfoDeprecated)> handler) const final;

  //
  // |fuchsia.io/Node| operations.
  //

  void Clone(CloneRequestView request, CloneCompleter::Sync& completer) final;
  void Close(CloseCompleter::Sync& completer) final;
  void Query(QueryCompleter::Sync& completer) final;
  void GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) final;
  void Sync(SyncCompleter::Sync& completer) final;
  void GetAttr(GetAttrCompleter::Sync& completer) final;
  void SetAttr(SetAttrRequestView request, SetAttrCompleter::Sync& completer) final;
  void GetFlags(GetFlagsCompleter::Sync& completer) override;
  void SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) override;
  void QueryFilesystem(QueryFilesystemCompleter::Sync& completer) final;
  void GetAttributes(fuchsia_io::wire::Node2GetAttributesRequest* request,
                     GetAttributesCompleter::Sync& completer) final;
  void UpdateAttributes(fuchsia_io::wire::MutableNodeAttributes* request,
                        UpdateAttributesCompleter::Sync& completer) final;
  void Reopen(fuchsia_io::wire::Node2ReopenRequest* request,
              ReopenCompleter::Sync& completer) final {
    request->object_request.Close(ZX_ERR_NOT_SUPPORTED);
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  void ListExtendedAttributes(ListExtendedAttributesRequestView request,
                              ListExtendedAttributesCompleter::Sync& completer) final {
    request->iterator.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetExtendedAttribute(GetExtendedAttributeRequestView request,
                            GetExtendedAttributeCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void SetExtendedAttribute(SetExtendedAttributeRequestView request,
                            SetExtendedAttributeCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void RemoveExtendedAttribute(RemoveExtendedAttributeRequestView request,
                               RemoveExtendedAttributeCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void LinkInto(fuchsia_io::wire::LinkableLinkIntoRequest* request,
                LinkIntoCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
#endif

  //
  // |fuchsia.io/File| operations.
  //

  void Describe(DescribeCompleter::Sync& completer) final;
  void Resize(ResizeRequestView request, ResizeCompleter::Sync& completer) final;
  void GetBackingMemory(GetBackingMemoryRequestView request,
                        GetBackingMemoryCompleter::Sync& completer) final;
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  void Allocate(AllocateRequestView request, AllocateCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void EnableVerity(EnableVerityRequestView request, EnableVerityCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
#endif

  //
  // |fuchsia.io/AdvisoryLocking| operations.
  //

  void AdvisoryLock(fidl::WireServer<fuchsia_io::File>::AdvisoryLockRequestView request,
                    AdvisoryLockCompleter::Sync& _completer) final;

  zx_status_t ResizeInternal(uint64_t length);
  zx_status_t GetBackingMemoryInternal(fuchsia_io::wire::VmoFlags flags, zx::vmo* out_vmo);

 private:
  std::optional<fidl::ServerBindingRef<fuchsia_io::File>> binding_;
  const zx_koid_t koid_;
  bool append_;
};

}  // namespace fs::internal

#endif  // SRC_STORAGE_LIB_VFS_CPP_CONNECTION_FILE_CONNECTION_H_
