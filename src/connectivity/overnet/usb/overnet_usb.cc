// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/overnet/usb/overnet_usb.h"

#include <fidl/fuchsia.hardware.usb.function/cpp/fidl.h>
#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <iterator>
#include <optional>
#include <variant>

#include <fbl/auto_lock.h>
#include <usb/request-cpp.h>

#include "fidl/fuchsia.hardware.overnet/cpp/wire_types.h"
#include "lib/async/cpp/task.h"
#include "lib/async/cpp/wait.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/internal/transport.h"

namespace fendpoint = fuchsia_hardware_usb_endpoint;
namespace ffunction = fuchsia_hardware_usb_function;

static constexpr uint8_t kOvernetMagic[] = "OVERNET USB\xff\x00\xff\x00\xff";
static constexpr size_t kOvernetMagicSize = sizeof(kOvernetMagic) - 1;

zx::result<> OvernetUsb::Start() {
  zx::result<ddk::UsbFunctionProtocolClient> function =
      compat::ConnectBanjo<ddk::UsbFunctionProtocolClient>(incoming());
  if (function.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect function", KV("status", function.status_string()));
    return function.take_error();
  }
  function_ = *function;

  auto client = incoming()->Connect<ffunction::UsbFunctionService::Device>();
  if (client.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect fidl protocol",
             KV("status", zx_status_get_string(client.error_value())));
    return zx::error(client.error_value());
  }

  zx_status_t status =
      function_.AllocStringDesc("Overnet USB interface", &descriptors_.data_interface.i_interface);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate string descriptor",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = function_.AllocInterface(&descriptors_.data_interface.b_interface_number);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate data interface",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = function_.AllocEp(USB_DIR_OUT, &descriptors_.out_ep.b_endpoint_address);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate bulk out interface",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }
  endpoint_dispatcher_.StartThread("endpoint_thread");

  status = bulk_out_ep_.Init(descriptors_.out_ep.b_endpoint_address, *client,
                             endpoint_dispatcher_.dispatcher());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to init UsbEndpoint", KV("endpoint", "out"),
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = function_.AllocEp(USB_DIR_IN, &descriptors_.in_ep.b_endpoint_address);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate bulk in interface",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = bulk_in_ep_.Init(descriptors_.in_ep.b_endpoint_address, *client,
                            endpoint_dispatcher_.dispatcher());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to init UsbEndpoint", KV("endpoint", "in"),
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  auto actual = bulk_in_ep_.AddRequests(kRequestPoolSize, kMtu,
                                        fuchsia_hardware_usb_request::Buffer::Tag::kVmoId);
  if (actual != kRequestPoolSize) {
    FDF_SLOG(ERROR, "Could not allocate all requests for IN endpoint",
             KV("wanted", kRequestPoolSize), KV("got", actual));
  }
  actual = bulk_out_ep_.AddRequests(kRequestPoolSize, kMtu,
                                    fuchsia_hardware_usb_request::Buffer::Tag::kVmoId);
  if (actual != kRequestPoolSize) {
    FDF_SLOG(ERROR, "Could not allocate all requests for OUT endpoint",
             KV("wanted", kRequestPoolSize), KV("got", actual));
  }

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);

  auto connector = devfs_connector_.Bind(dispatcher_);

  if (connector.is_error()) {
    FDF_SLOG(ERROR, "devfs_connector_.Bind() failed", KV("error", connector.status_string()));
    return connector.take_error();
  }

  fdf::DevfsAddArgs devfs;
  devfs.connector(std::move(connector.value()));
  devfs.class_name("overnet-usb");

  fdf::NodeAddArgs args;
  args.devfs_args(std::move(devfs));
  args.name("overnet_usb");

  auto controller_eps = fidl::CreateEndpoints<fdf::NodeController>();
  if (controller_eps.is_error()) {
    FDF_SLOG(ERROR, "Could not create node controller endpoints",
             KV("error", controller_eps.status_string()));
    return controller_eps.take_error();
  }
  node_controller_.Bind(std::move(controller_eps->client));

  auto node_eps = fidl::CreateEndpoints<fdf::Node>();
  if (node_eps.is_error()) {
    FDF_SLOG(ERROR, "Could not create node endpoints", KV("error", node_eps.status_string()));
    return node_eps.take_error();
  }
  node_.Bind(std::move(node_eps->client));

  auto result = fidl::Call(node())->AddChild({{
      .args = std::move(args),
      .controller = std::move(controller_eps->server),
      .node = std::move(node_eps->server),
  }});

  if (result.is_error()) {
    FDF_SLOG(ERROR, "Could not add child node",
             KV("error", result.error_value().FormatDescription()));
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok();
}

