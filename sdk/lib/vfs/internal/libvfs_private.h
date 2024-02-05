// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Internal library used to provide stable ABI for the in-tree VFS (//src/storage/lib/vfs/cpp).
// Public symbols must have C linkage, and must provide a stable ABI. In particular, this library
// may be linked against code that uses a different version of the C++ standard library or even
// a different version of the fuchsia.io protocol.
//
// **WARNING**: This library is distributed in binary format with the Fuchsia SDK. Use caution when
// making changes to ensure binary compatibility. Some changes may require a soft transition:
// https://fuchsia.dev/fuchsia-src/development/source_code/working_across_petals#soft-transitions

#ifndef LIB_VFS_INTERNAL_LIBVFS_PRIVATE_H_
#define LIB_VFS_INTERNAL_LIBVFS_PRIVATE_H_

#include <lib/async/dispatcher.h>
#include <stdint.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// NOLINTBEGIN(modernize-use-using): This library exposes a C interface.

// Defines if a VmoFile is writable or not.
typedef uint8_t vfs_internal_write_mode_t;
#define VFS_INTERNAL_WRITE_MODE_READ_ONLY ((vfs_internal_write_mode_t)0)
#define VFS_INTERNAL_WRITE_MODE_WRITABLE ((vfs_internal_write_mode_t)1)

// Defines how a VMO is shared from a VmoFile when a sharing mode is not specified.
typedef uint8_t vfs_internal_sharing_mode_t;
#define VFS_INTERNAL_SHARING_MODE_NONE ((vfs_internal_sharing_mode_t)0)
#define VFS_INTERNAL_SHARING_MODE_DUPLICATE ((vfs_internal_sharing_mode_t)1)
#define VFS_INTERNAL_SHARING_MODE_COW ((vfs_internal_sharing_mode_t)2)

// Handle to a VFS object capable of serving nodes.
typedef struct vfs_internal_vfs vfs_internal_vfs_t;

// Handle to a node/directory entry.
typedef struct vfs_internal_node vfs_internal_node_t;

// Callback to destroy a user-provided cookie.
typedef void (*vfs_internal_destroy_cookie_t)(void* cookie);

// Callback to connect a service node to `request`.
typedef zx_status_t (*vfs_internal_svc_connector_t)(const void* cookie, zx_handle_t request);

// Callback to populate contents of a pseudo-file during open.
typedef zx_status_t (*vfs_internal_read_handler_t)(void* cookie, const char** data_out,
                                                   size_t* len_out);

// Callback to release any buffers the pseudo-file implementation may allocate during open.
typedef void (*vfs_internal_release_buffer_t)(void* cookie);

// Callback to consume file contents when a pseudo-file is closed.
typedef zx_status_t (*vfs_internal_write_handler_t)(const void* cookie, const char* data,
                                                    size_t len);

// Create a VFS object capable of serving node connections using `dispatcher`. The object is NOT
// thread-safe and must be used with a single-threaded dispatcher.
zx_status_t vfs_internal_create(async_dispatcher_t* dispatcher, vfs_internal_vfs_t** out_vfs);

// Destroy a VFS created by `vfs_create`. `vfs` should not be used after this is called. Any active
// connections will be unbound before `vfs` is destroyed.
zx_status_t vfs_internal_destroy(vfs_internal_vfs_t* vfs);

// Use `vfs` to serve the given `vnode` over `channel` with specified `rights. `channel` must be
// be protocol compatible with the type of node being served. Takes ownership of `channel` and
// closes the handle on failure or when `vfs` is destroyed.
zx_status_t vfs_internal_serve(vfs_internal_vfs_t* vfs, const vfs_internal_node_t* vnode,
                               zx_handle_t channel, uint32_t rights);

// Destroy the specified `vnode` handle. The underlying node will remain alive so long as there are
// active connections or other vnodes that hold a reference to it.
zx_status_t vfs_internal_node_destroy(vfs_internal_node_t* vnode);

// Create a pseudo directory capable of server-side modification. Directory entries can be added or
// removed at runtime, but cannot be modified by clients.
zx_status_t vfs_internal_directory_create(vfs_internal_node_t** out_vnode);

