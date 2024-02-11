// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_VIRTIO_PCI_DEVICE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_VIRTIO_PCI_DEVICE_H_

#include <lib/ddk/debug.h>
#include <lib/stdcompat/span.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zx/bti.h>
#include <lib/zx/pmt.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <limits>
#include <memory>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

namespace virtio_display {

// Implements generic parts of the virtio specification over the PCI transport.
class VirtioPciDevice : public virtio::Device {
 public:
  static zx::result<std::unique_ptr<VirtioPciDevice>> Create(
      zx::bti bti, std::unique_ptr<virtio::Backend> backend);

  // `bti` is used to obtain physical memory addresses given to the virtio
  // device.
  //
  // `virtio_queue_buffer_pool` must be large enough to store requests and
  // responses exchanged with the virtio device. The buffer must reside at
  // `virtio_queue_buffer_pool_physical_address` in the virtio device's physical
  // address space.
  //
  // The instance hangs onto `virtio_queue_buffer_pool_vmo` and
  // `virtio_queue_buffer_pool_pin` for the duration of its lifetime. They are
  // intended to keep the memory backing `virtio_queue_buffer_pool` alive and
  // pinned to `virtio_queue_buffer_pool_physical_address`.
  explicit VirtioPciDevice(zx::bti bti, std::unique_ptr<virtio::Backend> backend,
                           zx::vmo virtio_queue_buffer_pool_vmo,
                           zx::pmt virtio_queue_buffer_pool_pin,
                           zx_paddr_t virtio_queue_buffer_pool_physical_address,
                           cpp20::span<uint8_t> virtio_queue_buffer_pool);
  ~VirtioPciDevice() override;

  // Synchronous request/response exchange on the main virtqueue.
  //
  // The returned reference points to data owned by the GpuDevice instance, and
  // is only valid until the method is called again.
  //
  // Call sites are expected to rely on partial template type inference. The
  // argument type should not be specified twice.
  //
  //     const virtio_abi::EmptyRequest request = {...};
  //     const auto& response =
  //         device->ExchangeRequestResponse<virtio_abi::EmptyResponse>(
  //             request);
  //
  // The current implementation fully serializes request/response exchanges.
  // More precisely, even if ExchangeRequestResponse() is called concurrently,
  // the virtio device will never be given a request while another request is
  // pending. While this behavior is observable, it is not part of the API. The
  // implementation may be evolved to issue concurrent requests in the future.
  template <typename ResponseType, typename RequestType>
  const ResponseType& ExchangeRequestResponse(const RequestType& request);

  // virtio::Device
  zx_status_t Init() override;
  void IrqRingUpdate() override;
  void IrqConfigChange() override;
  const char* tag() const override;

 private:
  // Called when the virtio device notifies the driver of having used a buffer.
  //
  // `used_descriptor_index` is the virtqueue descriptor table index pointing to
  // the data that was consumed by the device. The indicated buffer, as well as
  // all the linked buffers, are now owned by the driver.
  void VirtioBufferUsedByDevice(uint32_t used_descriptor_index) __TA_REQUIRES(virtio_queue_mutex_);

  // Ensures that a single ExchangeRequestResponse() call is in progress.
  fbl::Mutex exchange_request_response_mutex_;

  // Protects data members modified by multiple threads.
  fbl::Mutex virtio_queue_mutex_;

  // Signaled when the virtio device consumes a designated virtio queue buffer.
  //
  // The buffer is identified by `virtio_queue_request_index_`.
  fbl::ConditionVariable virtio_queue_buffer_used_signal_;

  // Identifies the request buffer allocated in ExchangeRequestResponse().
  //
  // nullopt iff no ExchangeRequestResponse() is in progress.
  std::optional<uint16_t> virtio_queue_request_index_ __TA_GUARDED(virtio_queue_mutex_);

  // The GPU device's control virtqueue.
  //
  // Defined in the VIRTIO spec Section 5.7.2 "GPU Device" > "Virtqueues".
  virtio::Ring virtio_queue_ __TA_GUARDED(virtio_queue_mutex_);

  // Backs `virtio_queue_buffer_pool_`.
  const zx::vmo virtio_queue_buffer_pool_vmo_;