void OvernetUsb::FidlConnect(fidl::ServerEnd<fuchsia_hardware_overnet::Device> request) {
  device_binding_group_.AddBinding(dispatcher_, std::move(request), this,
                                   fidl::kIgnoreBindingClosure);
}

void OvernetUsb::PrepareStop(fdf::PrepareStopCompleter completer) {
  Shutdown([completer = std::move(completer)]() mutable { completer(zx::ok()); });
}

size_t OvernetUsb::UsbFunctionInterfaceGetDescriptorsSize() { return sizeof(descriptors_); }

void OvernetUsb::UsbFunctionInterfaceGetDescriptors(uint8_t* out_descriptors_buffer,
                                                    size_t descriptors_size,
                                                    size_t* out_descriptors_actual) {
  memcpy(out_descriptors_buffer, &descriptors_,
         std::min(descriptors_size, UsbFunctionInterfaceGetDescriptorsSize()));
  *out_descriptors_actual = UsbFunctionInterfaceGetDescriptorsSize();
}

// NOLINTNEXTLINE(readability-convert-member-functions-to-static)
zx_status_t OvernetUsb::UsbFunctionInterfaceControl(const usb_setup_t* setup,
                                                    const uint8_t* write_buffer, size_t write_size,
                                                    uint8_t* out_read_buffer, size_t read_size,
                                                    size_t* out_read_actual) {
  FDF_SLOG(WARNING, "Overnet USB driver received control message");
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t OvernetUsb::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  fbl::AutoLock lock(&lock_);
  if (!configured) {
    if (std::holds_alternative<Unconfigured>(state_)) {
      return ZX_OK;
    }

    function_.CancelAll(BulkInAddress());
    function_.CancelAll(BulkOutAddress());

    state_ = Unconfigured();
    callback_ = std::nullopt;

    zx_status_t status = function_.DisableEp(BulkInAddress());
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Failed to disable data in endpoint",
               KV("status", zx_status_get_string(status)));
      return status;
    }
    status = function_.DisableEp(BulkOutAddress());
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Failed to disable data out endpoint",
               KV("status", zx_status_get_string(status)));
      return status;
    }
    return ZX_OK;
  }

  if (!std::holds_alternative<Unconfigured>(state_)) {
    return ZX_OK;
  }

  zx_status_t status = function_.ConfigEp(&descriptors_.in_ep, nullptr);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to configure bulk in endpoint",
             KV("status", zx_status_get_string(status)));
    return status;
  }
  status = function_.ConfigEp(&descriptors_.out_ep, nullptr);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to configure bulk out endpoint",
             KV("status", zx_status_get_string(status)));
    return status;
  }

  state_ = Ready();

  std::vector<fuchsia_hardware_usb_request::Request> requests;
  while (auto req = bulk_out_ep_.GetRequest()) {
    req->reset_buffers(bulk_out_ep_.GetMapped);
    zx_status_t status = req->CacheFlushInvalidate(bulk_out_ep_.GetMapped);
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Cache flush failed", KV("status", zx_status_get_string(status)));
    }

    requests.emplace_back(req->take_request());
  }
  auto result = bulk_out_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to QueueRequests",
             KV("status", result.error_value().FormatDescription()));
    return result.error_value().status();
  }

  return ZX_OK;
}

// NOLINTNEXTLINE(readability-convert-member-functions-to-static,bugprone-easily-swappable-parameters)
zx_status_t OvernetUsb::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  return ZX_OK;
}

std::optional<usb::FidlRequest> OvernetUsb::PrepareTx() {
  if (!Online()) {
    return std::nullopt;
  }

  auto request = bulk_in_ep_.GetRequest();
  if (!request) {
    FDF_SLOG(DEBUG, "No available TX requests");
    return std::nullopt;
  }
  request->clear_buffers();

  return request;
}

