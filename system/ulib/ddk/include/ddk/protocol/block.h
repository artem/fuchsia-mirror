// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// WARNING: THIS FILE IS MACHINE GENERATED. DO NOT EDIT.
//          MODIFY system/fidl/protocols/block.banjo INSTEAD.

#pragma once

#include <zircon/compiler.h>
#include <zircon/device/block.h>
#include <zircon/types.h>

__BEGIN_CDECLS;

// Forward declarations

typedef struct block_read_write block_read_write_t;
typedef struct block_trim block_trim_t;
typedef union block_op block_op_t;
typedef struct block_impl_protocol block_impl_protocol_t;
typedef void (*block_impl_queue_callback)(void* ctx, zx_status_t status, block_op_t* op);

// Declarations

// Require that this operation will not begin until all previous
// operations have completed.
// Prevents earlier operations from being reordered after this one.
#define BLOCK_FL_BARRIER_BEFORE UINT32_C(0x00000100)

// Require that this operation complete before any subsequent
// operations are started.
// Prevents later operations from being reordered before this one.
#define BLOCK_FL_BARRIER_AFTER UINT32_C(0x00000200)

// Mark this operation as "Force Unit Access" (FUA), indicating that
// it should not complete until the data is written to the non-volatile
// medium (write), and that reads should bypass any on-device caches.
#define BLOCK_FL_FORCE_ACCESS UINT32_C(0x00001000)

// `BLOCK_OP_READ`, `BLOCK_OP_WRITE`
struct block_read_write {
    // Command and flags.
    uint32_t command;
    // Available for temporary use.
    uint32_t extra;
    // VMO of data to read or write.
    zx_handle_t vmo;
    // Transfer length in blocks (0 is invalid).
    uint32_t length;
    // Device offset in blocks.
    uint64_t offset_dev;
    // VMO offset in blocks.
    uint64_t offset_vmo;
};

#define BLOCK_OP_WRITE UINT32_C(0x00000002)

// Write any controller or device cached data to nonvolatile storage.
// This operation always implies BARRIER_BEFORE and BARRIER_AFTER,
// meaning that previous operations will complete before it starts
// and later operations will not start until it is done.
#define BLOCK_OP_FLUSH UINT32_C(0x00000003)

#define BLOCK_OP_TRIM UINT32_C(0x00000004)

#define BLOCK_OP_READ UINT32_C(0x00000001)

#define BLOCK_OP_MASK UINT32_C(0x000000FF)

// `BLOCK_OP_TRIM`
struct block_trim {
    // Command and flags.
    uint32_t command;
};

union block_op {
    // All Commands
    uint32_t command;
    // Read and Write ops use rw for parameters.
    block_read_write_t rw;
    block_trim_t trim;
};

typedef struct block_impl_protocol_ops {
    void (*query)(void* ctx, block_info_t* out_info, size_t* out_block_op_size);
    void (*queue)(void* ctx, block_op_t* txn, block_impl_queue_callback callback, void* cookie);
    zx_status_t (*get_stats)(void* ctx, bool clear, block_stats_t* out_stats);
} block_impl_protocol_ops_t;

struct block_impl_protocol {
    block_impl_protocol_ops_t* ops;
    void* ctx;
};

// Obtain the parameters of the block device (block_info_t) and
// the required size of block_txn_t.  The block_txn_t's submitted
// via queue() must have block_op_size_out - sizeof(block_op_t) bytes
// available at the end of the structure for the use of the driver.
static inline void block_impl_query(const block_impl_protocol_t* proto, block_info_t* out_info,
                                    size_t* out_block_op_size) {
    proto->ops->query(proto->ctx, out_info, out_block_op_size);
}
// Submit an IO request for processing. Ownership of |op| is transferred to
// callee until |completion_cb| is invoked|. Success or failure will
// be reported via the |completion_cb|.  This / callback may be called
// before the queue() method returns.
static inline void block_impl_queue(const block_impl_protocol_t* proto, block_op_t* txn,
                                    block_impl_queue_callback callback, void* cookie) {
    proto->ops->queue(proto->ctx, txn, callback, cookie);
}
// Returns stats concerning IO operations on the device. Will return
// ZX_ERR_NOT_SUPPORTED if stats are not enabled on this device. Clears the
// metrics on the block device if clear is true.
static inline zx_status_t block_impl_get_stats(const block_impl_protocol_t* proto, bool clear,
                                               block_stats_t* out_stats) {
    return proto->ops->get_stats(proto->ctx, clear, out_stats);
}

__END_CDECLS;