  // Pins `virtio_queue_buffer_pool_vmo_` at a known physical address.
  const zx::pmt virtio_queue_buffer_pool_pin_;

  // The starting address of `virtio_queue_buffer_pool_`.
  const zx_paddr_t virtio_queue_buffer_pool_physical_address_;

  // Memory pinned at a known physical address, used for virtqueue buffers.
  //
  // The span's data is modified by the driver and by the virtio device.
  const cpp20::span<uint8_t> virtio_queue_buffer_pool_;
};

template <typename ResponseType, typename RequestType>
const ResponseType& VirtioPciDevice::ExchangeRequestResponse(const RequestType& request) {
  static constexpr size_t request_size = sizeof(RequestType);
  static constexpr size_t response_size = sizeof(ResponseType);
  zxlogf(TRACE, "Sending %zu-byte request, expecting %zu-byte response", request_size,
         response_size);

  // Request/response exchanges are fully serialized.
  //
  // Relaxing this implementation constraint would require the following:
  // * An allocation scheme for `virtual_queue_buffer_pool`. An easy solution is
  //   to sub-divide the area into equally-sized cells, where each cell can hold
  //   the largest possible request + response.
  // * A map of virtio queue descriptor index to condition variable, so multiple
  //   threads can be waiting on the device notification interrupt.
  // * Handling the case where `virtual_queue_buffer_pool` is full. Options are
  //   waiting on a new condition variable signaled whenever a buffer is
  //   released, and asserting that implies the buffer pool is statically sized
  //   to handle the maximum possible concurrency.
  //
  // Optionally, the locking around `virtio_queue_mutex_` could be more
  // fine-grained. Allocating the descriptors needs to be serialized, but
  // populating them can be done concurrently. Last, submitting the descriptors
  // to the virtio device must be serialized.
  fbl::AutoLock exhange_request_response_lock(&exchange_request_response_mutex_);

  fbl::AutoLock virtio_queue_lock(&virtio_queue_mutex_);

  // Allocate two virtqueue descriptors. This is the minimum number of
  // descriptors needed to represent a request / response exchange using the
  // split virtqueue format described in the VIRTIO spec Section 2.7 "Split
  // Virtqueues". This is because each descriptor can point to a read-only or a
  // write-only memory buffer, and we need one of each.
  //
  // The first (returned) descriptor will point to the request buffer, and the
  // second (chained) descriptor will point to the response buffer.

  uint16_t request_descriptor_index;
  vring_desc* const request_descriptor =
      virtio_queue_.AllocDescChain(/*count=*/2, &request_descriptor_index);
  ZX_ASSERT(request_descriptor);
  virtio_queue_request_index_ = request_descriptor_index;

  cpp20::span<uint8_t> request_span = virtio_queue_buffer_pool_.subspan(0, request_size);
  std::memcpy(request_span.data(), &request, request_size);

  const zx_paddr_t request_physical_address = virtio_queue_buffer_pool_physical_address_;
  request_descriptor->addr = request_physical_address;
  static_assert(request_size <= std::numeric_limits<uint32_t>::max());
  request_descriptor->len = static_cast<uint32_t>(request_size);
  request_descriptor->flags = VRING_DESC_F_NEXT;

  vring_desc* const response_descriptor = virtio_queue_.DescFromIndex(request_descriptor->next);
  ZX_ASSERT(response_descriptor);

  cpp20::span<uint8_t> response_span =
      virtio_queue_buffer_pool_.subspan(request_size, response_size);
  std::fill(response_span.begin(), response_span.end(), 0);

  const zx_paddr_t response_physical_address = request_physical_address + request_size;
  response_descriptor->addr = response_physical_address;
  static_assert(response_size <= std::numeric_limits<uint32_t>::max());
  response_descriptor->len = static_cast<uint32_t>(response_size);
  response_descriptor->flags = VRING_DESC_F_WRITE;

  // Submit the transfer & wait for the response
  virtio_queue_.SubmitChain(request_descriptor_index);
  virtio_queue_.Kick();

  virtio_queue_buffer_used_signal_.Wait(&virtio_queue_mutex_);

  return *reinterpret_cast<ResponseType*>(response_span.data());
}

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_VIRTIO_PCI_DEVICE_H_
