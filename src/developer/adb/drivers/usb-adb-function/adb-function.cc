// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adb-function.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/zx/vmar.h>
#include <zircon/assert.h>

#include <cstdint>
#include <optional>

#include <fbl/auto_lock.h>
#include <usb/peripheral.h>
#include <usb/request-cpp.h>

namespace usb_adb_function {

namespace {

// CompleterType follows fidl::internal::WireCompleter<RequestType>::Async
template <typename CompleterType>
void CompleteTxn(CompleterType& completer, zx_status_t status) {
  if (status == ZX_OK) {
    completer.Reply(fit::ok());
  } else {
    completer.Reply(fit::error(status));
  }
}

}  // namespace

void UsbAdbDevice::Start(StartRequestView request, StartCompleter::Sync& completer) {
  zx_status_t ret_status = ZX_OK;
  bool enable_ep = false;
  {
    fbl::AutoLock _(&adb_mutex_);
    if (adb_binding_.has_value()) {
      zxlogf(ERROR, "Device is already bound");
      ret_status = ZX_ERR_ALREADY_BOUND;
    } else {
      adb_binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                           std::move(request->interface), this, [this](fidl::UnbindInfo info) {
                             zxlogf(INFO, "Device closed with reason '%s'",
                                    info.FormatDescription().c_str());
                             Stop();
                           });
      enable_ep = Online();
      fbl::AutoLock _(&lock_);
      auto result = fidl::WireSendEvent(adb_binding_.value())->OnStatusChanged(status_);
      if (!result.ok()) {
        zxlogf(ERROR, "Could not call AdbInterface Status %s", result.error().status_string());
        ret_status = ZX_ERR_IO;
      }
    }
  }
  // Configure endpoints as adb binding is set now.
  {
    auto status = ConfigureEndpoints(enable_ep);
    if (status != ZX_OK) {
      zxlogf(ERROR, "ConfigureEndpoints failed %d", status);
    }
    ret_status = ret_status == ZX_OK ? status : ret_status;
  }
  CompleteTxn(completer, ret_status);
}

void UsbAdbDevice::Stop(StopCompleter::Sync& completer) {
  Stop();
  completer.Reply(fit::ok());
}

void UsbAdbDevice::Stop() {
  // Disable endpoints to prevent new requests present in our pipeline from getting queued.
  ConfigureEndpoints(false);

  // Cancel all requests in the pipeline -- the completion handler will free these requests as they
  // come in.
  //
  // Do not hold locks when calling this method. It might result in deadlock as completion callbacks
  // could be invoked during this call.
  bulk_out_ep_->CancelAll().Then([](fidl::Result<fendpoint::Endpoint::CancelAll>& result) {
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to cancel all for bulk out endpoint %s",
             result.error_value().FormatDescription().c_str());
    }
  });
  bulk_in_ep_->CancelAll().Then([](fidl::Result<fendpoint::Endpoint::CancelAll>& result) {
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to cancel all for bulk in endpoint %s",
             result.error_value().FormatDescription().c_str());
    }
  });

  {
    fbl::AutoLock _(&adb_mutex_);
    adb_binding_.reset();
    while (!rx_requests_.empty()) {
      rx_requests_.front().Reply(fit::error(ZX_ERR_BAD_STATE));
      rx_requests_.pop();
    }
  }

  test_stop_sync_.Signal();
}

zx::result<> UsbAdbDevice::SendLocked() {
  if (tx_pending_reqs_.empty()) {
    return zx::ok();
  }

  if (!Online()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  auto& current = tx_pending_reqs_.front();
  std::vector<fuchsia_hardware_usb_request::Request> requests;
  while (current.start < current.request.data().size()) {
    auto req = bulk_in_ep_.GetRequest();
    if (!req) {
      break;
    }
    req->clear_buffers();

    size_t to_copy = std::min(current.request.data().size() - current.start, vmo_data_size_);
    auto actual = req->CopyTo(0, current.request.data().data() + current.start, to_copy,
                              bulk_in_ep_.GetMappedLocked);
    size_t actual_total = 0;
    for (size_t i = 0; i < actual.size(); i++) {
      // Fill in size of data.
      (*req)->data()->at(i).size(actual[i]);
      actual_total += actual[i];
    }
    auto status = req->CacheFlush(bulk_in_ep_.GetMappedLocked);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Cache flush failed %d", status);
    }

    requests.emplace_back(req->take_request());
    current.start += actual_total;
  }

  if (!requests.empty()) {
    auto result = bulk_in_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
    }
  }

  if (current.start == current.request.data().size()) {
    CompleteTxn(current.completer, ZX_OK);
    tx_pending_reqs_.pop();
  }

  return zx::ok();
}

