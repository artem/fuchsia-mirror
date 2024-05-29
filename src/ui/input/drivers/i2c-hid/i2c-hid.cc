// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c-hid.h"

#include <endian.h>
#include <fidl/fuchsia.hardware.acpi/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/trace/event.h>
#include <lib/zx/time.h>
#include <stdbool.h>
#include <stdlib.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <vector>

#include <fbl/auto_lock.h>

#include "src/devices/lib/acpi/client.h"
#include "src/devices/lib/fragment-irq/dfv1/fragment-irq.h"

namespace i2c_hid {

namespace fhidbus = fuchsia_hardware_hidbus;

namespace {

// Sets the bytes for an I2c command. This requires there to be at least 5 bytes in |command_bytes|.
// Also |command_register| must be in the host format. Returns the number of bytes set.
constexpr uint8_t SetI2cCommandBytes(uint16_t command_register, uint8_t command,
                                     uint8_t command_data, uint8_t* command_bytes) {
  // Setup command_bytes as little endian.
  command_bytes[0] = static_cast<uint8_t>(command_register & 0xff);
  command_bytes[1] = static_cast<uint8_t>(command_register >> 8);
  command_bytes[2] = command_data;
  command_bytes[3] = command;
  return 4;
}

// Set the command bytes for a HID GET/SET command. Returns the number of bytes set.
static uint8_t SetI2cGetSetCommandBytes(uint16_t command_register, uint8_t command,
                                        fuchsia_hardware_input::ReportType rpt_type, uint8_t rpt_id,
                                        uint8_t* command_bytes) {
  uint8_t command_bytes_index = 0;
  uint8_t command_data = static_cast<uint8_t>(static_cast<uint8_t>(rpt_type) << 4U);
  if (rpt_id < 0xF) {
    command_data |= rpt_id;
  } else {
    command_data |= 0x0F;
  }
  command_bytes_index += SetI2cCommandBytes(command_register, command, command_data, command_bytes);
  if (rpt_id >= 0xF) {
    command_bytes[command_bytes_index++] = rpt_id;
  }
  return command_bytes_index;
}

}  // namespace

// Poll interval: 10 ms
constexpr zx::duration kI2cPollInterval = zx::msec(10);

// Send the device a HOST initiated RESET.  Caller must call
// i2c_wait_for_ready_locked() afterwards to guarantee completion.
// If |force| is false, do not issue a reset if there is one outstanding.
zx_status_t I2cHidbus::Reset(bool force) {
  uint8_t buf[4];

  uint16_t cmd_reg = letoh16(hiddesc_.wCommandRegister);
  SetI2cCommandBytes(cmd_reg, kResetCommand, 0, buf);
  fbl::AutoLock lock(&i2c_lock_);

  if (!force && i2c_pending_reset_) {
    return ZX_OK;
  }

  i2c_pending_reset_ = true;
  zx_status_t status = i2c_.WriteSync(buf, sizeof(buf));

  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c-hid: could not issue reset: %d", status);
    return status;
  }

  return ZX_OK;
}

// Must be called with i2c_lock held.
void I2cHidbus::WaitForReadyLocked() {
  while (i2c_pending_reset_) {
    i2c_reset_cnd_.Wait(&i2c_lock_);
  }
}

