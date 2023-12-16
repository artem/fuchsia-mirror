// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/drivers/misc/goldfish_control/heap.h"

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fit/function.h>

#include <memory>

#include <fbl/intrusive_double_list.h>

#include "src/graphics/drivers/misc/goldfish_control/control_device.h"

namespace goldfish {

Heap::Heap(Control* control, const char* tag)
    : control_(control), loop_(&kAsyncLoopConfigNoAttachToCurrentThread), tag_(tag) {
  ZX_DEBUG_ASSERT(control_);

  // Start server thread. Heap server must be running on a separate
  // thread as sysmem might be making synchronous allocation requests
  // from the main thread.
  std::string thread_name = tag_ + "-thread";
  loop_.StartThread(thread_name.c_str());
}

Heap::~Heap() { loop_.Shutdown(); }

void Heap::BindWithHeapProperties(zx::channel server_request,
                                  std::unique_ptr<fidl::Arena<512>> allocator,
                                  fuchsia_hardware_sysmem::wire::HeapProperties heap_properties) {
  async::PostTask(
      loop_.dispatcher(),
      [server_end = fidl::ServerEnd<fuchsia_hardware_sysmem::Heap>(std::move(server_request)),
       allocator = std::move(allocator), heap_properties = std::move(heap_properties),
       this]() mutable {
        auto binding =
            fidl::BindServer(loop_.dispatcher(), std::move(server_end), this,
                             [](Heap* self, fidl::UnbindInfo info,
                                fidl::ServerEnd<fuchsia_hardware_sysmem::Heap> server_end) {
                               self->OnClose(info, server_end.TakeChannel());
                             });
        auto result = fidl::WireSendEvent(binding)->OnRegister(std::move(heap_properties));
        if (!result.ok()) {
          zxlogf(ERROR, "[%s] OnRegister() failed: %s", tag_.c_str(),
                 result.FormatDescription().c_str());
        }
      });
}

void Heap::OnClose(fidl::UnbindInfo info, zx::channel channel) {
  if (info.is_dispatcher_shutdown()) {
    // The Control device Heap belongs to is already destroyed so that the
    // pending wait is cancelled. In that case we don't need to remove the heap,
    // instead just exit.
    zxlogf(INFO, "[%s] Control device is destroyed: status: %d", tag_.c_str(), info.status());
    return;
  }

  if (info.is_peer_closed()) {
    zxlogf(INFO, "[%s] Client closed Heap connection: epitaph: %d", tag_.c_str(), info.status());
  } else if (!info.is_user_initiated()) {
    zxlogf(ERROR, "[%s] Channel internal error: status: %d", tag_.c_str(), info.status());
  }

  control_->RemoveHeap(this);
}

}  // namespace goldfish
