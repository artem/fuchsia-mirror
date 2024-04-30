// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_FTDI_FTDI_H_
#define SRC_DEVICES_SERIAL_DRIVERS_FTDI_FTDI_H_

#include <fidl/fuchsia.hardware.ftdi/cpp/wire.h>
#include <fuchsia/hardware/serialimpl/cpp/banjo.h>
#include <fuchsia/hardware/usb/c/banjo.h>
#include <lib/ddk/device.h>
#include <lib/stdcompat/span.h>
#include <lib/sync/completion.h>
#include <lib/zx/time.h>

#include <thread>

#include <ddktl/device.h>
#include <fbl/mutex.h>
#include <usb/request-cpp.h>
#include <usb/usb-request.h>
#include <usb/usb.h>

namespace ftdi_serial {

constexpr uint16_t kFtdiTypeR = 0x0600;
constexpr uint16_t kFtdiTypeBm = 0x0400;
constexpr uint16_t kFtdiTypeAm = 0x0200;
constexpr uint16_t kFtdiType2232c = 0x0500;
constexpr uint16_t kFtdiType2232h = 0x0700;
constexpr uint16_t kFtdiType4232h = 0x0800;
constexpr uint16_t kFtdiType232h = 0x0900;

// Clock divisors.
constexpr uint32_t kFtdiTypeRDivisor = 16;
constexpr uint32_t kFtdiHClk = 120000000;
constexpr uint32_t kFtdiCClk = 48000000;

// Usb binding rules.
constexpr uint32_t kFtdiUsbVid = 0x0403;
constexpr uint32_t kFtdiUsb232rPid = 0x6001;
constexpr uint32_t kFtdiUsb2232Pid = 0x6010;
constexpr uint32_t kFtdiUsb232hPid = 0x6014;

// Reset the port.
constexpr uint8_t kFtdiSioReset = 0;

// Set the modem control register.
constexpr uint8_t kFtdiSioModemCtrl = 1;

// Set flow control register.
constexpr uint8_t kFtdiSioSetFlowCtrl = 2;

// Set baud rate.
constexpr uint8_t kFtdiSioSetBaudrate = 3;

// Set the data characteristics of the port.
constexpr uint8_t kFtdiSioSetData = 4;

// Set the bitmode.
constexpr uint8_t kFtdiSioSetBitmode = 0x0B;

// Requests.
constexpr uint8_t kFtdiSioResetRequest = kFtdiSioReset;
constexpr uint8_t kFtdiSioSetBaudrateRequest = kFtdiSioSetBaudrate;
constexpr uint8_t kFtdiSioSetDataRequest = kFtdiSioSetData;
constexpr uint8_t kFtdiSioSetFlowCtrlRequest = kFtdiSioSetFlowCtrl;
constexpr uint8_t kFtdiSioSetModemCtrlRequest = kFtdiSioModemCtrl;
constexpr uint8_t kFtdiSioPollModemStatusRequest = 0x05;
constexpr uint8_t kFtdiSioSetEventCharRequest = 0x06;
constexpr uint8_t kFtdiSioSetErrorCharRequest = 0x07;
constexpr uint8_t kFtdiSioSetLatencyTimerRequest = 0x09;
constexpr uint8_t kFtdiSioGetLatencyTimerRequest = 0x0A;
constexpr uint8_t kFtdiSioSetBitmodeRequest = 0x0B;
constexpr uint8_t kFtdiSioReadPinsRequest = 0x0C;
constexpr uint8_t kFtdiSioReadEepromRequest = 0x90;
constexpr uint8_t kFtdiSioWriteEepromRequest = 0x91;
constexpr uint8_t kFtdiSioEraseEepromRequest = 0x92;

class FtdiSerial {
 public:
  // Synchronously read len bytes into buf.
  virtual zx_status_t Read(uint8_t* buf, size_t len) = 0;