void I2cHidbus::GetReport(fhidbus::wire::HidbusGetReportRequest* request,
                          GetReportCompleter::Sync& completer) {
  uint16_t cmd_reg = letoh16(hiddesc_.wCommandRegister);
  uint16_t data_reg = letoh16(hiddesc_.wDataRegister);
  uint8_t command_bytes[7];
  uint8_t command_bytes_index = 0;

  // Set the command bytes.
  command_bytes_index += SetI2cGetSetCommandBytes(cmd_reg, kGetReportCommand, request->rpt_type,
                                                  request->rpt_id, command_bytes);
  command_bytes[command_bytes_index++] = static_cast<uint8_t>(data_reg & 0xff);
  command_bytes[command_bytes_index++] = static_cast<uint8_t>(data_reg >> 8);

  fbl::AutoLock lock(&i2c_lock_);

  // Send the command and read back the length of the response.
  std::vector<uint8_t> report(request->len + 2);
  zx_status_t status =
      i2c_.WriteReadSync(command_bytes, command_bytes_index, report.data(), report.size());
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c-hid: could not issue get_report: %d", status);
    completer.ReplyError(status);
    return;
  }

  uint16_t response_len = static_cast<uint16_t>(report[0] + (report[1] << 8U));
  if (response_len != request->len + 2) {
    zxlogf(ERROR, "i2c-hid: response_len %d != len: %ld", response_len, request->len + 2);
  }

  uint16_t report_len = response_len - 2;
  if (report_len > request->len) {
    completer.ReplyError(ZX_ERR_BUFFER_TOO_SMALL);
    return;
  }
  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(report.data() + 2, report_len));
}

void I2cHidbus::SetReport(fhidbus::wire::HidbusSetReportRequest* request,
                          SetReportCompleter::Sync& completer) {
  uint16_t cmd_reg = letoh16(hiddesc_.wCommandRegister);
  uint16_t data_reg = letoh16(hiddesc_.wDataRegister);

  // The command bytes are the 6 or 7 bytes for the command, 2 bytes for the report's size,
  // and then the full report.
  std::vector<uint8_t> command_bytes(7 + 2 + request->data.count());
  uint8_t command_bytes_index = 0;

  // Set the command bytes.
  command_bytes_index += SetI2cGetSetCommandBytes(cmd_reg, kSetReportCommand, request->rpt_type,
                                                  request->rpt_id, command_bytes.data());

  command_bytes[command_bytes_index++] = static_cast<uint8_t>(data_reg & 0xff);
  command_bytes[command_bytes_index++] = static_cast<uint8_t>(data_reg >> 8);
  // Set the bytes for the report's size.
  command_bytes[command_bytes_index++] = static_cast<uint8_t>((request->data.count() + 2) & 0xff);
  command_bytes[command_bytes_index++] = static_cast<uint8_t>((request->data.count() + 2) >> 8);
  // Set the bytes for the report.
  memcpy(command_bytes.data() + command_bytes_index, request->data.data(), request->data.count());
  command_bytes_index += request->data.count();

  fbl::AutoLock lock(&i2c_lock_);

  // Send the command.
  zx_status_t status = i2c_.WriteSync(command_bytes.data(), command_bytes_index);
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c-hid: could not issue set_report: %d", status);
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void I2cHidbus::Query(QueryCompleter::Sync& completer) {
  fidl::Arena<> arena;
  auto info = fhidbus::wire::HidInfo::Builder(arena)
                  .dev_num(0)
                  .boot_protocol(fhidbus::wire::HidBootProtocol::kNone)
                  .vendor_id(hiddesc_.wVendorID)
                  .product_id(hiddesc_.wProductID)
                  .version(hiddesc_.wVersionID);
  if (!irq_.is_valid()) {
    info.polling_rate(kI2cPollInterval.to_usecs());
  }
  completer.ReplySuccess(info.Build());
}

void I2cHidbus::Start(StartCompleter::Sync& completer) {
  if (started_) {
    zxlogf(ERROR, "Already started");
    completer.ReplyError(ZX_ERR_ALREADY_BOUND);
    return;
  }

  started_ = true;
  completer.ReplySuccess();
}

void I2cHidbus::Stop(StopCompleter::Sync& completer) { Stop(); }

void I2cHidbus::Stop() { started_ = false; }

void I2cHidbus::GetDescriptor(fhidbus::wire::HidbusGetDescriptorRequest* request,
                              GetDescriptorCompleter::Sync& completer) {
  if (request->desc_type != fhidbus::wire::HidDescriptorType::kReport) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }

  fbl::AutoLock lock(&i2c_lock_);
  WaitForReadyLocked();

  size_t desc_len = letoh16(hiddesc_.wReportDescLength);
  uint16_t desc_reg = letoh16(hiddesc_.wReportDescRegister);
  uint16_t buf = htole16(desc_reg);

  std::vector<uint8_t> desc(desc_len);
  zx_status_t status =
      i2c_.WriteReadSync(reinterpret_cast<uint8_t*>(&buf), sizeof(uint16_t), desc.data(), desc_len);
  if (status < 0) {
    zxlogf(ERROR, "i2c-hid: could not read HID report descriptor from reg 0x%04x: %d", desc_reg,
           status);
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(desc.data(), desc.size()));
}

