// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/vfs.h"

#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include <string_view>
#include <utility>

#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {
namespace {

// Traverse the directory tree starting at |vndir| until the last component in |path|, or until a
// remote mount point is encountered. Both |path| and |vndir| are updated in-place.
//
// On success, |path| will be the canonical name for the entry to lookup within |vndir|. Note that
// on Fuchsia, the dot path (".") is used as the canonical form for a reference to |vndir| itself.
//
// See https://fxbug.dev/42103076 for a discussion on this mapping.
zx_status_t Traverse(fbl::RefPtr<Vnode>& vndir, std::string_view& path) {
  if (path.empty() || path.length() >= fio::kMaxPathLength) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Handle "." and "/", ensuring we map the latter to the canonical form used by the VFS.
  if (path == "." || path == "/") {
    path = ".";
    return ZX_OK;
  }

  // Allow a single leading '/'.
  if (path[0] == '/') {
    path = path.substr(1);
  }
  // Allow trailing '/', but only if preceded by something.
  if (path.length() > 1 && path.back() == '/') {
    path = path.substr(0, path.length() - 1);
  }

  while (!path.empty()) {
    // If we hit a remote mount point, the caller must forward the remainder of |path| there.
    if (vndir->IsRemote()) {
      return ZX_OK;
    }
    // Look for the next '/' separated path component.
    size_t slash = path.find('/');
    std::string_view component = path.substr(0, slash);
    if (component.length() > fio::kMaxNameLength) {
      return ZX_ERR_BAD_PATH;  // Maps to ENAMETOOLONG
    }
    if (component.empty() || component == "." || component == "..") {
      // Clients are required to transform paths into their canonical form.
      return ZX_ERR_INVALID_ARGS;
    }
    // Stop traversal if this is the last component (e.g. there are no remaining '/' separators).
    if (slash == std::string_view::npos) {
      return ZX_OK;
    }
    // Traverse to the next component, updating |vndir| and |path| in-place.
    fbl::RefPtr<fs::Vnode> next_vn;
    if (zx_status_t status = vndir->Lookup(component, &next_vn); status != ZX_OK) {
      return status;
    }
    vndir = std::move(next_vn);
    path = path.substr(slash + 1);
  }
  return ZX_ERR_INVALID_ARGS;
}

zx::result<CreationType> GetCreationType(const fio::wire::NodeProtocols& protocols) {
  // It's an error to specify more than one protocol when trying to create an object.
  std::optional<CreationType> type;
  if (protocols.has_file()) {
    type = CreationType::kFile;
  }
  if (protocols.has_directory()) {
    if (type) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    type = CreationType::kDirectory;
  }
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (protocols.has_symlink()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  if (protocols.has_node()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
#endif
  if (!type) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  return zx::ok(*type);
}

}  // namespace

Vfs::Vfs() = default;

Vfs::OpenResult Vfs::Open(fbl::RefPtr<Vnode> vndir, std::string_view path,
                          VnodeConnectionOptions options, fuchsia_io::Rights connection_rights) {
  FS_PRETTY_TRACE_DEBUG("Vfs::Open: path='", path, "' options=", options,
                        ", connection_rights=", connection_rights);
  std::lock_guard lock(vfs_lock_);
  // Traverse directory tree until last component, updating |vndir| and |path| in-place.
  if (zx_status_t status = Traverse(vndir, path); status != ZX_OK) {
    return status;
  }
  if (vndir->IsRemote()) {
    // remote filesystem, return handle and path to caller
    return OpenResult::Remote{.vnode = std::move(vndir), .path = path};
  }
  // |Traverse()| should guarantee |path| is only a single and valid component.
  ZX_DEBUG_ASSERT(!path.empty() && path.find('/') == std::string_view::npos && path != "..");

  fbl::RefPtr<Vnode> vn;
  bool vn_is_open;
  {
    CreationMode mode = internal::CreationModeFromFidl(options.flags);
    std::optional<CreationType> type = std::nullopt;
    if (mode != CreationMode::kNever) {
      type = options.flags & fio::OpenFlags::kDirectory ? CreationType::kDirectory
                                                        : CreationType::kFile;
    }
    zx::result result = CreateOrLookup(std::move(vndir), path, mode, type, connection_rights);
    if (result.is_error()) {
      return result.error_value();
    }
    std::tie(vn, vn_is_open) = *std::move(result);
  }

  if (vn->IsRemote()) {
    // Opening a mount point: Traverse across remote.
    return OpenResult::Remote{.vnode = std::move(vn), .path = "."};
  }

  if (ReadonlyLocked() && (options.rights & fio::Rights::kWriteBytes) &&
      !vn->Supports(fuchsia_io::NodeProtocolKinds::kConnector)) {
    return ZX_ERR_ACCESS_DENIED;
  }

  if ((options.flags & (fio::OpenFlags::kPosixWritable | fio::OpenFlags::kPosixExecutable)) &&
      vn->Supports(fuchsia_io::NodeProtocolKinds::kDirectory)) {
    // This is such that POSIX open() can open a directory with O_RDONLY, and still get the
    // write/execute right if the parent directory connection has the write/execute right
    // respectively.  With the execute right in particular, the resulting connection may be passed
    // to fdio_get_vmo_exec() which requires the execute right. This transfers write and execute
    // from the parent, if present.
    fuchsia_io::Rights inheritable_rights;
    if (options.flags & fuchsia_io::OpenFlags::kPosixWritable) {
      inheritable_rights |= fuchsia_io::kWStarDir;
    }
    if (options.flags & fuchsia_io::OpenFlags::kPosixExecutable) {
      inheritable_rights |= fuchsia_io::kXStarDir;
    }
    options.rights |= connection_rights & inheritable_rights;
  }
  if (zx::result validated = vn->ValidateOptions(options); validated.is_error()) {
    return validated.error_value();
  }

  // |node_reference| requests that we don't actually open the underlying Vnode, but use the
  // connection as a reference to the Vnode.
  if (!(options.flags & fuchsia_io::OpenFlags::kNodeReference) && !vn_is_open) {
    if (zx_status_t status = OpenVnode(&vn); status != ZX_OK) {
      return status;
    }

    if (vn->IsRemote()) {
      // |OpenVnode| redirected us to a remote vnode; traverse across mount point.
      return OpenResult::Remote{.vnode = std::move(vn), .path = "."};
    }

    if (options.flags & fuchsia_io::OpenFlags::kTruncate) {
      if (zx_status_t status = vn->Truncate(0); status != ZX_OK) {
        vn->Close();
        return status;
      }
    }
  }

  return OpenResult::Ok{.vnode = std::move(vn), .options = options};
}

zx::result<Vfs::Open2Result> Vfs::Open2(fbl::RefPtr<Vnode> vndir, std::string_view path,
                                        const fuchsia_io::wire::ConnectionProtocols& protocols,
                                        fuchsia_io::Rights connection_rights)
    __TA_EXCLUDES(vfs_lock_) {
  FS_PRETTY_TRACE_DEBUG("Vfs::Open2: path: '", path, "', protocols: ", protocols,
                        ", rights: ", connection_rights);

  const fio::wire::NodeOptions* node_options = protocols.is_node() ? &protocols.node() : nullptr;
  const fio::wire::NodeProtocols* node_protocols =
      (node_options && node_options->has_protocols()) ? &node_options->protocols() : nullptr;
  const fio::wire::MutableNodeAttributes* creation_attributes = nullptr;
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
  creation_attributes = node_options && node_options->has_create_attributes()
                            ? &node_options->create_attributes()
                            : nullptr;
#endif

  // If the request is for this directory, ensure the request is valid.
  if (path == "." || path == "/") {
    if (!node_options) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    if (node_protocols) {
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
      if (!(node_protocols->has_directory() || node_protocols->has_node())) {
#else
      if (!node_protocols->has_directory()) {
#endif
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }

  const bool should_truncate = node_protocols && node_protocols->has_file() &&
                               (node_protocols->file() & fio::FileProtocolFlags::kTruncate);
  if (should_truncate && !(connection_rights & fio::Rights::kWriteBytes)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::lock_guard lock(vfs_lock_);
  // Traverse directory tree until last component, updating |vndir| and |path| in-place.
  if (zx_status_t status = Traverse(vndir, path); status != ZX_OK) {
    return zx::error(status);
  }
  if (vndir->IsRemote()) {
    // If we encountered a remote filesystem, forward the remainder of the request there.
    return Open2Result::Remote(std::move(vndir), path);
  }
  // |Traverse()| should guarantee |path| is only a single and valid component.
  ZX_DEBUG_ASSERT(!path.empty() && path.find('/') == std::string_view::npos && path != "..");

  // Try to open or create the object at |path| inside |vndir|.
  fbl::RefPtr<Vnode> vn;
  bool vn_is_open;
  {
    CreationMode mode = node_options && node_options->has_mode()
                            ? internal::CreationModeFromFidl(node_options->mode())
                            : CreationMode::kNever;
    std::optional<CreationType> type;
    if (mode != CreationMode::kNever) {
      zx::result result = GetCreationType(*node_protocols);
      if (result.is_error()) {
        return result.take_error();
      }
      type = *result;
    }
    // It's an error to specify create attributes when opening an existing object.
    if (mode == CreationMode::kNever && creation_attributes) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    // *NOTE*: All filesystems which allow setting mutable attributes currently support the *same*
    // set of attributes for all object types. Thus, we use the parent directory to verify that the
    // request is valid.  This assumption is verified afterwards with a debug assert.
    //
    // If a filesystem supports different attributes for files vs. directories, for example, we will
    // need to  support atomic creation of objects with attributes in the |Vnode| interface.
    std::optional<VnodeAttributesUpdate> new_attrs;
    if (creation_attributes) {
      new_attrs = VnodeAttributesUpdate::FromIo2(*creation_attributes);
      if (new_attrs->Query() - vndir->SupportedMutableAttributes()) {
        return zx::error(ZX_ERR_NOT_SUPPORTED);
      }
    }
    // Create or lookup the Vnode.
    zx::result result = CreateOrLookup(std::move(vndir), path, mode, type, connection_rights);
    if (result.is_error()) {
      return result.take_error();
    }
    std::tie(vn, vn_is_open) = *std::move(result);
    // If we created a new Vnode, set any attributes specified with the request.
    if (vn_is_open && new_attrs) {
      ZX_DEBUG_ASSERT(!(new_attrs->Query() - vn->SupportedMutableAttributes()));
      zx::result result = vn->UpdateAttributes(*new_attrs);
      ZX_DEBUG_ASSERT_MSG(result.is_ok(), "Updating attributes on a new vnode should never fail!");
      if (result.is_error()) {
        return result.take_error();
      }
    }
  }

  if (vn->IsRemote()) {
    // Opening a mount point: Traverse across remote.
    return Open2Result::Remote(std::move(vn), ".");
  }

  if (ReadonlyLocked() && (connection_rights & fs::kAllMutableIo2Rights)) {
    FS_PRETTY_TRACE_DEBUG("Vfs::Open2: Rights incompatible, filesystem is read-only.");
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  zx::result protocol =
      internal::NegotiateProtocol(vn->GetProtocols(), internal::GetProtocols(protocols));
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
  // If we couldn't negotiate a supported protocol, see if we can fall back to a node connection.
  if (protocol.is_error() && protocols.is_node() &&
      (!node_protocols || node_protocols->has_node())) {
    if ((node_protocols->node() & fio::NodeProtocolFlags::kMustBeDirectory) &&
        !vn->Supports(fio::NodeProtocolKinds::kDirectory)) {
      return zx::error(ZX_ERR_NOT_DIR);
    }
    protocol = zx::ok(VnodeProtocol::kNode);
  }
#endif
  if (protocol.is_error()) {
    return protocol.take_error();
  }

  fuchsia_io::Rights rights =
      node_options && node_options->has_rights() ? node_options->rights() : fuchsia_io::Rights(0);
  if (!vn->ValidateRights(rights)) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  // If the Vnode is already opened (e.g. it was just created) we can return early.
  if (vn_is_open) {
    return Open2Result::Local(std::move(vn), *protocol);
  }

  zx::result opened_node = Open2Result::OpenVnode(std::move(vn), *protocol);
  // If we opened the node as a file, truncate it if required.
  if (opened_node.is_ok() && opened_node->protocol() == VnodeProtocol::kFile && should_truncate) {
    if (zx_status_t status = opened_node->vnode()->Truncate(0); status != ZX_OK) {
      return zx::error(status);
    }
  }
  return opened_node;
}

zx_status_t Vfs::Unlink(fbl::RefPtr<Vnode> vndir, std::string_view name, bool must_be_dir) {
  {
    std::lock_guard lock(vfs_lock_);
    if (ReadonlyLocked()) {
      return ZX_ERR_ACCESS_DENIED;
    }
    if (zx_status_t status = vndir->Unlink(name, must_be_dir); status != ZX_OK) {
      return status;
    }
  }
  return ZX_OK;
}

zx::result<std::tuple<fbl::RefPtr<Vnode>, /*vnode_is_open*/ bool>> Vfs::CreateOrLookup(
    fbl::RefPtr<fs::Vnode> vndir, std::string_view name, CreationMode mode,
    std::optional<CreationType> type, fio::Rights connection_rights) {
  // If the request requires we create an object, ensure the VFS isn't in read-only mode.
  if (mode != CreationMode::kNever && ReadonlyLocked()) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }
  // If |name| points to this directory, just return |vndir|.
  if (name == ".") {
    if (mode == CreationMode::kAlways) {
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    return zx::ok(std::tuple{std::move(vndir), false});
  }
  // Try to create a new object if the request requires it.
  switch (mode) {
    case CreationMode::kAllowExisting:
    case CreationMode::kAlways: {
      ZX_DEBUG_ASSERT(type.has_value());
      // Try to create a new object and notify any watchers if one was added. Note that
      // |Vnode::Create()| ensures the returned object has already been opened on success.
      zx::result created = vndir->Create(name, *type);
      if (created.is_ok()) {
        vndir->Notify(name, fio::WatchEvent::kAdded);
        return zx::ok(std::tuple{*std::move(created), true});
      }
      // If |name| already exists in this directory, look it up if the request allows it.
      if (created.error_value() == ZX_ERR_ALREADY_EXISTS && mode == CreationMode::kAllowExisting) {
        break;
      }
      // If the filesystem doesn't support creating objects, we must still try to find an entry
      // matching |name| so we can return ZX_ERR_ALREADY_EXISTS. This is required for |open()| and
      // |mkdir()| to return EEXIST if an entry matching |name| already exists.
      //
      // *NOTE*: The POSIX specification states that |open()| and |mkdir()| should return EROFS if
      // the filesystem doesn't support creating new objects, and an existing one was not found.
      // Unfortunately there is no equivalent Zircon status that maps to that error code, nor can we
      // return ZX_ERR_NOT_SUPPORTED (ENOTSUP is not a valid error for these syscalls).
      //
      // For now, if we fail to find an existing object, we will return ZX_ERR_NOT_FOUND (ENOENT)
      // below. This doesn't cause an for most callers, as most only check for the EEXIST case,
      // which is common when specifying O_CREAT | O_EXCL.
      if (created.error_value() == ZX_ERR_NOT_SUPPORTED) {
        break;
      }
      return created.take_error();
    }
    case fs::CreationMode::kNever:
      break;
  }
  // We didn't create a new object, try to lookup an existing entry matching |name|.
  fbl::RefPtr<Vnode> vn;
  if (zx_status_t status = vndir->Lookup(name, &vn); status != ZX_OK) {
    return zx::error(status);
  }
  if (mode == CreationMode::kAlways) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }
  return zx::ok(std::tuple{std::move(vn), false});
}

zx::result<bool> Vfs::TrimName(std::string_view& name) {
  bool is_dir = false;
  while (!name.empty() && name.back() == '/') {
    is_dir = true;
    name.remove_suffix(1);
  }

  if (name.empty()) {
    // 'name' should not contain paths consisting of exclusively '/' characters.
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (name.length() > NAME_MAX) {
    // Name must be less than the maximum-expected length.
    return zx::error(ZX_ERR_BAD_PATH);
  }
  if (name.find('/') != std::string::npos) {
    // Name must not contain '/' characters after being trimmed.
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(is_dir);
}

zx_status_t Vfs::Readdir(Vnode* vn, VdirCookie* cookie, void* dirents, size_t len,
                         size_t* out_actual) {
  std::lock_guard lock(vfs_lock_);
  return vn->Readdir(cookie, dirents, len, out_actual);
}

void Vfs::SetReadonly(bool value) {
  std::lock_guard lock(vfs_lock_);
  readonly_ = value;
}

}  // namespace fs