void OvernetUsb::HandleSocketReadable(async_dispatcher_t*, async::WaitBase*, zx_status_t status,
                                      const zx_packet_signal_t*) {
  if (status != ZX_OK) {
    if (status != ZX_ERR_CANCELED) {
      FDF_SLOG(WARNING, "Unexpected error waiting on socket",
               KV("status", zx_status_get_string(status)));
    }

    return;
  }

  fbl::AutoLock lock(&lock_);
  auto request = PrepareTx();

  if (!request) {
    return;
  }

  // This should always be true because when we registered VMOs, we only registered one per
  // request.
  ZX_ASSERT((*request)->data()->size() == 1);

  std::optional<zx_vaddr_t> addr = bulk_out_ep_.GetMappedAddr(request->request(), 0);

  if (!addr.has_value()) {
    FDF_LOG(ERROR, "Failed to map request");
    return;
  }

  size_t actual;

  std::visit(
      [this, &addr, &actual, &status](auto&& state) __TA_REQUIRES(lock_) {
        state_ = std::forward<decltype(state)>(state).SendData(reinterpret_cast<uint8_t*>(*addr),
                                                               kMtu, &actual, &status);
      },
      std::move(state_));

  if (status == ZX_OK) {
    (*request)->data()->at(0).size(actual);
    status = request->CacheFlush(bulk_in_ep_.GetMappedLocked);
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Cache flush failed", KV("status", zx_status_get_string(status)));
    }
    std::vector<fuchsia_hardware_usb_request::Request> requests;
    requests.emplace_back(request->take_request());
    auto result = bulk_in_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to QueueRequests",
               KV("status", result.error_value().FormatDescription()));
    }
  } else {
    bulk_in_ep_.PutRequest(usb::FidlRequest(std::move(*request)));
  }

  std::visit(
      [this](auto& state) __TA_REQUIRES(lock_) {
        if (std::forward<decltype(state)>(state).ReadsWaiting()) {
          ProcessReadsFromSocket();
        }
      },
      state_);
}

OvernetUsb::State OvernetUsb::Running::SendData(uint8_t* data, size_t len, size_t* actual,
                                                zx_status_t* status) && {
  *status = socket_.read(0, data, len, actual);

  if (*status != ZX_OK && *status != ZX_ERR_SHOULD_WAIT) {
    if (*status != ZX_ERR_PEER_CLOSED) {
      FDF_SLOG(ERROR, "Failed to read from socket", KV("status", zx_status_get_string(*status)));
    }
    return Ready();
  }

  return std::move(*this);
}

void OvernetUsb::HandleSocketWritable(async_dispatcher_t*, async::WaitBase*, zx_status_t status,
                                      const zx_packet_signal_t*) {
  fbl::AutoLock lock(&lock_);

  if (status != ZX_OK) {
    if (status != ZX_ERR_CANCELED) {
      FDF_SLOG(WARNING, "Unexpected error waiting on socket",
               KV("status", zx_status_get_string(status)));
    }

    return;
  }

  std::visit([this](auto&& state)
                 __TA_REQUIRES(lock_) { state_ = std::forward<decltype(state)>(state).Writable(); },
             std::move(state_));
  std::visit(
      [this](auto& state) __TA_REQUIRES(lock_) {
        if (std::forward<decltype(state)>(state).WritesWaiting()) {
          ProcessWritesToSocket();
        }
      },
      state_);
}

OvernetUsb::State OvernetUsb::Running::Writable() && {
  if (socket_out_queue_.empty()) {
    return std::move(*this);
  }

  size_t actual;
  zx_status_t status =
      socket_.write(0, socket_out_queue_.data(), socket_out_queue_.size(), &actual);

  if (status == ZX_OK) {
    socket_out_queue_.erase(socket_out_queue_.begin(),
                            socket_out_queue_.begin() + static_cast<ssize_t>(actual));
  } else if (status != ZX_ERR_SHOULD_WAIT) {
    if (status != ZX_ERR_PEER_CLOSED) {
      FDF_SLOG(ERROR, "Failed to read from socket", KV("status", zx_status_get_string(status)));
    }
    return Ready();
  }

  return std::move(*this);
}

void OvernetUsb::SetCallback(fuchsia_hardware_overnet::wire::DeviceSetCallbackRequest* request,
                             SetCallbackCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  callback_ = Callback(fidl::WireSharedClient(std::move(request->callback), dispatcher_,
                                              fidl::ObserveTeardown([this]() {
                                                fbl::AutoLock lock(&lock_);
                                                callback_ = std::nullopt;
                                              })));
  HandleSocketAvailable();
  lock.release();

  completer.Reply();
}

