// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_TUN_NETWORK_TUN_DEVICE_ADAPTER_H_
#define SRC_CONNECTIVITY_NETWORK_TUN_NETWORK_TUN_DEVICE_ADAPTER_H_

#include <fidl/fuchsia.hardware.network/cpp/wire.h>

#include <array>
#include <queue>

#include <fbl/mutex.h>

#include "buffer.h"
#include "config.h"
#include "port_adapter.h"
#include "src/connectivity/network/drivers/network-device/device/public/network_device.h"

namespace network {
namespace tun {

class DeviceAdapter;

// An abstract DeviceAdapter parent.
//
// This abstract class allows the owner of a `DeviceAdapter` to change its behavior and be notified
// of important events.
class DeviceAdapterParent {
 public:
  virtual ~DeviceAdapterParent() = default;

  // Gets the DeviceAdapter's configuration.
  virtual const BaseDeviceConfig& config() const = 0;
  // Called when transmit buffers become available.
  virtual void OnTxAvail(DeviceAdapter* device) = 0;
  // Called when receive buffers become available.
  virtual void OnRxAvail(DeviceAdapter* device) = 0;
};

// An entity that instantiates a `NetworkDeviceInterface` and provides an implementations of
// `fuchsia.hardware.network.device.NetworkDeviceImpl` that grants access to the buffers exchanged
// with the interface.
//
// `DeviceAdapter` is used to provide the business logic of virtual NetworkDevice implementations
// both for `tun.Device` and `tun.DevicePair` device classes.
// `DeviceAdapter` maintains the buffer nomenclature used by the DeviceInterface, that is: A "Tx"
// buffer is a buffer that contains data that is expected to be sent over a link, and an "Rx" buffer
// is free space that can be used to write data received over a link and push it back to
// applications.
class DeviceAdapter : public ddk::NetworkDeviceImplProtocol<DeviceAdapter> {
 public:
  // Creates a new `DeviceAdapter` with  `parent`, that will serve its requests through
  // `dispatcher`.
  static zx::result<std::unique_ptr<DeviceAdapter>> Create(async_dispatcher_t* dispatcher,
                                                           DeviceAdapterParent* parent);

  // Binds `req` to this adapter's `NetworkDeviceInterface`.
  zx_status_t Bind(fidl::ServerEnd<netdev::Device> req);
  // Binds `req` to the port with `port_id` in this adapter's `NetworkDeviceInterface`.
  zx_status_t BindPort(uint8_t port_id, fidl::ServerEnd<netdev::Port> req);

  // Tears down this adapter and calls `callback` when teardown is finished.
  // Tearing down causes all client channels to be closed.
  // There are no guarantees over which thread `callback` is called.
  // It is invalid to attempt to tear down a device that is already tearing down or is already torn
  // down.
  void Teardown(fit::function<void()> callback);
  // Same as `Teardown`, but blocks until teardown is complete.
  void TeardownSync();

  // NetworkDeviceImpl protocol:
  void NetworkDeviceImplInit(const network_device_ifc_protocol_t* iface,
                             network_device_impl_init_callback callback, void* cookie);
  void NetworkDeviceImplStart(network_device_impl_start_callback callback, void* cookie);
  void NetworkDeviceImplStop(network_device_impl_stop_callback callback, void* cookie);
  void NetworkDeviceImplGetInfo(device_impl_info_t* out_info);
  void NetworkDeviceImplQueueTx(const tx_buffer_t* buf_list, size_t buf_count);
  void NetworkDeviceImplQueueRxSpace(const rx_space_buffer_t* buf_list, size_t buf_count);
  void NetworkDeviceImplPrepareVmo(uint8_t vmo_id, zx::vmo vmo,
                                   network_device_impl_prepare_vmo_callback callback, void* cookie);
  void NetworkDeviceImplReleaseVmo(uint8_t vmo_id);
  void NetworkDeviceImplSetSnoop(bool snoop) { /* do nothing , only auto-snooping is allowed */
  }

