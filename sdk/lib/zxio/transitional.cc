// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zxio/cpp/transitional.h>
#include <lib/zxio/types.h>
#include <lib/zxio/zxio.h>
#include <sys/socket.h>
#include <zircon/types.h>

#include <algorithm>

zx_status_t zxio_sendmsg_inner(zxio_t* io, const struct msghdr* msg, int flags,
                               size_t* out_actual) {
  if (flags) {
    // TODO(https://fxbug.dev/67925): support MSG_OOB
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Variable length arrays have to have nonzero sizes, so we can't allocate a zx_iov for an empty
  // io vector. Instead, we can allocate 1 entry when msg_iovlen is 0.  This way zx_iov can still be
  // non-nullptr (for example, to avoid failure due to vector nullptr when calling
  // zx_stream_writev() further down; more generally so implementations further down don't need to
  // worry about allowing zx_iovec_t* nullptr but only when vector_count == 0), but we won't
  // actually use the 1 entry since msg->msg_iovlen is zero.
  zx_iovec_t zx_iov[std::max(msg->msg_iovlen, 1)];

  for (int i = 0; i < msg->msg_iovlen; ++i) {
    zx_iov[i] = {
        .buffer = msg->msg_iov[i].iov_base,
        .capacity = msg->msg_iov[i].iov_len,
    };
  }
  return zxio_writev(io, zx_iov, msg->msg_iovlen, 0, out_actual);
}

zx_status_t zxio_recvmsg_inner(zxio_t* io, struct msghdr* msg, int flags, size_t* out_actual) {
  zxio_flags_t zxio_flags = 0;
  if (flags & MSG_PEEK) {
    zxio_flags |= ZXIO_PEEK;
    flags &= ~MSG_PEEK;
  }
  if (flags) {
    // TODO(https://fxbug.dev/67925): support MSG_OOB
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Variable length arrays have to have nonzero sizes, so we can't allocate a zx_iov for an empty
  // io vector. Instead, we can allocate 1 entry when msg_iovlen is 0.  This way zx_iov can still be
  // non-nullptr (for example, to avoid failure due to vector nullptr when calling
  // zx_stream_readv() further down; more generally so implementations further down don't need to
  // worry about allowing zx_iovec_t* nullptr but only when vector_count == 0), but we won't
  // actually use the 1 entry since msg->msg_iovlen is zero.
  zx_iovec_t zx_iov[std::max(msg->msg_iovlen, 1)];

  for (int i = 0; i < msg->msg_iovlen; ++i) {
    iovec const& iov = msg->msg_iov[i];
    zx_iov[i] = {
        .buffer = iov.iov_base,
        .capacity = iov.iov_len,
    };
  }

  return zxio_readv(io, zx_iov, msg->msg_iovlen, zxio_flags, out_actual);
}