void OvernetUsb::HandleSocketAvailable() {
  if (!callback_) {
    return;
  }

  if (!peer_socket_) {
    return;
  }

  (*callback_)(std::move(*peer_socket_));
  peer_socket_ = std::nullopt;
}

void OvernetUsb::Callback::operator()(zx::socket socket) {
  if (!fidl_.is_valid()) {
    return;
  }

  fidl_->NewLink(std::move(socket))
      .Then([](fidl::WireUnownedResult<fuchsia_hardware_overnet::Callback::NewLink>& result) {
        if (!result.ok()) {
          auto res = result.FormatDescription();
          FDF_SLOG(ERROR, "Failed to share socket with component", KV("status", res));
        }
      });
}

OvernetUsb::State OvernetUsb::Unconfigured::ReceiveData(uint8_t*, size_t len,
                                                        std::optional<zx::socket>*,
                                                        OvernetUsb* owner) && {
  FDF_SLOG(WARNING, "Dropped incoming data (device not configured)", KV("bytes", len));
  return *this;
}

OvernetUsb::State OvernetUsb::ShuttingDown::ReceiveData(uint8_t*, size_t len,
                                                        std::optional<zx::socket>*,
                                                        OvernetUsb* owner) && {
  FDF_SLOG(WARNING, "Dropped incoming data (device shutting down)", KV("bytes", len));
  return std::move(*this);
}

OvernetUsb::State OvernetUsb::Ready::ReceiveData(uint8_t* data, size_t len,
                                                 std::optional<zx::socket>* peer_socket,
                                                 OvernetUsb* owner) && {
  if (len != kOvernetMagicSize ||
      !std::equal(kOvernetMagic, kOvernetMagic + kOvernetMagicSize, data)) {
    FDF_SLOG(WARNING, "Dropped incoming data (driver not synchronized)", KV("bytes", len));
    return *this;
  }

  zx::socket socket;
  *peer_socket = zx::socket();

  zx_status_t status = zx::socket::create(ZX_SOCKET_STREAM, &socket, &peer_socket->value());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to create socket", KV("status", zx_status_get_string(status)));
    // There are two errors that can happen here: a kernel out of memory condition which the docs
    // say we shouldn't try to handle, and invalid arguments, which should be impossible.
    abort();
  }

  return Running(std::move(socket), owner);
}

OvernetUsb::State OvernetUsb::Running::ReceiveData(uint8_t* data, size_t len,
                                                   std::optional<zx::socket>* peer_socket,
                                                   OvernetUsb* owner) && {
  if (len == kOvernetMagicSize &&
      std::equal(kOvernetMagic, kOvernetMagic + kOvernetMagicSize, data)) {
    return Ready().ReceiveData(data, len, peer_socket, owner);
  }

  zx_status_t status;

  if (socket_out_queue_.empty()) {
    size_t actual = 0;
    while (len > 0) {
      status = socket_.write(0, data, len, &actual);

      if (status != ZX_OK) {
        break;
      }

      len -= actual;
      data += actual;
    }

    if (len == 0) {
      return std::move(*this);
    }

    if (status != ZX_ERR_SHOULD_WAIT) {
      if (status != ZX_ERR_PEER_CLOSED) {
        FDF_SLOG(ERROR, "Failed to write to socket", KV("status", zx_status_get_string(status)));
      }
      return Ready();
    }
  }

  if (len != 0) {
    std::copy(data, data + len, std::back_inserter(socket_out_queue_));
  }

  return std::move(*this);
}

void OvernetUsb::SendMagicReply(usb::FidlRequest request) {
  auto actual = request.CopyTo(0, kOvernetMagic, kOvernetMagicSize, bulk_in_ep_.GetMappedLocked);

  for (size_t i = 0; i < actual.size(); i++) {
    request->data()->at(i).size(actual[i]);
  }

  auto status = request.CacheFlush(bulk_in_ep_.GetMappedLocked);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Cache flush failed", KV("status", zx_status_get_string(status)));
  }

  std::vector<fuchsia_hardware_usb_request::Request> requests;
  requests.emplace_back(request.take_request());
  auto result = bulk_in_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to QueueRequests",
             KV("status", result.error_value().FormatDescription()));
    ResetState();
    return;
  }

  auto& state = std::get<Running>(state_);

  state.MagicSent();
  ProcessReadsFromSocket();
  HandleSocketAvailable();
}