// TODO(teisenbe/tkilbourn): Remove this once we pipe IRQs from ACPI
int I2cHidbus::WorkerThreadNoIrq() {
  zxlogf(INFO, "i2c-hid: using noirq");

  zx_status_t status = Reset(true);
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c-hid: failed to reset i2c device");
    return 0;
  }

  uint16_t len = letoh16(hiddesc_.wMaxInputLength);
  uint8_t* buf = static_cast<uint8_t*>(malloc(len));
  uint16_t report_len = 0;

  // Last report received, so we can deduplicate.  This is only necessary since
  // we haven't wired through interrupts yet, and some devices always return
  // the last received report when you attempt to read from them.
  uint8_t* last_report = static_cast<uint8_t*>(malloc(len));
  size_t last_report_len = 0;
  // Skip deduplicating for the first read.
  bool dedupe = false;

  zx_time_t last_timeout_warning = 0;
  const zx_duration_t kMinTimeBetweenWarnings = ZX_SEC(10);

  // Until we have a way to map the GPIO associated with an i2c slave to an
  // IRQ, we just poll.
  while (!stop_worker_thread_) {
    usleep(kI2cPollInterval.to_usecs());
    TRACE_DURATION("input", "Device Read");

    last_report_len = report_len;

    // Swap buffers
    uint8_t* tmp = last_report;
    last_report = buf;
    buf = tmp;

    {
      fbl::AutoLock lock(&i2c_lock_);

      // Perform a read with no register address.
      status = i2c_.WriteReadSync(nullptr, 0, buf, len);
      if (status != ZX_OK) {
        if (status == ZX_ERR_TIMED_OUT) {
          zx_time_t now = zx_clock_get_monotonic();
          if (now - last_timeout_warning > kMinTimeBetweenWarnings) {
            zxlogf(DEBUG, "i2c-hid: device_read timed out");
            last_timeout_warning = now;
          }
          continue;
        }
        zxlogf(ERROR, "i2c-hid: device_read failure %d", status);
        continue;
      }

      report_len = letoh16(*(uint16_t*)buf);

      // Check for duplicates.  See comment by |last_report| definition.
      if (dedupe && last_report_len == report_len && report_len <= len &&
          !memcmp(buf, last_report, report_len)) {
        continue;
      }

      dedupe = true;

      if (report_len == 0x0) {
        zxlogf(DEBUG, "i2c-hid reset detected");
        // Either host or device reset.
        i2c_pending_reset_ = false;
        i2c_reset_cnd_.Broadcast();
        continue;
      }

      if (i2c_pending_reset_) {
        zxlogf(INFO, "i2c-hid: received event while waiting for reset? %u", report_len);
        continue;
      }

      if ((report_len == 0xffff) || (report_len == 0x3fff)) {
        // nothing to read
        continue;
      }

      if ((report_len < 2) || (report_len > len)) {
        zxlogf(ERROR, "i2c-hid: bad report len (rlen %hu, bytes read %d)!!!", report_len, len);
        continue;
      }
    }

    sync_completion_t wait;
    async::PostTask(dispatcher_, [this, &buf, &report_len, &wait]() {
      if (!(started_ && binding_)) {
        return;
      }
      auto result = fidl::WireSendEvent(*binding_)->OnReportReceived(
          fidl::VectorView<uint8_t>::FromExternal(buf + 2, report_len - 2),
          zx_clock_get_monotonic());
      if (!result.ok()) {
        zxlogf(ERROR, "OnReportReceived failed %s", result.error().FormatDescription().c_str());
      }
      sync_completion_signal(&wait);
    });
    sync_completion_wait(&wait, ZX_TIME_INFINITE);
  }

  free(buf);
  free(last_report);
  return 0;
}

