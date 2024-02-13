// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio.h"

#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/defer.h>
#include <zircon/types.h>

#include <memory>

#include <bind/fuchsia/gpio/cpp/bind.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

namespace gpio {

// Helper functions for converting FIDL result types to zx_status_t and back.

template <typename T>
inline zx_status_t FidlStatus(const T& result) {
  if (result.ok()) {
    return result->is_ok() ? ZX_OK : result->error_value();
  }
  return result.status();
}

inline fit::result<zx_status_t> FidlResult(zx_status_t status) {
  if (status == ZX_OK) {
    return fit::success();
  }
  return fit::error(status);
}

zx_status_t GpioDevice::GpioConfigIn(uint32_t flags) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.ConfigIn(pin_, flags);
}

zx_status_t GpioDevice::GpioConfigOut(uint8_t initial_value) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.ConfigOut(pin_, initial_value);
}

zx_status_t GpioDevice::GpioSetAltFunction(uint64_t function) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.SetAltFunction(pin_, function);
}

zx_status_t GpioDevice::GpioRead(uint8_t* out_value) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.Read(pin_, out_value);
}

zx_status_t GpioDevice::GpioWrite(uint8_t value) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.Write(pin_, value);
}

zx_status_t GpioDevice::GpioGetInterrupt(uint32_t flags, zx::interrupt* out_irq) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.GetInterrupt(pin_, flags, out_irq);
}

zx_status_t GpioDevice::GpioReleaseInterrupt() {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.ReleaseInterrupt(pin_);
}

zx_status_t GpioDevice::GpioSetPolarity(gpio_polarity_t polarity) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.SetPolarity(pin_, polarity);
}

zx_status_t GpioDevice::GpioGetDriveStrength(uint64_t* ds_ua) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.GetDriveStrength(pin_, ds_ua);
}

zx_status_t GpioDevice::GpioSetDriveStrength(uint64_t ds_ua, uint64_t* out_actual_ds_ua) {
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(gpio_banjo_.is_valid());
  return gpio_banjo_.SetDriveStrength(pin_, ds_ua, out_actual_ds_ua);
}

void GpioDevice::GetPin(GetPinCompleter::Sync& completer) { completer.ReplySuccess(pin_); }

void GpioDevice::GetName(GetNameCompleter::Sync& completer) {
  completer.ReplySuccess(fidl::StringView::FromExternal(name_));
}

void GpioDevice::ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    return completer.Reply(
        FidlResult(gpio_banjo_.ConfigIn(pin_, static_cast<uint32_t>(request->flags))));
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)
      ->ConfigIn(pin_, request->flags)
      .ThenExactlyOnce(fit::inline_callback<
                       void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::ConfigIn>&),
                       sizeof(ConfigInCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess();
            }
          }));
}

void GpioDevice::ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    return completer.Reply(FidlResult(gpio_banjo_.ConfigOut(pin_, request->initial_value)));
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)
      ->ConfigOut(pin_, request->initial_value)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::ConfigOut>&),
              sizeof(ConfigOutCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess();
                }
              }));
}

void GpioDevice::Read(ReadCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    uint8_t value{};
    if (zx_status_t status = gpio_banjo_.Read(pin_, &value); status == ZX_OK) {
      completer.ReplySuccess(value);
    } else {
      completer.ReplyError(status);
    }
    return;
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)->Read(pin_).ThenExactlyOnce(
      fit::inline_callback<void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::Read>&),
                           sizeof(ReadCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess(result->value()->value);
            }
          }));
}

void GpioDevice::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    return completer.Reply(FidlResult(gpio_banjo_.Write(pin_, request->value)));
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)
      ->Write(pin_, request->value)
      .ThenExactlyOnce(fit::inline_callback<
                       void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::Write>&),
                       sizeof(WriteCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess();
            }
          }));
}

void GpioDevice::SetDriveStrength(SetDriveStrengthRequestView request,
                                  SetDriveStrengthCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    uint64_t actual_ds_ua{};
    if (zx_status_t status = gpio_banjo_.SetDriveStrength(pin_, request->ds_ua, &actual_ds_ua);
        status == ZX_OK) {
      completer.ReplySuccess(actual_ds_ua);
    } else {
      completer.ReplyError(status);
    }
    return;
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)
      ->SetDriveStrength(pin_, request->ds_ua)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::SetDriveStrength>&),
              sizeof(SetDriveStrengthCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess(result->value()->actual_ds_ua);
                }
              }));
}