void UsbAdbDevice::QueueTx(QueueTxRequest& request, QueueTxCompleter::Sync& completer) {
  size_t length = request.data().size();

  if (!Online() || length == 0) {
    zxlogf(INFO, "Invalid state - Online %d Length %zu", Online(), length);
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  fbl::AutoLock _(&bulk_in_ep_.mutex_);
  tx_pending_reqs_.emplace(
      txn_req_t{.request = std::move(request), .start = 0, .completer = completer.ToAsync()});

  auto result = SendLocked();
  if (result.is_error()) {
    zxlogf(INFO, "SendLocked failed %d", result.error_value());
  }
}

void UsbAdbDevice::Receive(ReceiveCompleter::Sync& completer) {
  // Return early during shutdown.
  if (!Online()) {
    completer.Reply(fit::error(ZX_ERR_BAD_STATE));
    return;
  }

  fbl::AutoLock lock(&bulk_out_ep_.mutex_);
  if (!pending_replies_.empty()) {
    auto completion = std::move(pending_replies_.front());
    pending_replies_.pop();
    lock.release();

    auto req = usb::FidlRequest(std::move(completion.request().value()));

    // This should always be true because when we registered VMOs, we only registered one per
    // request.
    ZX_ASSERT(req->data()->size() == 1);
    auto addr = bulk_out_ep_.GetMappedAddr(req.request(), 0);
    if (!addr.has_value()) {
      zxlogf(ERROR, "Failed to get mapped");
      completer.Reply(fit::error(ZX_ERR_INTERNAL));
    } else {
      completer.Reply(fit::ok(
          std::vector<uint8_t>(reinterpret_cast<uint8_t*>(*addr),
                               reinterpret_cast<uint8_t*>(*addr) + *completion.transfer_size())));
    }

    req.reset_buffers(bulk_out_ep_.GetMapped);
    auto status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
    }

    std::vector<fuchsia_hardware_usb_request::Request> requests;
    requests.emplace_back(req.take_request());
    auto result = bulk_out_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
    }
  } else {
    fbl::AutoLock _(&adb_mutex_);
    rx_requests_.emplace(completer.ToAsync());
  }
}

zx_status_t UsbAdbDevice::InsertUsbRequest(fuchsia_hardware_usb_request::Request req,
                                           usb_endpoint::UsbEndpoint<UsbAdbDevice>& ep) {
  ep.PutRequest(usb::FidlRequest(std::move(req)));
  fbl::AutoLock _(&lock_);
  // Return without adding the request to the pool during shutdown.
  ShutdownComplete();
  return shutdown_callback_ ? ZX_ERR_CANCELED : ZX_OK;
}