int I2cHidbus::WorkerThreadIrq() {
  zxlogf(DEBUG, "i2c-hid: using irq");

  zx_status_t status = Reset(true);
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c-hid: failed to reset i2c device");
    return 0;
  }

  uint16_t len = letoh16(hiddesc_.wMaxInputLength);
  uint8_t* buf = static_cast<uint8_t*>(malloc(len));

  zx_time_t last_timeout_warning = 0;
  const zx_duration_t kMinTimeBetweenWarnings = ZX_SEC(10);

  while (true) {
    zx::time timestamp;
    zx_status_t status = irq_.wait(&timestamp);
    if (status != ZX_OK) {
      if (status != ZX_ERR_CANCELED) {
        zxlogf(ERROR, "i2c-hid: interrupt wait failed %d", status);
      }
      break;
    }
    if (stop_worker_thread_) {
      break;
    }

    TRACE_DURATION("input", "Device Read");
    uint16_t report_len = 0;
    {
      fbl::AutoLock lock(&i2c_lock_);

      // Perform a read with no register address.
      status = i2c_.WriteReadSync(nullptr, 0, buf, len);
      if (status != ZX_OK) {
        if (status == ZX_ERR_TIMED_OUT) {
          zx_time_t now = zx_clock_get_monotonic();
          if (now - last_timeout_warning > kMinTimeBetweenWarnings) {
            zxlogf(DEBUG, "i2c-hid: device_read timed out");
            last_timeout_warning = now;
          }
          continue;
        }
        zxlogf(ERROR, "i2c-hid: device_read failure %d", status);
        continue;
      }

      report_len = letoh16(*(uint16_t*)buf);
      if (report_len == 0x0) {
        zxlogf(DEBUG, "i2c-hid reset detected");
        // Either host or device reset.
        i2c_pending_reset_ = false;
        i2c_reset_cnd_.Broadcast();
        continue;
      }

      if (i2c_pending_reset_) {
        zxlogf(INFO, "i2c-hid: received event while waiting for reset? %u", report_len);
        continue;
      }

      if ((report_len < 2) || (report_len > len)) {
        zxlogf(ERROR, "i2c-hid: bad report len (report_len %hu, bytes_read %d)!!!", report_len,
               len);
        continue;
      }
    }

    sync_completion_t wait;
    async::PostTask(dispatcher_, [this, &buf, &report_len, &timestamp, &wait]() {
      if (!(started_ && binding_)) {
        return;
      }
      auto result = fidl::WireSendEvent(*binding_)->OnReportReceived(
          fidl::VectorView<uint8_t>::FromExternal(buf + 2, report_len - 2), timestamp.get());
      if (!result.ok()) {
        zxlogf(ERROR, "OnReportReceived failed %s", result.error().FormatDescription().c_str());
      }
      sync_completion_signal(&wait);
    });
    sync_completion_wait(&wait, ZX_TIME_INFINITE);
  }

  free(buf);
  return 0;
}

void I2cHidbus::Shutdown() {
  stop_worker_thread_ = true;
  if (irq_.is_valid()) {
    irq_.destroy();
  }
  if (worker_thread_started_) {
    worker_thread_started_ = false;
    thrd_join(worker_thread_, NULL);
  }
}

void I2cHidbus::DdkUnbind(ddk::UnbindTxn txn) {
  Shutdown();
  txn.Reply();
}

void I2cHidbus::DdkRelease() { delete this; }