void GpioDevice::GetDriveStrength(GetDriveStrengthCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    uint64_t result_ua{};
    if (zx_status_t status = gpio_banjo_.GetDriveStrength(pin_, &result_ua); status == ZX_OK) {
      completer.ReplySuccess(result_ua);
    } else {
      completer.ReplyError(status);
    }
    return;
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)->GetDriveStrength(pin_).ThenExactlyOnce(
      fit::inline_callback<
          void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::GetDriveStrength>&),
          sizeof(GetDriveStrengthCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess(result->value()->result_ua);
            }
          }));
}

void GpioDevice::GetInterrupt(GetInterruptRequestView request,
                              GetInterruptCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    zx::interrupt irq{};
    if (zx_status_t status = gpio_banjo_.GetInterrupt(pin_, request->flags, &irq);
        status == ZX_OK) {
      completer.ReplySuccess(std::move(irq));
    } else {
      completer.ReplyError(status);
    }
    return;
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)
      ->GetInterrupt(pin_, request->flags)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::GetInterrupt>&),
              sizeof(GetInterruptCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess(std::move(result->value()->irq));
                }
              }));
}

void GpioDevice::ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    return completer.Reply(FidlResult(gpio_banjo_.ReleaseInterrupt(pin_)));
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)->ReleaseInterrupt(pin_).ThenExactlyOnce(
      fit::inline_callback<
          void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::ReleaseInterrupt>&),
          sizeof(ReleaseInterruptCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess();
            }
          }));
}

void GpioDevice::SetAltFunction(SetAltFunctionRequestView request,
                                SetAltFunctionCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    return completer.Reply(FidlResult(gpio_banjo_.SetAltFunction(pin_, request->function)));
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)
      ->SetAltFunction(pin_, request->function)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::SetAltFunction>&),
              sizeof(SetAltFunctionCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess();
                }
              }));
}

void GpioDevice::SetPolarity(SetPolarityRequestView request,
                             SetPolarityCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  if (gpio_banjo_.is_valid()) {
    return completer.Reply(
        FidlResult(gpio_banjo_.SetPolarity(pin_, static_cast<uint32_t>(request->polarity))));
  }

  fdf::Arena arena('GPIO');
  gpio_->buffer(arena)
      ->SetPolarity(pin_, request->polarity)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::SetPolarity>&),
              sizeof(SetPolarityCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess();
                }
              }));
}

zx_status_t GpioDevice::InitAddDevice(const uint32_t controller_id) {
  char name[20];
  snprintf(name, sizeof(name), "gpio-%u", pin_);

  zx_device_prop_t props[] = {
      {BIND_GPIO_PIN, 0, pin_},
      {BIND_GPIO_CONTROLLER, 0, controller_id},
  };

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  fuchsia_hardware_gpio::Service::InstanceHandler handler({
      .device = bindings_->CreateHandler(this, fidl_dispatcher_->async_dispatcher(),
                                         fidl::kIgnoreBindingClosure),
  });
  auto result = outgoing_->AddService<fuchsia_hardware_gpio::Service>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory");
    return result.status_value();
  }

  result = outgoing_->Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory");
    return result.status_value();
  }

  std::array offers = {
      fuchsia_hardware_gpio::Service::Name,
  };

  return DdkAdd(
      ddk::DeviceAddArgs(name).set_props(props).set_fidl_service_offers(offers).set_outgoing_dir(
          endpoints->client.TakeChannel()));
}

void GpioDevice::DdkUnbind(ddk::UnbindTxn txn) {
  async::PostTask(fidl_dispatcher_->async_dispatcher(), [this, txn = std::move(txn)]() mutable {
    {
      fbl::AutoLock lock(&lock_);

      // The release hook must run synchronously on the main driver dispatcher, so destroy any
      // objects that live on the FIDL dispatcher during unbind instead.

      outgoing_.reset();
      bindings_.reset();
      gpio_.reset();
    }

    txn.Reply();
  });
}

