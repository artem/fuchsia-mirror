// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_FILE_CONNECTION_H_
#define SRC_STORAGE_LIB_VFS_CPP_FILE_CONNECTION_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include <fidl/fuchsia.io/cpp/wire.h>

#include <cstdint>

#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/connection.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs::internal {

class FileConnection : public Connection, public fidl::WireServer<fuchsia_io::File> {
 public:
  // Refer to documentation for |Connection::Connection|.
  FileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode, VnodeProtocol protocol,
                 VnodeConnectionOptions options, zx_koid_t koid);

  ~FileConnection() override;

  zx::result<VnodeRepresentation> NodeGetRepresentation() const override;

 private:
  std::unique_ptr<Binding> Bind(async_dispatcher*, zx::channel, OnUnbound) override;

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
  void QueryFilesystem(QueryFilesystemCompleter::Sync& completer) final;
  void GetAttributes(fuchsia_io::wire::Node2GetAttributesRequest* request,
                     GetAttributesCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void UpdateAttributes(fuchsia_io::wire::MutableNodeAttributes* request,
                        UpdateAttributesCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void Reopen(fuchsia_io::wire::Node2ReopenRequest* request,
              ReopenCompleter::Sync& completer) final {
    request->object_request.Close(ZX_ERR_NOT_SUPPORTED);
  }
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  void Allocate(AllocateRequestView request, AllocateCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
#endif

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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

  const zx_koid_t koid_;
};

}  // namespace fs::internal

#endif  // SRC_STORAGE_LIB_VFS_CPP_FILE_CONNECTION_H_