void UsbAdbDevice::RxComplete(fendpoint::Completion completion) {
  // Return early during shutdown.
  {
    fbl::AutoLock _(&lock_);
    if (shutdown_callback_) {
      bulk_out_ep_.PutRequest(usb::FidlRequest(std::move(completion.request().value())));
      ShutdownComplete();
      return;
    }
  }

  // This should always be true because when we registered VMOs, we only registered one per request.
  ZX_ASSERT(completion.request()->data()->size() == 1);
  if (*completion.status() == ZX_ERR_IO_NOT_PRESENT) {
    InsertUsbRequest(std::move(completion.request().value()), bulk_out_ep_);
    return;
  }

  if (*completion.status() != ZX_OK) {
    zxlogf(ERROR, "RxComplete called with error %d.", *completion.status());
    auto req = usb::FidlRequest(std::move(completion.request().value()));
    req.reset_buffers(bulk_out_ep_.GetMapped);

    auto status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
    }

    std::vector<fuchsia_hardware_usb_request::Request> requests;
    requests.emplace_back(req.take_request());
    auto result = bulk_out_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
    }
    return;
  }

  if (*completion.status() == ZX_OK) {
    fbl::AutoLock _(&adb_mutex_);
    if (!rx_requests_.empty()) {
      // This should always be true because when we registered VMOs, we only registered one per
      // request.
      ZX_ASSERT(completion.request()->data()->size() == 1);
      auto addr = bulk_out_ep_.GetMappedAddr(*completion.request(), 0);
      if (!addr.has_value()) {
        zxlogf(ERROR, "Failed to get mapped");
        rx_requests_.front().Reply(fit::error(ZX_ERR_INTERNAL));
        rx_requests_.pop();
      } else {
        rx_requests_.front().Reply(fit::ok(
            std::vector<uint8_t>(reinterpret_cast<uint8_t*>(*addr),
                                 reinterpret_cast<uint8_t*>(*addr) + *completion.transfer_size())));
        rx_requests_.pop();
      }

      auto req = usb::FidlRequest(std::move(completion.request().value()));
      req.reset_buffers(bulk_out_ep_.GetMapped);

      auto status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
      }

      std::vector<fuchsia_hardware_usb_request::Request> requests;
      requests.push_back(req.take_request());
      auto result = bulk_out_ep_->QueueRequests(std::move(requests));
      if (result.is_error()) {
        zxlogf(ERROR, "Failed to QueueRequests %s",
               result.error_value().FormatDescription().c_str());
      }
    } else {
      fbl::AutoLock _(&bulk_out_ep_.mutex_);
      pending_replies_.push(std::move(completion));
    }
  }
}

void UsbAdbDevice::TxComplete(fendpoint::Completion completion) {
  fbl::AutoLock _(&bulk_in_ep_.mutex_);
  if (InsertUsbRequest(std::move(completion.request().value()), bulk_in_ep_) != ZX_OK) {
    return;
  }
  // Do not queue requests if status is ZX_ERR_IO_NOT_PRESENT, as the underlying connection could
  // be disconnected or USB_RESET is being processed. Calling adb_send_locked in such scenario
  // will deadlock and crash the driver (see https://fxbug.dev/42174506).
  if (*completion.status() == ZX_ERR_IO_NOT_PRESENT) {
    return;
  }

  auto result = SendLocked();
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to SendLocked %d", result.error_value());
  }
}

size_t UsbAdbDevice::UsbFunctionInterfaceGetDescriptorsSize() { return sizeof(descriptors_); }