void GpioDevice::DdkRelease() { delete this; }

zx_status_t GpioRootDevice::Create(void* ctx, zx_device_t* parent) {
  const ddk::GpioImplProtocolClient gpio_banjo(parent);
  if (gpio_banjo.is_valid()) {
    zxlogf(INFO, "Using Banjo gpioimpl protocol");
  }

  uint32_t controller_id = 0;
  {
    fdf::WireSyncClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio_fidl;
    if (!gpio_banjo.is_valid()) {
      zx::result gpio_fidl_client =
          DdkConnectRuntimeProtocol<fuchsia_hardware_gpioimpl::Service::Device>(parent);
      if (gpio_fidl_client.is_ok()) {
        zxlogf(INFO, "Failed to get Banjo gpioimpl protocol, falling back to FIDL");
        gpio_fidl = fdf::WireSyncClient(std::move(*gpio_fidl_client));
      } else {
        zxlogf(ERROR, "Failed to get Banjo or FIDL gpioimpl protocol");
        return ZX_ERR_NO_RESOURCES;
      }

      fdf::Arena arena('GPIO');
      if (const auto result = gpio_fidl.buffer(arena)->GetControllerId(); result.ok()) {
        controller_id = result->controller_id;
      } else {
        zxlogf(ERROR, "Failed to get controller ID: %s", result.status_string());
        return result.status();
      }
    }

    // Process init metadata while we are still the exclusive owner of the GPIO client.
    GpioInitDevice::Create(parent, {gpio_banjo, std::move(gpio_fidl)}, controller_id);
  }

  fbl::AllocChecker ac;
  std::unique_ptr<GpioRootDevice> root(new (&ac) GpioRootDevice(parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto scheduler_role = ddk::GetEncodedMetadata<fuchsia_scheduler::wire::RoleName>(
      parent, DEVICE_METADATA_SCHEDULER_ROLE_NAME);
  if (scheduler_role.is_ok()) {
    const std::string role_name(scheduler_role->role.get());

    zx::result result = fdf::SynchronizedDispatcher::Create(
        {}, "GPIO", fit::bind_member<&GpioRootDevice::DispatcherShutdownHandler>(root.get()),
        role_name);
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to create SynchronizedDispatcher: %s", result.status_string());
      return result.error_value();
    }

    // If scheduler role metadata was provided, create a new dispatcher using the role, and use
    // that dispatcher instead of the default dispatcher passed to this method.
    root->fidl_dispatcher_.emplace(*std::move(result));

    zxlogf(DEBUG, "Using dispatcher with role \"%s\"", role_name.c_str());
  }

  zx_status_t status = root->DdkAdd(ddk::DeviceAddArgs("gpio").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  auto root_release = fit::defer([&root]() { [[maybe_unused]] auto ptr = root.release(); });

  auto pins = ddk::GetMetadataArray<gpio_pin_t>(parent, DEVICE_METADATA_GPIO_PINS);
  if (pins.is_error()) {
    if (pins.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No pins metadata provided");
    } else {
      zxlogf(ERROR, "Failed to get metadata array: %s", pins.status_string());
      return pins.status_value();
    }
  } else {
    // Make sure that the list of GPIO pins has no duplicates.
    auto gpio_cmp_lt = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin < rhs.pin; };
    auto gpio_cmp_eq = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin == rhs.pin; };
    std::sort(pins.value().begin(), pins.value().end(), gpio_cmp_lt);
    auto result = std::adjacent_find(pins.value().begin(), pins.value().end(), gpio_cmp_eq);
    if (result != pins.value().end()) {
      zxlogf(ERROR, "gpio pin '%d' was published more than once", result->pin);
      return ZX_ERR_INVALID_ARGS;
    }

    if (root->fidl_dispatcher_) {
      // Pin devices must be created on the FIDL dispatcher if it exists.
      async::PostTask(root->fidl_dispatcher_->async_dispatcher(),
                      [=, root = root.get(), pins = *std::move(pins)]() {
                        root->AddPinDevices(controller_id, gpio_banjo, pins);
                      });
    } else {
      return root->AddPinDevices(controller_id, gpio_banjo, *pins);
    }
  }

  return ZX_OK;
}