void OvernetUsb::Running::MagicSent() {
  socket_is_new_ = false;

  async::PostTask(owner_->dispatcher_, [this]() {
    fbl::AutoLock lock(&owner_->lock_);

    if (std::holds_alternative<ShuttingDown>(owner_->state_)) {
      if (!owner_->HasPendingRequests()) {
        lock.release();
        owner_->ShutdownComplete();
      }
      return;
    }

    if (std::get_if<Running>(&owner_->state_) != this) {
      return;
    }

    if (read_waiter_->is_pending()) {
      read_waiter_->Cancel();
    }
    read_waiter_->set_object(socket()->get());
    if (write_waiter_->is_pending()) {
      write_waiter_->Cancel();
    }
    write_waiter_->set_object(socket()->get());
  });
}

void OvernetUsb::ReadComplete(fendpoint::Completion completion) {
  fbl::AutoLock lock(&lock_);
  auto request = usb::FidlRequest(std::move(completion.request().value()));
  if (*completion.status() == ZX_ERR_IO_NOT_PRESENT) {
    // Device disconnected from host.
    if (std::holds_alternative<ShuttingDown>(state_)) {
      if (!HasPendingRequests()) {
        lock.release();
        ShutdownComplete();
      }
      return;
    }
    bulk_out_ep_.PutRequest(std::move(request));
    return;
  }

  if (*completion.status() == ZX_OK) {
    // This should always be true because when we registered VMOs, we only registered one per
    // request.
    ZX_ASSERT(request->data()->size() == 1);
    auto addr = bulk_out_ep_.GetMappedAddr(request.request(), 0);
    if (!addr.has_value()) {
      FDF_SLOG(ERROR, "Failed to map RX data");
      return;
    }

    uint8_t* data = reinterpret_cast<uint8_t*>(*addr);
    size_t data_length = *completion.transfer_size();

    std::visit(
        [this, data, data_length](auto&& state) __TA_REQUIRES(lock_) {
          state_ = std::forward<decltype(state)>(state).ReceiveData(data, data_length,
                                                                    &peer_socket_, this);
        },
        std::move(state_));
    std::visit(
        [this](auto& state) __TA_REQUIRES(lock_) {
          if (std::forward<decltype(state)>(state).NewSocket()) {
            auto request = PrepareTx();

            if (request) {
              SendMagicReply(std::move(*request));
            }
          }
          if (std::forward<decltype(state)>(state).WritesWaiting()) {
            ProcessWritesToSocket();
          }
        },
        state_);
  } else if (*completion.status() != ZX_ERR_CANCELED) {
    FDF_SLOG(ERROR, "Read failed", KV("status", zx_status_get_string(*completion.status())));
  }

  if (Online()) {
    std::vector<fuchsia_hardware_usb_request::Request> requests;
    requests.emplace_back(request.take_request());
    auto result = bulk_out_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to QueueRequests",
               KV("status", result.error_value().FormatDescription()));
    }
  } else {
    if (std::holds_alternative<ShuttingDown>(state_)) {
      if (!HasPendingRequests()) {
        lock.release();
        ShutdownComplete();
      }
      return;
    }
    bulk_out_ep_.PutRequest(std::move(request));
  }
}

void OvernetUsb::WriteComplete(fendpoint::Completion completion) {
  auto request = usb::FidlRequest(std::move(completion.request().value()));
  fbl::AutoLock lock(&lock_);
  if (std::holds_alternative<ShuttingDown>(state_)) {
    if (!HasPendingRequests()) {
      lock.release();
      ShutdownComplete();
    }
    return;
  }

  if (auto state = std::get_if<Running>(&state_)) {
    if (state->NewSocket()) {
      SendMagicReply(std::move(request));
      return;
    }
  }

  bulk_in_ep_.PutRequest(std::move(request));
  ProcessReadsFromSocket();
}

void OvernetUsb::Shutdown(fit::function<void()> callback) {
  fbl::AutoLock lock(&lock_);
  function_.CancelAll(BulkInAddress());
  function_.CancelAll(BulkOutAddress());

  state_ = ShuttingDown(std::move(callback));

  if (!HasPendingRequests()) {
    lock.release();
    ShutdownComplete();
  }
}

void OvernetUsb::ShutdownComplete() {
  if (auto state = std::get_if<ShuttingDown>(&state_)) {
    state->FinishWithCallback();
  } else {
    FDF_SLOG(ERROR, "ShutdownComplete called outside of shutdown path");
  }
}

FUCHSIA_DRIVER_EXPORT(OvernetUsb);
