// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MISC_GOLDFISH_CONTROL_HOST_VISIBLE_HEAP_H_
#define SRC_GRAPHICS_DRIVERS_MISC_GOLDFISH_CONTROL_HOST_VISIBLE_HEAP_H_

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/wait.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fit/function.h>
#include <lib/zx/pmt.h>

#include <functional>
#include <memory>
#include <unordered_map>

#include "src/graphics/drivers/misc/goldfish_control/heap.h"

namespace goldfish {

class Control;

// LLCPP synchronous server of a goldfish device-local Fuchsia sysmem Heap
// interface.
class HostVisibleHeap : public Heap {
 public:
  static std::unique_ptr<HostVisibleHeap> Create(Control* control);

  ~HostVisibleHeap() override;

  // |fidl::WireServer<fuchsia_sysmem2::Heap>|
  void AllocateVmo(AllocateVmoRequestView request, AllocateVmoCompleter::Sync& completer) override;

  // |fidl::WireServer<fuchsia_sysmem2::Heap>|
  void DeleteVmo(DeleteVmoRequestView request, DeleteVmoCompleter::Sync& completer) override;

  // |Heap|
  void Bind(zx::channel server_request) override;

 private:
  // This constructor is for internal use only. Use |HostVisibleHeap::Create()| instead.
  explicit HostVisibleHeap(Control* control);

  // Destroy VMO and deallocate address space blocks stored in |blocks_\.
  // |buffer_key| is the unique ID of the logical buffer - i.e. the key of |blocks_|.
  void DeallocateVmo(BufferKey buffer_key);

  // Address space block information.
  //
  // The |Block| possesses the parent |vmo| acquired directly from goldfish
  // address space, until all its children are destroyed and it receives a
  // ZX_VMO_ZERO_CHILDREN signal.
  //
  // For this purpose, we also store an async Wait |wait_deallocate| waiting
  // on that signal to invoke |deallocate_callback|, and dispatch it on
  // the async |dispatcher|.
  struct Block {
    Block(zx::vmo vmo, uint64_t paddr, fit::function<void(Block&)> deallocate_callback,
          async_dispatcher_t* dispatcher);
    // The parent |vmo| acquired directly from goldfish address space.
    // This is kept by |Block| to ensure that address space blocks are
    // deallocated correctly when |vmo| is never referenced anymore.
    zx::vmo vmo;

    // Physical memory address of this memory block acquired from
    // goldfish address space.
    uint64_t paddr;

    std::optional<DeleteVmoCompleter::Async> maybe_delete_completer;

    // Since address space block is allocated at |AllocateVmo()| and it's
    // possible that the VMO can be destroyed before |CreateResource()|,
    // we should ensure that the block is deallocated in any case when
    // the VMO is unused instead of relying on |DestroyResource()|.
    //
    // So we keep the parent VMO within Heap, and send child of the parent
    // VMO as |AllocateVmo()| result. In this way, we can wait on Zircon
    // VMO "zero children" signal on |vmo| to tear down VMO and its associated
    // address space block properly. The async Wait serves this purpose by
    // attach an async wait on that signal to the heap's dispatcher.
    async::Wait wait_deallocate;
  };

  // Stores all the |Block|s allocated by the heap. Entries are created in
  // |AllocateVmo()|, and retrieved in |CreateResource()|.
  //
  // Key:   koid of |vmo| returned by AllocateVmo().
  // Value: Address space block info.
  using BlockMapType = std::unordered_map<BufferKey, Block, BufferKeyHash>;
  BlockMapType blocks_;
};

}  // namespace goldfish

#endif  // SRC_GRAPHICS_DRIVERS_MISC_GOLDFISH_CONTROL_HOST_VISIBLE_HEAP_H_