void GpioRootDevice::DdkUnbind(ddk::UnbindTxn txn) {
  ZX_ASSERT_MSG(!unbind_txn_, "unbind_txn_ already set");
  if (fidl_dispatcher_) {
    unbind_txn_.emplace(std::move(txn));

    // Post a task to the FIDL dispatcher to start shutting itself down. If run on this thread
    // instead, the dispatcher could change states while a task on it is running, potentially
    // causing an assert failure if that task was trying to bind a FIDL server.
    async::PostTask(fidl_dispatcher_->async_dispatcher(),
                    [&]() { fidl_dispatcher_->ShutdownAsync(); });
  } else {
    txn.Reply();
  }
}

void GpioRootDevice::DdkRelease() { delete this; }

void GpioRootDevice::DispatcherShutdownHandler(fdf_dispatcher_t* dispatcher) {
  async::PostTask(driver_dispatcher_->async_dispatcher(), [this]() {
    ZX_DEBUG_ASSERT_MSG(unbind_txn_,
                        "unbind_txn_ was not set before the FIDL dispatcher shut down");
    unbind_txn_->Reply();
  });
}

zx_status_t GpioRootDevice::AddPinDevices(const uint32_t controller_id,
                                          const ddk::GpioImplProtocolClient& gpio_banjo,
                                          const std::vector<gpio_pin_t>& pins) {
  const fdf::UnownedDispatcher fidl_dispatcher =
      fidl_dispatcher_ ? fdf::UnownedDispatcher(fidl_dispatcher_->get())
                       : fdf::Dispatcher::GetCurrent();

  for (const auto& pin : pins) {
    fbl::AllocChecker ac;
    std::unique_ptr<GpioDevice> dev;

    if (gpio_banjo.is_valid()) {
      dev.reset(new (&ac)
                    GpioDevice(zxdev(), fidl_dispatcher->borrow(), gpio_banjo, pin.pin, pin.name));
    } else {
      zx::result gpio =
          DdkConnectRuntimeProtocol<fuchsia_hardware_gpioimpl::Service::Device>(parent());
      ZX_ASSERT_MSG(gpio.is_ok(), "Failed to get additional FIDL client: %s", gpio.status_string());
      dev.reset(new (&ac) GpioDevice(zxdev(), fidl_dispatcher->borrow(), *std::move(gpio), pin.pin,
                                     pin.name));
    }

    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    zx_status_t status = dev->InitAddDevice(controller_id);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
      return status;
    }

    // dev is now owned by devmgr.
    [[maybe_unused]] auto ptr = dev.release();
  }

  return ZX_OK;
}

void GpioInitDevice::Create(zx_device_t* parent, GpioImplProxy gpio, const uint32_t controller_id) {
  // Don't add the init device if anything goes wrong here, as the hardware may be in a state that
  // child devices don't expect.
  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_gpioimpl::wire::InitMetadata>(
      parent, DEVICE_METADATA_GPIO_INIT);
  if (!decoded.is_ok()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No init metadata provided");
    } else {
      zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    }
    return;
  }

  auto device = std::make_unique<GpioInitDevice>(parent);
  if (device->ConfigureGpios(*decoded.value(), gpio) != ZX_OK) {
    // Return without adding the init device if some GPIOs could not be configured. This will
    // prevent all drivers that depend on the initial state from binding, which should make it more
    // obvious that something has gone wrong.
    return;
  }

  zx_device_prop_t props[] = {
      {BIND_INIT_STEP, 0, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO},
      {BIND_GPIO_CONTROLLER, 0, controller_id},
  };

  zx_status_t status = device->DdkAdd(
      ddk::DeviceAddArgs("gpio-init").set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE).set_props(props));
  if (status == ZX_OK) {
    [[maybe_unused]] auto _ = device.release();
  } else {
    zxlogf(ERROR, "Failed to add gpio-init: %s", zx_status_get_string(status));
  }
}