// Add a directory entry to `dir`. `dir` must be a directory node created by `vfs_directory_create`.
zx_status_t vfs_internal_directory_add(vfs_internal_node_t* dir, const vfs_internal_node_t* vnode,
                                       const char* name);

// Remove an existing directory entry from `dir`.
zx_status_t vfs_internal_directory_remove(vfs_internal_node_t* dir, const char* name);

// Create a remote directory node. Open requests to this node will be forwarded to `remote`.
zx_status_t vfs_internal_remote_directory_create(zx_handle_t remote,
                                                 vfs_internal_node_t** out_vnode);

// Context associated with a service node. Note that `cookie` is shared across the `connect` and
// `destroy` callbacks, so they are grouped together here.
typedef struct vfs_internal_svc_context {
  void* cookie;
  vfs_internal_svc_connector_t connect;
  vfs_internal_destroy_cookie_t destroy;
} vfs_internal_svc_context_t;

// Create a service connector node. The `cookie` passed in `context` will be destroyed on failure,
// or when the node is destroyed.
zx_status_t vfs_internal_service_create(const vfs_internal_svc_context_t* context,
                                        vfs_internal_node_t** out_vnode);

// Create a file-like object backed by a VMO. Takes ownership of `vmo_handle` and destroys it on
// failure, or when the node is destroyed.
zx_status_t vfs_internal_vmo_file_create(zx_handle_t vmo_handle, uint64_t length,
                                         vfs_internal_write_mode_t writable,
                                         vfs_internal_sharing_mode_t sharing_mode,
                                         vfs_internal_node_t** out_vnode);

// Context associated with a pseudo-file node. Note that `cookie` is shared across the various
// callbacks, so they are grouped together here.
typedef struct vfs_internal_file_context {
  void* cookie;
  vfs_internal_read_handler_t read;
  vfs_internal_release_buffer_t release;
  vfs_internal_write_handler_t write;
  vfs_internal_destroy_cookie_t destroy;
} vfs_internal_file_context_t;

// Create a buffered file-like object backed by callbacks. The `cookie` passed in `context` will be
// destroyed on failure, or when the node is destroyed.
zx_status_t vfs_internal_pseudo_file_create(size_t max_bytes,
                                            const vfs_internal_file_context_t* context,
                                            vfs_internal_node_t** out_vnode);

// Create a composed service directory which allows dynamic fallback services.
zx_status_t vfs_internal_composed_svc_dir_create(vfs_internal_node_t** out_vnode);

// Adds a service instance to this composed service directory.
zx_status_t vfs_internal_composed_svc_dir_add(vfs_internal_node_t* dir,
                                              const vfs_internal_node_t* service_node,
                                              const char* name);

// Sets the fallback directory for a composed service directory. `fallback_channel` must be
// compatible with the fuchsia.io protocol.
zx_status_t vfs_internal_composed_svc_dir_set_fallback(vfs_internal_node_t* dir,
                                                       zx_handle_t fallback_channel);

// Entries in a lazy directory.
typedef struct vfs_internal_lazy_entry {
  uint64_t id;
  const char* name;
  uint32_t type;
} vfs_internal_lazy_entry_t;

// Callback used to query the contents of a lazy directory.
typedef void (*vfs_internal_get_contents_t)(void* cookie, vfs_internal_lazy_entry** entries_out,
                                            size_t* len_out);

// Callback used to get a lazy directory entry.
typedef zx_status_t (*vfs_internal_get_entry_t)(void* cookie, vfs_internal_node_t** node_out,
                                                uint64_t id, const char* name);

// Context that encapsulates the state of a lazy directory.
typedef struct vfs_internal_lazy_dir_context {
  void* cookie;
  vfs_internal_get_contents_t get_contents;
  vfs_internal_get_entry_t get_entry;
} vfs_internal_lazy_dir_context_t;

// Create a new lazy directory node. The state of `context` must outlive the directory entry.
zx_status_t vfs_internal_lazy_dir_create(const vfs_internal_lazy_dir_context* context,
                                         vfs_internal_node_t** out_vnode);

// NOLINTEND(modernize-use-using)

__END_CDECLS

#endif  // LIB_VFS_INTERNAL_LIBVFS_PRIVATE_H_