  // Attempts to get a pending transmit buffer containing data expected to reach the network from
  // the pool of pending buffers.
  // The second argument given to `callback` is the number of remaining pending buffers (not
  // including the one given to it).
  // Returns `true` if a buffer was successfully allocated. The buffer given to `callback` is
  // discarded from the list of pending buffers and marked as pending for return.
  bool TryGetTxBuffer(fit::callback<zx_status_t(TxBuffer&, size_t)> callback);
  // Calls `func` with all currently enqueued tx buffers.
  // If `func` returns a value different than `ZX_OK`, the buffer is returned to the device
  // implementation with that error.
  void RetainTxBuffers(fit::function<zx_status_t(TxBuffer&)> func);
  // Attempts to write `data` and `meta` into an available rx buffer and return it to the
  // `NetworkDeviceInterface`.
  // Returns the number of remaining available buffers.
  // Returns `ZX_ERR_BAD_STATE` if the device is offline, or `ZX_ERR_SHOULD_WAIT` if there are no
  // buffers available to write `data` into
  zx::result<size_t> WriteRxFrame(PortAdapter& port,
                                  fuchsia_hardware_network::wire::FrameType frame_type,
                                  const uint8_t* data, size_t count,
                                  const std::optional<fuchsia_net_tun::wire::FrameMetadata>& meta);
  zx::result<size_t> WriteRxFrame(PortAdapter& port,
                                  fuchsia_hardware_network::wire::FrameType frame_type,
                                  const fidl::VectorView<uint8_t>& data,
                                  const std::optional<fuchsia_net_tun::wire::FrameMetadata>& meta);
  zx::result<size_t> WriteRxFrame(PortAdapter& port,
                                  fuchsia_hardware_network::wire::FrameType frame_type,
                                  const std::vector<uint8_t>& data,
                                  const std::optional<fuchsia_net_tun::wire::FrameMetadata>& meta);
  // Copies all pending tx buffers from `this` consuming any available rx buffers from `other`.
  // If `return_failed_buffers` is `true`, all buffers from `this` that couldn't be immediately
  // copied into available buffers from `other` will be returned to applications in a failure state,
  // otherwise buffers from `this` will remain in the available buffer pool.
  void CopyTo(DeviceAdapter* other, bool return_failed_buffers);

  // Handles status change notifications from port with `port_id`.
  void OnPortStatusChanged(uint8_t port_id, const port_status_t& new_status);
  // Adds |port| to the device.
  zx_status_t AddPort(PortAdapter& port);
  // Removes port with |port_id|.
  void RemovePort(uint8_t port_id);

 private:
  static constexpr uint16_t kFifoDepth = 128;
  explicit DeviceAdapter(DeviceAdapterParent* parent);

  // Enqueues a single fulfilled rx frame.
  void EnqueueRx(uint8_t port_id, fuchsia_hardware_network::wire::FrameType frame_type,
                 RxBuffer buffer, size_t length,
                 const std::optional<fuchsia_net_tun::wire::FrameMetadata>& meta)
      __TA_REQUIRES(rx_lock_);
  // Commits all pending rx buffers, returning them to the `NetworkDeviceInterface`.
  void CommitRx() __TA_REQUIRES(rx_lock_);
  // Enqueues a single consumed tx frame.
  // `status` indicates the success or failure when consuming the frame, as dictated by the
  // `NetworkDeviceInterface` contract.
  void EnqueueTx(uint32_t id, zx_status_t status) __TA_REQUIRES(tx_lock_);
  // Commits all pending tx frames, returning them to the `NetworkDeviceInterface`.
  void CommitTx() __TA_REQUIRES(tx_lock_);
  // Allocates rx buffer space with at least `length` bytes.
  zx::result<RxBuffer> AllocRxSpace(size_t length) __TA_REQUIRES(rx_lock_);
  void ReclaimRxSpace(RxBuffer buffer) __TA_REQUIRES(rx_lock_);

  std::unique_ptr<NetworkDeviceInterface> device_;
  DeviceAdapterParent* const parent_;  // pointer to parent, not owned.

  fbl::Mutex rx_lock_;
  fbl::Mutex tx_lock_;
  // NOTE: VmoStore is not thread safe in itself. Our concurrency guarantees come from the
  // `NetworkDeviceInterface` contract, where VMOs are released once we've returned all the buffers.
  // If we wrap it in a lock, or add thread safety to `VmoStore`, we end up with unnecessary added
  // complexity and also we lose the benefit of being able to operate on rx and tx frames without
  // shared locks between them.
  VmoStore vmos_;
  std::queue<TxBuffer> tx_buffers_ __TA_GUARDED(tx_lock_);
  bool tx_available_ __TA_GUARDED(tx_lock_) = false;
  std::queue<rx_space_buffer_t> rx_buffers_ __TA_GUARDED(rx_lock_);
  bool rx_available_ __TA_GUARDED(rx_lock_) = false;
  std::vector<rx_buffer_t> return_rx_list_ __TA_GUARDED(rx_lock_);
  std::array<rx_buffer_part_t, kFifoDepth> return_rx_parts_ __TA_GUARDED(rx_lock_);
  size_t return_rx_parts_count_ __TA_GUARDED(rx_lock_) = 0;
  std::vector<tx_result_t> return_tx_list_ __TA_GUARDED(tx_lock_);
  ddk::NetworkDeviceIfcProtocolClient device_iface_;
  const device_impl_info_t device_info_;
  std::array<std::atomic_bool, MAX_PORTS> port_online_status_;
};
}  // namespace tun
}  // namespace network

#endif  // SRC_CONNECTIVITY_NETWORK_TUN_NETWORK_TUN_DEVICE_ADAPTER_H_
