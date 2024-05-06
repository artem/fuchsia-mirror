// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/vfs_types.h"

#include <fidl/fuchsia.io/cpp/natural_types.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/fit/function.h>

#include <type_traits>

namespace fio = fuchsia_io;

namespace fs {

namespace {

constexpr VnodeConnectionOptions FlagsToConnectionOptions(fio::OpenFlags flags) {
  VnodeConnectionOptions options;
  // Filter out io1 OpenFlags.RIGHT_* flags, translated to io2 Rights below.
  options.flags = flags & ~kAllIo1Rights;

  // Using Open1 requires GET_ATTRIBUTES as this is not expressible via |fio::OpenFlags|.
  // TODO(https://fxbug.dev/324080764): Restrict GET_ATTRIBUTES.
  options.rights = fio::Rights::kGetAttributes;

  // Approximate a set of io2 Rights corresponding to what is expected by |flags|.
  if (!(options.flags & fio::OpenFlags::kNodeReference)) {
    if (flags & fio::OpenFlags::kRightReadable) {
      options.rights |= fio::kRStarDir;
    }
    if (flags & fio::OpenFlags::kRightWritable) {
      options.rights |= fio::kWStarDir;
    }
    if (flags & fio::OpenFlags::kRightExecutable) {
      options.rights |= fio::kXStarDir;
    }
  }

  return options;
}

}  // namespace

zx::result<VnodeConnectionOptions> VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags flags) {
  if ((flags & fio::OpenFlags::kNodeReference) &&
      (flags - fio::kOpenFlagsAllowedWithNodeReference)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if ((flags & fio::OpenFlags::kNotDirectory) && (flags & fio::OpenFlags::kDirectory)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (flags & fio::OpenFlags::kCloneSameRights) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if ((flags & fio::OpenFlags::kTruncate) && !(flags & fio::OpenFlags::kRightWritable)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(FlagsToConnectionOptions(flags));
}

zx::result<VnodeConnectionOptions> VnodeConnectionOptions::FromCloneFlags(fio::OpenFlags flags,
                                                                          VnodeProtocol protocol) {
  constexpr fio::OpenFlags kValidCloneFlags = kAllIo1Rights | fio::OpenFlags::kAppend |
                                              fio::OpenFlags::kDescribe |
                                              fio::OpenFlags::kCloneSameRights;
  // Any flags not present in |kValidCloneFlags| should be ignored.
  flags &= kValidCloneFlags;

  // If CLONE_SAME_RIGHTS is specified, the client cannot request any specific rights.
  if ((flags & fio::OpenFlags::kCloneSameRights) && (flags & kAllIo1Rights)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Ensure we map the request to the correct flags based on the connection's protocol.
  switch (protocol) {
    case fs::VnodeProtocol::kNode: {
      flags |= fio::OpenFlags::kNodeReference;
      break;
    }
    case fs::VnodeProtocol::kDirectory: {
      flags |= fio::OpenFlags::kDirectory;
      break;
    }
    default: {
      flags |= fio::OpenFlags::kNotDirectory;
      break;
    }
  }

  VnodeConnectionOptions options = FlagsToConnectionOptions(flags);

  // Downscope the rights specified by |flags| to match those that were granted to this node
  // based on |protocol|. io1 OpenFlags expand to a set of rights which may not be compatible
  // with this protocol (e.g. OpenFlags::RIGHT_WRITABLE grants Rights::MODIFY_DIRECTORY, but this is
  // not applicable to files which do not have this right).
  options.rights = internal::DownscopeRights(options.rights, protocol);

  return zx::ok(options);
}

fio::OpenFlags VnodeConnectionOptions::ToIoV1Flags() const {
  return flags | RightsToOpenFlags(rights);
}

fio::OpenFlags RightsToOpenFlags(fio::Rights rights) {
  fio::OpenFlags flags = {};
  // Map io2 rights to io1 flags only if all constituent io2 rights are present.
  if ((rights & fio::kRStarDir) == fio::kRStarDir) {
    flags |= fio::OpenFlags::kRightReadable;
  }
  if ((rights & fio::kWStarDir) == fio::kWStarDir) {
    flags |= fio::OpenFlags::kRightWritable;
  }
  if ((rights & fio::kXStarDir) == fio::kXStarDir) {
    flags |= fio::OpenFlags::kRightExecutable;
  }
  return flags;
}

fio::wire::NodeAttributes VnodeAttributes::ToIoV1NodeAttributes() const {
  return fio::wire::NodeAttributes{.mode = mode,
                                   .id = inode,
                                   .content_size = content_size,
                                   .storage_size = storage_size,
                                   .link_count = link_count,
                                   .creation_time = creation_time,
                                   .modification_time = modification_time};
}

namespace internal {

fio::Rights DownscopeRights(fio::Rights rights, VnodeProtocol protocol) {
  switch (protocol) {
    case VnodeProtocol::kDirectory: {
      // Directories support all rights.
      return rights;
    }
    case VnodeProtocol::kFile: {
      return rights & (fio::Rights::kReadBytes | fio::Rights::kWriteBytes | fio::Rights::kExecute |
                       fio::Rights::kGetAttributes | fio::Rights::kUpdateAttributes);
    }
    case VnodeProtocol::kNode: {
      // Node connections only support GET_ATTRIBUTES.
      return rights & fio::Rights::kGetAttributes;
    }
    default: {
      // Remove all rights from unknown or unsupported node types.
      return {};
    }
  }
}

zx::result<VnodeProtocol> NegotiateProtocol(fio::NodeProtocolKinds supported,
                                            fio::NodeProtocolKinds requested) {
  using fio::NodeProtocolKinds;
  // Remove protocols that were not requested from the set of supported protocols.
  supported = supported & requested;
  // Attempt to negotiate a protocol for the connection based on the following order. The fuchsia.io
  // protocol does not enforce a particular order for resolution, and when callers specify multiple
  // protocols, they must be prepared to accept any that were set in the request.
  if (supported & NodeProtocolKinds::kConnector) {
    return zx::ok(VnodeProtocol::kService);
  }
  if (supported & NodeProtocolKinds::kDirectory) {
    return zx::ok(VnodeProtocol::kDirectory);
  }
  if (supported & NodeProtocolKinds::kFile) {
    return zx::ok(VnodeProtocol::kFile);
  }
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  if (supported & NodeProtocolKinds::kSymlink) {
    return zx::ok(VnodeProtocol::kSymlink);
  }
#endif
  // If we failed to resolve a protocol, we determine what error to return from a combination of the
  // type of node and the protocols which were requested.
  if ((requested & NodeProtocolKinds::kDirectory) && !(supported & NodeProtocolKinds::kDirectory)) {
    return zx::error(ZX_ERR_NOT_DIR);
  }
  if ((requested & NodeProtocolKinds::kFile) && !(supported & NodeProtocolKinds::kFile)) {
    return zx::error(ZX_ERR_NOT_FILE);
  }
  return zx::error(ZX_ERR_WRONG_TYPE);
}

}  // namespace internal

}  // namespace fs
