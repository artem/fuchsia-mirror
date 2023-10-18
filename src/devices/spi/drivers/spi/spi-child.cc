// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi-child.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/trace-engine/types.h>
#include <zircon/errors.h>

#include <ddktl/fidl.h>
#include <fbl/vector.h>

namespace spi {

void SpiChild::OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) {
  Bind(dispatcher_, std::move(request->session));
}

void SpiChild::TransmitVector(TransmitVectorRequest& request,
                              TransmitVectorCompleter::Sync& completer) {
  auto result = spi_->TransmitVector(cs_, request.data());
  if (result.is_ok()) {
    completer.Reply(ZX_OK);
  } else {
    completer.Reply(result.error_value());
  }
}

void SpiChild::ReceiveVector(ReceiveVectorRequest& request,
                             ReceiveVectorCompleter::Sync& completer) {
  auto result = spi_->ReceiveVector(cs_, request.size());
  if (result.is_ok() && result.value().size() == request.size()) {
    completer.Reply({ZX_OK, std::move(result.value())});
  } else {
    completer.Reply({result.status_value() == ZX_OK ? ZX_ERR_INTERNAL : result.error_value(),
                     std::vector<uint8_t>()});
  }
}

void SpiChild::ExchangeVector(ExchangeVectorRequest& request,
                              ExchangeVectorCompleter::Sync& completer) {
  auto result = spi_->ExchangeVector(cs_, request.txdata());
  if (result.is_ok() && result.value().size() == request.txdata().size()) {
    completer.Reply({ZX_OK, std::move(result.value())});
  } else {
    completer.Reply({result.status_value() == ZX_OK ? ZX_ERR_INTERNAL : result.error_value(),
                     std::vector<uint8_t>()});
  }
}

void SpiChild::RegisterVmo(RegisterVmoRequest& request, RegisterVmoCompleter::Sync& completer) {
  auto result =
      spi_->RegisterVmo(cs_, request.vmo_id(), std::move(request.vmo()), request.rights());
  if (result.is_ok()) {
    completer.Reply(fit::ok());
  } else {
    completer.Reply(fit::error(result.error_value()));
  }
}

void SpiChild::UnregisterVmo(UnregisterVmoRequest& request,
                             UnregisterVmoCompleter::Sync& completer) {
  auto result = spi_->UnregisterVmo(cs_, request.vmo_id());
  if (!result.is_ok()) {
    completer.Reply(fit::error(result.error_value()));
    return;
  }
  completer.Reply(fit::ok(std::move(result.value())));
}

void SpiChild::Transmit(TransmitRequest& request, TransmitCompleter::Sync& completer) {
  TRACE_DURATION("spi", "Transmit", "cs", cs_, "size", request.buffer().size());
  auto result = spi_->TransmitVmo(cs_, request.buffer());
  if (result.is_ok()) {
    completer.Reply(fit::ok());
    return;
  }
  completer.Reply(fit::error(result.error_value()));
}

void SpiChild::Receive(ReceiveRequest& request, ReceiveCompleter::Sync& completer) {
  TRACE_DURATION("spi", "Receive", "cs", cs_, "size", request.buffer().size());
  auto result = spi_->ReceiveVmo(cs_, request.buffer());
  if (result.is_ok()) {
    completer.Reply(fit::ok());
    return;
  }
  completer.Reply(fit::error(result.error_value()));
}