void UsbAdbDevice::UsbFunctionInterfaceGetDescriptors(uint8_t* buffer, size_t buffer_size,
                                                      size_t* out_actual) {
  const size_t length = std::min(sizeof(descriptors_), buffer_size);
  std::memcpy(buffer, &descriptors_, length);
  *out_actual = length;
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceControl(const usb_setup_t* setup,
                                                      const uint8_t* write_buffer,
                                                      size_t write_size, uint8_t* out_read_buffer,
                                                      size_t read_size, size_t* out_read_actual) {
  if (out_read_actual != NULL) {
    *out_read_actual = 0;
  }

  return ZX_OK;
}

zx_status_t UsbAdbDevice::ConfigureEndpoints(bool enable) {
  zx_status_t status;
  // Configure endpoint if not already done.
  if (enable && !bulk_out_ep_.RequestsEmpty()) {
    status = function_.ConfigEp(&descriptors_.bulk_out_ep, nullptr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to Config BULK OUT ep - %d.", status);
      return status;
    }

    status = function_.ConfigEp(&descriptors_.bulk_in_ep, nullptr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to Config BULK IN ep - %d.", status);
      return status;
    }

    // queue RX requests
    std::vector<fuchsia_hardware_usb_request::Request> requests;
    while (auto req = bulk_out_ep_.GetRequest()) {
      req->reset_buffers(bulk_out_ep_.GetMapped);
      auto status = req->CacheFlushInvalidate(bulk_out_ep_.GetMapped);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
      }

      requests.emplace_back(req->take_request());
    }
    auto result = bulk_out_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
      return result.error_value().status();
    }
    zxlogf(INFO, "ADB endpoints configured.");
  } else {
    status = function_.DisableEp(bulk_out_addr());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to disable BULK OUT ep - %d.", status);
      return status;
    }

    status = function_.DisableEp(bulk_in_addr());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to disable BULK IN ep - %d.", status);
      return status;
    }
  }
  return ZX_OK;
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  zxlogf(INFO, "configured? - %d  speed - %d.", configured, speed);
  bool adb_configured = false;
  {
    fbl::AutoLock _(&lock_);
    status_ = fadb::StatusFlags(configured);
    speed_ = speed;
  }

  {
    fbl::AutoLock _(&adb_mutex_);
    if (adb_binding_.has_value()) {
      auto result =
          fidl::WireSendEvent(adb_binding_.value())->OnStatusChanged(fadb::StatusFlags(configured));
      if (!result.ok()) {
        zxlogf(ERROR, "Could not call AdbInterface Status - %d.", result.status());
        return ZX_ERR_IO;
      }
      adb_configured = true;
    }
  }

  // Enable endpoints only when USB is configured and ADB interface is set.
  return ConfigureEndpoints(configured && adb_configured);
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  zxlogf(INFO, "interface - %d alt_setting - %d.", interface, alt_setting);
  zx_status_t status;

  if (interface != descriptors_.adb_intf.b_interface_number || alt_setting > 1) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (alt_setting) {
    if ((status = function_.ConfigEp(&descriptors_.bulk_out_ep, NULL)) != ZX_OK ||
        (status = function_.ConfigEp(&descriptors_.bulk_in_ep, NULL)) != ZX_OK) {
      zxlogf(ERROR, "usb_function_config_ep failed - %d.", status);
    }
  } else {
    if ((status = function_.DisableEp(bulk_out_addr())) != ZX_OK ||
        (status = function_.DisableEp(bulk_in_addr())) != ZX_OK) {
      zxlogf(ERROR, "usb_function_disable_ep failed - %d.", status);
    }
  }

  auto online = fadb::StatusFlags(0);
  if (alt_setting && status == ZX_OK) {
    online = fadb::StatusFlags::kOnline;

    // queue our IN reqs
    std::vector<fuchsia_hardware_usb_request::Request> requests;
    while (auto req = bulk_out_ep_.GetRequest()) {
      req->reset_buffers(bulk_out_ep_.GetMapped);
      auto status = req->CacheFlushInvalidate(bulk_out_ep_.GetMapped);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
      }

      requests.emplace_back(req->take_request());
    }
    auto result = bulk_out_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
      return result.error_value().status();
    }
  }

  {
    fbl::AutoLock _(&lock_);
    status_ = online;
  }

  fbl::AutoLock _(&adb_mutex_);
  if (adb_binding_.has_value()) {
    auto result = fidl::WireSendEvent(adb_binding_.value())->OnStatusChanged(online);
    if (!result.ok()) {
      zxlogf(ERROR, "Could not call AdbInterface Status.");
      return ZX_ERR_IO;
    }
  }

  return status;
}

void UsbAdbDevice::ShutdownComplete() {
  // Multiple threads/callbacks could observe pending_request == 0 and call ShutdownComplete
  // multiple times. Only call the callback if not already called.
  if (shutdown_callback_ && bulk_in_ep_.RequestsFull() && bulk_out_ep_.RequestsFull()) {
    shutdown_callback_();
  }
}

void UsbAdbDevice::Shutdown() {
  Stop();

  // Free all request pools.
  std::queue<txn_req_t> queue;
  {
    fbl::AutoLock _(&bulk_in_ep_.mutex_);
    std::swap(queue, tx_pending_reqs_);
  }

  while (!queue.empty()) {
    CompleteTxn(queue.front().completer, ZX_ERR_PEER_CLOSED);
    queue.pop();
  }

  // Call shutdown complete if all requests are released. This method will be called in completion
  // callbacks if pending requests exists.
  {
    fbl::AutoLock _(&lock_);
    ShutdownComplete();
  }
}