zx_status_t I2cHidbus::ReadI2cHidDesc(I2cHidDesc* hiddesc) {
  uint8_t buf[2];
  uint8_t out[4];
  zx_status_t status;

  // Get the address of the HID descriptor from ACPI.
  // https://docs.microsoft.com/en-us/windows-hardware/drivers/bringup/hidi2c-device-specific-method---dsm-
  // i2c-hid DSM UUID: 3cdff6f7-4267-4555-ad05-b30a3d8938de
  auto result = acpi_client_.CallDsm(
      acpi::Uuid::Create(0x3cdff6f7, 0x4267, 0x4555, 0xad05, 0xb30a3d8938de), 1, 1, std::nullopt);
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to EvaluateObject call: %s", result.status_string());
    return result.error_value();
  }

  if (result->is_status()) {
    zxlogf(ERROR, "EvaluateObject failed: 0x%x", fidl::ToUnderlying(result->status_val()));
    return ZX_ERR_INTERNAL;
  }

  if (!result->is_integer()) {
    zxlogf(ERROR, "I2CHID _DSM has wrong return type!");
    return ZX_ERR_WRONG_TYPE;
  }

  uint64_t address = result->integer_val();
  zxlogf(INFO, "i2c-hid using address=0x%lx", address);
  buf[0] = address & 0xff;
  buf[1] = (address >> 8) & 0xff;

  fbl::AutoLock lock(&i2c_lock_);

  status = i2c_.WriteReadSync(buf, sizeof(buf), out, sizeof(out));
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c-hid: could not read HID descriptor: %d", status);
    return ZX_ERR_NOT_SUPPORTED;
  }

  // We can safely cast here because the descriptor length is the first
  // 2 bytes of out.
  uint16_t desc_len = letoh16(*(reinterpret_cast<uint16_t*>(out)));
  if (desc_len > sizeof(I2cHidDesc)) {
    desc_len = sizeof(I2cHidDesc);
  }

  if (desc_len == 0) {
    zxlogf(ERROR, "i2c-hid: could not read HID descriptor: %d", status);
    return ZX_ERR_NOT_SUPPORTED;
  }

  status = i2c_.WriteReadSync(buf, sizeof(buf), reinterpret_cast<uint8_t*>(hiddesc), desc_len);
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c-hid: could not read HID descriptor: %d", status);
    return ZX_ERR_NOT_SUPPORTED;
  }

  zxlogf(DEBUG, "i2c-hid: desc:");
  zxlogf(DEBUG, "  report desc len: %u", letoh16(hiddesc->wReportDescLength));
  zxlogf(DEBUG, "  report desc reg: %u", letoh16(hiddesc->wReportDescRegister));
  zxlogf(DEBUG, "  input reg:       %u", letoh16(hiddesc->wInputRegister));
  zxlogf(DEBUG, "  max input len:   %u", letoh16(hiddesc->wMaxInputLength));
  zxlogf(DEBUG, "  output reg:      %u", letoh16(hiddesc->wOutputRegister));
  zxlogf(DEBUG, "  max output len:  %u", letoh16(hiddesc->wMaxOutputLength));
  zxlogf(DEBUG, "  command reg:     %u", letoh16(hiddesc->wCommandRegister));
  zxlogf(DEBUG, "  data reg:        %u", letoh16(hiddesc->wDataRegister));
  zxlogf(DEBUG, "  vendor id:       %x", hiddesc->wVendorID);
  zxlogf(DEBUG, "  product id:      %x", hiddesc->wProductID);
  zxlogf(DEBUG, "  version id:      %x", hiddesc->wVersionID);

  return ZX_OK;
}

