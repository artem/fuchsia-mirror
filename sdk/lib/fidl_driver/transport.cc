// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// clang-format off
// The MakeAnyTransport overloads need to be defined before including
// message.h, which uses them.
#include <lib/fidl_driver/cpp/transport.h>
// clang-format on

#include <lib/fdf/cpp/channel_read.h>
#include <lib/fidl/llcpp/message.h>
#include <lib/fidl/llcpp/message_storage.h>

#include <optional>

namespace fidl {
namespace internal {

namespace {
zx_status_t driver_write(fidl_handle_t handle, WriteOptions write_options, const void* data,
                         uint32_t data_count, const fidl_handle_t* handles,
                         const void* handle_metadata, uint32_t handles_count) {
  // Note: in order to force the encoder to only output one iovec, only provide an iovec buffer of
  // 1 element to the encoder.
  ZX_ASSERT(data_count == 1);
  ZX_ASSERT(handles_count == 0);
  ZX_ASSERT(handle_metadata == nullptr);

  const zx_channel_iovec_t& iovec = static_cast<const zx_channel_iovec_t*>(data)[0];
  fdf_arena_t* arena =
      write_options.outgoing_transport_context.release<internal::DriverTransport>();
  void* arena_data = fdf_arena_allocate(arena, iovec.capacity);
  memcpy(arena_data, const_cast<void*>(iovec.buffer), iovec.capacity);

  // TODO(fxbug.dev/90646) Remove const_cast.
  zx_status_t status = fdf_channel_write(handle, 0, arena, arena_data, iovec.capacity,
                                         const_cast<fidl_handle_t*>(handles), handles_count);

  fdf_arena_destroy(arena);
  return status;
}

void driver_read(fidl_handle_t handle, std::optional<ReadBuffers> existing_buffers,
                 ReadOptions read_options, TransportReadCallback callback) {
  fdf_arena_t* out_arena;
  void* out_data;
  uint32_t out_num_bytes;
  fidl_handle_t* out_handles;
  uint32_t out_num_handles;
  zx_status_t status = fdf_channel_read(handle, 0, &out_arena, &out_data, &out_num_bytes,
                                        &out_handles, &out_num_handles);
  if (status != ZX_OK) {
    callback(Result::TransportError(status), ReadBuffers{}, IncomingTransportContext());
    return;
  }

  if (existing_buffers) {
    if (existing_buffers->data_count < out_num_bytes) {
      callback(Result::TransportError(ZX_ERR_BUFFER_TOO_SMALL), ReadBuffers{},
               IncomingTransportContext());
    }
    memcpy(existing_buffers->data, out_data, out_num_bytes);

    if (existing_buffers->handles_count < out_num_handles) {
      callback(Result::TransportError(ZX_ERR_BUFFER_TOO_SMALL), ReadBuffers{},
               IncomingTransportContext());
    }
    memcpy(existing_buffers->handles, out_handles, out_num_handles * sizeof(fidl_handle_t));
  }

  ReadBuffers out_buffers = {
      .data = out_data,
      .data_count = out_num_bytes,
      .handles = out_handles,
      .handle_metadata = nullptr,
      .handles_count = out_num_handles,
  };
  callback(Result::Ok(), out_buffers,
           IncomingTransportContext::Create<internal::DriverTransport>(out_arena));
}

zx_status_t driver_create_waiter(fidl_handle_t handle, async_dispatcher_t* dispatcher,
                                 TransportWaitSuccessHandler success_handler,
                                 TransportWaitFailureHandler failure_handler,
                                 AnyTransportWaiter& any_transport_waiter) {
  any_transport_waiter.emplace<DriverWaiter>(handle, dispatcher, std::move(success_handler),
                                             std::move(failure_handler));
  return ZX_OK;
}

void driver_close(fidl_handle_t handle) { fdf_handle_close(handle); }

void driver_close_context(void* arena) { fdf_arena_destroy(static_cast<fdf_arena_t*>(arena)); }

}  // namespace

const TransportVTable DriverTransport::VTable = {
    .type = FIDL_TRANSPORT_TYPE_DRIVER,
    .encoding_configuration = &DriverTransport::EncodingConfiguration,
    .write = driver_write,
    .read = driver_read,
    .create_waiter = driver_create_waiter,
    .close = driver_close,
    .close_incoming_transport_context = driver_close_context,
    .close_outgoing_transport_context = driver_close_context,
};

zx_status_t DriverWaiter::Begin() {
  state_->channel_read.emplace(
      state_->handle, 0 /* options */,
      // TODO(bprosnitz) Pass in a raw pointer after DriverWaiter::Cancel is implemented.
      [state = state_](fdf_dispatcher_t* dispatcher, fdf::ChannelRead* channel_read,
                       fdf_status_t status) {
        if (status != ZX_OK) {
          return state->failure_handler(fidl::UnbindInfo::DispatcherError(status));
        }

        fidl::MessageRead(
            fdf::UnownedChannel(state->handle),
            [&state](IncomingMessage msg,
                     fidl::internal::IncomingTransportContext incoming_transport_context) {
              if (!msg.ok()) {
                return state->failure_handler(fidl::UnbindInfo{msg});
              }
              state->channel_read = std::nullopt;
              return state->success_handler(msg, std::move(incoming_transport_context));
            });
      });
  return state_->channel_read->Begin(fdf_dispatcher_from_async_dispatcher(state_->dispatcher));
}

const CodingConfig DriverTransport::EncodingConfiguration = {};

}  // namespace internal
}  // namespace fidl