  // Synchronously write len bytes from buf.
  virtual zx_status_t Write(uint8_t* buf, size_t len) = 0;
};

class FtdiDevice;
using DeviceType = ddk::Device<FtdiDevice, ddk::Unbindable,
                               ddk::Messageable<fuchsia_hardware_ftdi::Device>::Mixin>;
class FtdiDevice : public DeviceType,
                   public ddk::SerialImplProtocol<FtdiDevice, ddk::base_protocol>,
                   public FtdiSerial {
 public:
  explicit FtdiDevice(zx_device_t* parent) : DeviceType(parent), usb_client_(parent) {}
  ~FtdiDevice();

  zx_status_t Bind();

  void DdkUnbind(ddk::UnbindTxn txn);

  void DdkRelease();

  static zx_status_t Bind(zx_device_t* device);

  // |ddk::SerialImpl|
  zx_status_t SerialImplGetInfo(serial_port_info_t* info);

  // |ddk::SerialImpl|
  zx_status_t SerialImplConfig(uint32_t baud_rate, uint32_t flags);

  // |ddk::SerialImpl|
  zx_status_t SerialImplEnable(bool enable);

  // |ddk::SerialImpl|
  zx_status_t SerialImplRead(uint8_t* data, size_t len, size_t* actual);

  // |ddk::SerialImpl|
  zx_status_t SerialImplWrite(const uint8_t* buf, size_t length, size_t* actual);

  // |ddk::SerialImpl|
  zx_status_t SerialImplSetNotifyCallback(const serial_notify_t* cb);

  // |FtdiSerial::Read|
  zx_status_t Read(uint8_t* buf, size_t len) override;

  // |FtdiSerial::Write|
  zx_status_t Write(uint8_t* buf, size_t len) override;

 private:
  static constexpr zx::duration kSerialReadWriteTimeout = zx::sec(1);

  void CreateI2C(CreateI2CRequestView request, CreateI2CCompleter::Sync& _completer) override;

  static zx_status_t FidlCreateI2c(void* ctx,
                                   const fuchsia_hardware_ftdi::wire::I2cBusLayout* layout,
                                   const fuchsia_hardware_ftdi::wire::I2cDevice* device);
  zx_status_t Reset();
  zx_status_t SetBaudrate(uint32_t baudrate);
  zx_status_t CalcDividers(uint32_t* baudrate, uint32_t clock, uint32_t divisor,
                           uint16_t* integer_div, uint16_t* fraction_div);
  void WriteComplete(usb_request_t* request);
  void ReadComplete(usb_request_t* request);
  // Copies starting at request_offset bytes into request, and returns the number of bytes copied.
  static size_t CopyFromRequest(usb::Request<>& request, size_t request_offset,
                                cpp20::span<uint8_t> buffer);
  size_t ReadAtMost(uint8_t* buffer, size_t len) __TA_REQUIRES(mutex_);
  // Writes as much data as possible with a single USB request, and returns a subspan pointing to
  // the remaining data that did not fit into the request.
  cpp20::span<const uint8_t> QueueWriteRequest(cpp20::span<const uint8_t> data, usb::Request<> req)
      __TA_REQUIRES(mutex_);

  // Notifies the callback if the state is updated (|need_to_notify_cb| is true), and
  // resets |need_to_notify_cb| to false.
  void NotifyCallback() __TA_EXCLUDES(mutex_);

  // Checks the readable and writeable state of the system. Updates |state_| and
  // |need_to_notify_cb|. Any caller of this is responsible for calling NotifyCallback
  // once the lock is released.
  void CheckStateLocked() __TA_REQUIRES(mutex_);
  zx_status_t SetBitMode(uint8_t line_mask, uint8_t mode);

  ddk::UsbProtocolClient usb_client_ = {};

  uint16_t ftditype_ = 0;
  uint32_t baudrate_ = 0;

  uint32_t state_ = 0;

  size_t read_offset_ = 0;

  size_t parent_req_size_ = 0;
  uint8_t bulk_in_addr_ = 0;
  uint8_t bulk_out_addr_ = 0;

  fbl::Mutex mutex_ = {};

  // pool of free USB requests
  usb::RequestQueue<> free_read_queue_ __TA_GUARDED(mutex_);
  usb::RequestQueue<> free_write_queue_ __TA_GUARDED(mutex_);
  // list of received packets not yet read by upper layer
  usb::RequestQueue<> completed_reads_queue_ __TA_GUARDED(mutex_);

  serial_port_info_t serial_port_info_ __TA_GUARDED(mutex_);
  bool need_to_notify_cb_ = false;
  serial_notify_t notify_cb_ = {};
  std::thread cancel_thread_;

  sync_completion_t serial_readable_;
  sync_completion_t serial_writable_;
};

}  // namespace ftdi_serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_FTDI_FTDI_H_