void UsbAdbDevice::DdkUnbind(ddk::UnbindTxn txn) {
  {
    fbl::AutoLock _(&lock_);
    ZX_ASSERT(!shutdown_callback_);
    shutdown_callback_ = [unbind_txn = std::move(txn)]() mutable { unbind_txn.Reply(); };
  }
  Shutdown();
}

void UsbAdbDevice::DdkRelease() { delete this; }

void UsbAdbDevice::DdkSuspend(ddk::SuspendTxn txn) {
  {
    fbl::AutoLock _(&lock_);
    ZX_ASSERT(!shutdown_callback_);
    shutdown_callback_ = [suspend_txn = std::move(txn)]() mutable {
      suspend_txn.Reply(ZX_OK, suspend_txn.requested_state());
    };
  }
  Shutdown();
}

zx_status_t UsbAdbDevice::InitEndpoint(
    fidl::ClientEnd<fuchsia_hardware_usb_function::UsbFunction>& client, uint8_t direction,
    uint8_t* ep_addrs, usb_endpoint::UsbEndpoint<UsbAdbDevice>& ep, uint32_t req_count) {
  auto status = function_.AllocEp(direction, ep_addrs);
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb_function_alloc_ep failed - %d.", status);
    return status;
  }

  status = ep.Init(*ep_addrs, client, dispatcher_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to init UsbEndpoint %d", status);
    return status;
  }

  // TODO(127854): When we support active pinning of VMOs, adb may want to use VMOs that are not
  // perpetually pinned.
  auto actual =
      ep.AddRequests(req_count, vmo_data_size_, fuchsia_hardware_usb_request::Buffer::Tag::kVmoId);
  if (actual != req_count) {
    zxlogf(ERROR, "Wanted %u requests, only got %zu requests", req_count, actual);
  }
  return actual == 0 ? ZX_ERR_INTERNAL : ZX_OK;
}

zx_status_t UsbAdbDevice::Init() {
  auto client =
      DdkConnectFidlProtocol<fuchsia_hardware_usb_function::UsbFunctionService::Device>(parent_);
  if (client.is_error()) {
    zxlogf(ERROR, "Failed to connect fidl protocol");
    return client.error_value();
  }

  auto status = function_.AllocInterface(&descriptors_.adb_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb_function_alloc_interface failed - %d.", status);
    return status;
  }

  status = InitEndpoint(*client, USB_DIR_OUT, &descriptors_.bulk_out_ep.b_endpoint_address,
                        bulk_out_ep_, bulk_rx_count_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "InitEndpoint failed - %d.", status);
    return status;
  }
  status = InitEndpoint(*client, USB_DIR_IN, &descriptors_.bulk_in_ep.b_endpoint_address,
                        bulk_in_ep_, bulk_tx_count_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "InitEndpoint failed - %d.", status);
    return status;
  }

  status = DdkAdd("usb-adb-function", DEVICE_ADD_NON_BINDABLE);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not add UsbAdbDevice %d.", status);
    return status;
  }

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  return ZX_OK;
}

zx_status_t UsbAdbDevice::Bind(void* ctx, zx_device_t* parent) {
  auto adb = std::make_unique<UsbAdbDevice>(parent, 16, 16, 2048);
  if (!adb) {
    zxlogf(ERROR, "Could not create UsbAdbDevice.");
    return ZX_ERR_NO_MEMORY;
  }
  auto status = adb->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not init UsbAdbDevice - %d.", status);
    adb->DdkRelease();
    return status;
  }

  {
    // The DDK now owns this reference.
    [[maybe_unused]] auto released = adb.release();
  }
  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops{};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = UsbAdbDevice::Bind;
  return ops;
}();

}  // namespace usb_adb_function

// clang-format off
ZIRCON_DRIVER(usb_adb, usb_adb_function::driver_ops, "zircon", "0.1");