zx_status_t GpioInitDevice::ConfigureGpios(
    const fuchsia_hardware_gpioimpl::wire::InitMetadata& metadata, const GpioImplProxy& gpio) {
  // Stop processing the list if any call returns an error so that GPIOs are not accidentally put
  // into an unexpected state.
  for (const auto& step : metadata.steps) {
    if (step.call.is_input_flags()) {
      if (zx_status_t status =
              gpio.ConfigIn(step.index, static_cast<uint32_t>(step.call.input_flags()));
          status != ZX_OK) {
        zxlogf(ERROR, "ConfigIn(%u) failed for %u: %s",
               static_cast<uint32_t>(step.call.input_flags()), step.index,
               zx_status_get_string(status));
        return status;
      }
    } else if (step.call.is_output_value()) {
      if (zx_status_t status = gpio.ConfigOut(step.index, step.call.output_value());
          status != ZX_OK) {
        zxlogf(ERROR, "ConfigOut(%u) failed for %u: %s", step.call.output_value(), step.index,
               zx_status_get_string(status));
        return status;
      }
    } else if (step.call.is_alt_function()) {
      if (zx_status_t status = gpio.SetAltFunction(step.index, step.call.alt_function());
          status != ZX_OK) {
        zxlogf(ERROR, "SetAltFunction(%lu) failed for %u: %s", step.call.drive_strength_ua(),
               step.index, zx_status_get_string(status));
        return status;
      }
    } else if (step.call.is_drive_strength_ua()) {
      uint64_t actual_ds;
      if (zx_status_t status =
              gpio.SetDriveStrength(step.index, step.call.drive_strength_ua(), &actual_ds);
          status != ZX_OK) {
        zxlogf(ERROR, "SetDriveStrength(%lu) failed for %u: %s", step.call.drive_strength_ua(),
               step.index, zx_status_get_string(status));
        return status;
      } else if (actual_ds != step.call.drive_strength_ua()) {
        zxlogf(WARNING, "Actual drive strength (%lu) doesn't match expected (%lu) for %u",
               actual_ds, step.call.drive_strength_ua(), step.index);
        return ZX_ERR_BAD_STATE;
      }
    } else if (step.call.is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.call.delay())));
    }
  }

  return ZX_OK;
}

zx_status_t GpioImplProxy::ConfigIn(uint32_t index, uint32_t flags) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.ConfigIn(index, flags);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->ConfigIn(
      index, static_cast<fuchsia_hardware_gpio::GpioFlags>(flags));
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::ConfigOut(uint32_t index, uint8_t initial_value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.ConfigOut(index, initial_value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->ConfigOut(index, initial_value);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::SetAltFunction(uint32_t index, uint64_t function) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.SetAltFunction(index, function);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->SetAltFunction(index, function);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::Read(uint32_t index, uint8_t* out_value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.Read(index, out_value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->Read(index);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_value = result->value()->value;
  return ZX_OK;
}

zx_status_t GpioImplProxy::Write(uint32_t index, uint8_t value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.Write(index, value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->Write(index, value);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::GetInterrupt(uint32_t index, uint32_t flags,
                                        zx::interrupt* out_irq) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.GetInterrupt(index, flags, out_irq);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->GetInterrupt(index, flags);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_irq = std::move(result->value()->irq);
  return ZX_OK;
}

zx_status_t GpioImplProxy::ReleaseInterrupt(uint32_t index) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.ReleaseInterrupt(index);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->ReleaseInterrupt(index);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::SetPolarity(uint32_t index, gpio_polarity_t polarity) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.SetPolarity(index, polarity);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->SetPolarity(
      index, static_cast<fuchsia_hardware_gpio::GpioPolarity>(polarity));
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::GetDriveStrength(uint32_t index, uint64_t* out_value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.GetDriveStrength(index, out_value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->GetDriveStrength(index);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_value = result->value()->result_ua;
  return ZX_OK;
}

zx_status_t GpioImplProxy::SetDriveStrength(uint32_t index, uint64_t ds_ua,
                                            uint64_t* out_actual_ua) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.SetDriveStrength(index, ds_ua, out_actual_ua);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->SetDriveStrength(index, ds_ua);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_actual_ua = result->value()->actual_ds_ua;
  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = GpioRootDevice::Create;
  return ops;
}();

}  // namespace gpio

ZIRCON_DRIVER(gpio, gpio::driver_ops, "zircon", "0.1");