void SpiChild::Exchange(ExchangeRequest& request, ExchangeCompleter::Sync& completer) {
  if (request.tx_buffer().size() != request.rx_buffer().size()) {
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  TRACE_DURATION("spi", "Exchange", "cs", cs_, "size", request.tx_buffer().size());
  auto result = spi_->ExchangeVmo(cs_, request.tx_buffer(), request.rx_buffer());
  if (result.is_ok()) {
    completer.Reply(fit::ok());
    return;
  }
  completer.Reply(fit::error(result.error_value()));
}

zx_status_t SpiChild::SpiTransmit(const uint8_t* txdata_list, size_t txdata_count) {
  auto result = spi_->TransmitVector(
      cs_, std::vector<uint8_t>(const_cast<uint8_t*>(txdata_list),
                                const_cast<uint8_t*>(txdata_list) + txdata_count));
  return result.status_value();
}

zx_status_t SpiChild::SpiReceive(uint32_t size, uint8_t* out_rxdata_list, size_t rxdata_count,
                                 size_t* out_rxdata_actual) {
  if (size > rxdata_count) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  auto result = spi_->ReceiveVector(cs_, size);
  if (!result.is_ok()) {
    return result.error_value();
  }
  if (result.value().size() != size) {
    return ZX_ERR_INTERNAL;
  }

  *out_rxdata_actual = result.value().size();
  memcpy(out_rxdata_list, result.value().data(), *out_rxdata_actual);

  return ZX_OK;
}

zx_status_t SpiChild::SpiExchange(const uint8_t* txdata_list, size_t txdata_count,
                                  uint8_t* out_rxdata_list, size_t rxdata_count,
                                  size_t* out_rxdata_actual) {
  if (txdata_count != rxdata_count) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto result = spi_->ExchangeVector(
      cs_, std::vector<uint8_t>(const_cast<uint8_t*>(txdata_list),
                                const_cast<uint8_t*>(txdata_list) + txdata_count));
  if (!result.is_ok()) {
    return result.error_value();
  }
  if (result.value().size() != rxdata_count) {
    return ZX_ERR_INTERNAL;
  }

  *out_rxdata_actual = result.value().size();
  memcpy(out_rxdata_list, result.value().data(), *out_rxdata_actual);

  return ZX_OK;
}

void SpiChild::CanAssertCs(CanAssertCsCompleter::Sync& completer) {
  completer.Reply(!has_siblings_);
}

void SpiChild::AssertCs(AssertCsCompleter::Sync& completer) {
  if (has_siblings_) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  completer.Reply(spi_->LockBus(cs_).status_value());
}

void SpiChild::DeassertCs(DeassertCsCompleter::Sync& completer) {
  if (has_siblings_) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  completer.Reply(spi_->UnlockBus(cs_).status_value());
}

void SpiChild::Bind(async_dispatcher_t* dispatcher,
                    fidl::ServerEnd<fuchsia_hardware_spi::Device> server_end) {
  if (binding_.has_value()) {
    server_end.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }
  binding_ = Binding{
      .binding = fidl::BindServer(
          dispatcher, std::move(server_end), this,
          [](SpiChild* self, fidl::UnbindInfo, fidl::ServerEnd<fuchsia_hardware_spi::Device>) {
            std::optional opt = std::exchange(self->binding_, {});
            ZX_ASSERT(opt.has_value());

            self->spi_->ReleaseRegisteredVmos(self->cs_);

            // If the server is unbinding because DdkUnbind is being called, then reply to the
            // unbind transaction so that it completes.
            Binding& binding = opt.value();
            if (binding.unbind_txn.has_value()) {
              binding.unbind_txn.value().Reply();
            }
          }),
  };
}

void SpiChild::DdkUnbind(ddk::UnbindTxn txn) {
  if (binding_.has_value()) {
    Binding& binding = binding_.value();
    ZX_ASSERT(!binding.unbind_txn.has_value());
    binding.unbind_txn.emplace(std::move(txn));
    binding.binding.Unbind();
  } else {
    txn.Reply();
  }
}

void SpiChild::DdkRelease() { delete this; }

zx_status_t SpiChild::ServeOutgoingDirectory(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  zx::result status = outgoing_.AddService<fuchsia_hardware_spi::Service>(
      fuchsia_hardware_spi::Service::InstanceHandler({
          .device =
              [this](fidl::ServerEnd<fuchsia_hardware_spi::Device> server_end) {
                Bind(dispatcher_, std::move(server_end));
              },
      }));
  if (status.is_error()) {
    return status.status_value();
  }

  return outgoing_.Serve(std::move(server_end)).status_value();
}

}  // namespace spi