zx_status_t I2cHidbus::Bind(ddk::I2cChannel i2c) {
  zx_status_t status;

  {
    fbl::AutoLock lock(&i2c_lock_);
    i2c_ = std::move(i2c);
  }
  auto irq_result = fragment_irq::GetInterrupt(parent_, 0u);
  if (irq_result.is_ok()) {
    irq_ = std::move(*irq_result);
  } else {
    zxlogf(WARNING, "Failed to map IRQ: %s", irq_result.status_string());
  }

  dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  auto result = outgoing_.AddService<fhidbus::Service>(fhidbus::Service::InstanceHandler({
      .device =
          [this](fidl::ServerEnd<fhidbus::Hidbus> server_end) {
            if (binding_) {
              server_end.Close(ZX_ERR_ALREADY_BOUND);
              return;
            }
            binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                             std::move(server_end), this, [this](fidl::UnbindInfo info) {
                               Stop();
                               binding_.reset();
                             });
          },
  }));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add Hidbus protocol: %s", result.status_string());
    return result.status_value();
  }
  auto [directory_client, directory_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  result = outgoing_.Serve(std::move(directory_server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to service the outgoing directory");
    return result.status_value();
  }

  std::array offers = {
      fhidbus::Service::Name,
  };
  status = DdkAdd(ddk::DeviceAddArgs("i2c-hid").set_fidl_service_offers(offers).set_outgoing_dir(
      directory_client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %d", status);
    return status;
  }
  return ZX_OK;
}

void I2cHidbus::DdkInit(ddk::InitTxn txn) {
  init_txn_ = std::move(txn);

  auto worker_thread = [](void* arg) -> int {
    auto dev = reinterpret_cast<I2cHidbus*>(arg);
    dev->worker_thread_started_ = true;
    zx_status_t status = ZX_OK;
    // Retry the first transaction a few times; in some cases (e.g. on Slate) the device was powered
    // on explicitly during enumeration, and there is a warmup period after powering on the device
    // during which the device is not responsive over i2c.
    // TODO(jfsulliv): It may make more sense to introduce a delay after powering on the device,
    // rather than here while attempting to bind.
    int retries = 3;
    while (retries-- > 0) {
      if ((status = dev->ReadI2cHidDesc(&dev->hiddesc_)) == ZX_OK) {
        break;
      }
      zx::nanosleep(zx::deadline_after(zx::msec(100)));
      zxlogf(INFO, "i2c-hid: Retrying reading HID descriptor");
    }
    ZX_ASSERT(dev->init_txn_);
    // This will make the device visible and able to be unbound.
    dev->init_txn_->Reply(status);
    // No need to remove the device, as replying to init_txn_ with an error will schedule
    // unbinding.
    if (status != ZX_OK) {
      return thrd_error;
    }

    if (dev->irq_.is_valid()) {
      dev->WorkerThreadIrq();
    } else {
      dev->WorkerThreadNoIrq();
    }
    // If |stop_worker_thread_| is not set, than we exited the worker thread because
    // of an error and not a shutdown. Call DdkAsyncRemove directly. This is a valid
    // call even if the device is currently unbinding.
    if (!dev->stop_worker_thread_) {
      dev->DdkAsyncRemove();
      return thrd_error;
    }
    return thrd_success;
  };

  int rc = thrd_create_with_name(&worker_thread_, worker_thread, this, "i2c-hid-worker-thread");
  if (rc != thrd_success) {
    return init_txn_->Reply(ZX_ERR_INTERNAL);
  }
  // If the worker thread was created successfully, it is in charge of replying to the init txn,
  // which will make the device visible.
}

static zx_status_t i2c_hid_bind(void* ctx, zx_device_t* parent) {
  auto client = acpi::Client::Create(parent);
  if (client.is_error()) {
    zxlogf(ERROR, "i2c-hid: could not get ACPI device");
    return client.error_value();
  }

  auto dev = std::make_unique<I2cHidbus>(parent, std::move(client.value()));

  ddk::I2cChannel i2c(parent, "i2c000");
  if (!i2c.is_valid()) {
    zxlogf(ERROR, "I2c-Hid: Could not get i2c protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t status = dev->Bind(std::move(i2c));
  if (status == ZX_OK) {
    // devmgr is now in charge of the memory for dev.
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

static zx_driver_ops_t i2c_hid_driver_ops = []() {
  zx_driver_ops_t i2c_hid_driver_ops = {};
  i2c_hid_driver_ops.version = DRIVER_OPS_VERSION;
  i2c_hid_driver_ops.bind = i2c_hid_bind;
  return i2c_hid_driver_ops;
}();

}  // namespace i2c_hid

// clang-format off
ZIRCON_DRIVER(i2c_hid, i2c_hid::i2c_hid_driver_ops, "zircon", "0.1");

// clang-format on
