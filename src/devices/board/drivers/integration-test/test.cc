// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <fidl/fuchsia.board.test/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <stdint.h>
#include <stdlib.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>

#include <memory>

#include <ddk/metadata/test.h>
#include <ddktl/device.h>
#include <fbl/array.h>
#include <fbl/vector.h>

#include "fidl/fuchsia.hardware.platform.bus/cpp/markers.h"
#include "src/devices/board/drivers/integration-test/test-bus-bind.h"

namespace board_test {
namespace fpbus = fuchsia_hardware_platform_bus;

class TestBoard;
using TestBoardType = ddk::Device<TestBoard, ddk::Messageable<fuchsia_board_test::Board>::Mixin>;

// This is the main class for the platform bus driver.
class TestBoard : public TestBoardType {
 public:
  explicit TestBoard(zx_device_t* parent, fdf::ClientEnd<fpbus::PlatformBus> pbus)
      : TestBoardType(parent), pbus_(std::move(pbus)) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  void CreateDevice(CreateDeviceRequestView request,
                    CreateDeviceCompleter::Sync& completer) override {
    fpbus::Node device = {};
    device.name() = request->entry.name.get();
    device.vid() = request->entry.vid;
    device.pid() = request->entry.pid;
    device.did() = request->entry.did;

    std::vector<fpbus::Metadata> metadata{[&]() {
      fpbus::Metadata ret;
      ret.type() = DEVICE_METADATA_TEST;
      ret.data() =
          std::vector<uint8_t>(request->entry.metadata.begin(), request->entry.metadata.end());
      return ret;
    }()};
    device.metadata() = std::move(metadata);

    fidl::Arena<> fidl_arena;
    auto result = pbus_.buffer(fdf::Arena('TEST'))->NodeAdd(fidl::ToWire(fidl_arena, device));
    if (!result.ok()) {
      zxlogf(ERROR, "%s: add node request failed: %s", __func__, result.FormatDescription().data());
    } else if (result->is_error()) {
      zxlogf(ERROR, "%s: add node failed: %s", __func__,
             zx_status_get_string(result->error_value()));
    }

    completer.Reply();
  }

  // Device protocol implementation.
  void DdkRelease();

 private:
  TestBoard(const TestBoard&) = delete;
  TestBoard& operator=(const TestBoard&) = delete;
  TestBoard(TestBoard&&) = delete;
  TestBoard& operator=(TestBoard&&) = delete;

  // Fetches devices to load from metadata and deserializes into a vector of
  // pbus_dev_t.
  zx_status_t FetchAndDeserialize();
  zx_status_t Start();
  int Thread();

  fbl::Vector<fpbus::Node> devices_;

  // TODO(https://fxbug.dev/42059490): Switch to fdf::SyncClient when it's available.
  fdf::WireSyncClient<fpbus::PlatformBus> pbus_;
  thrd_t thread_;
};

void TestBoard::DdkRelease() { delete this; }

// This function must be kept updated with the function that serializes the date.
// This function is driver_integration_test::GetBootItem.
zx_status_t TestBoard::FetchAndDeserialize() {
  size_t metadata_size;
  zx_status_t status = DdkGetMetadataSize(DEVICE_METADATA_BOARD_PRIVATE, &metadata_size);
  if (status != ZX_OK) {
    return status;
  }
  if (metadata_size < sizeof(DeviceList)) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  fbl::AllocChecker ac;
  std::vector<uint8_t> metadata_bytes(metadata_size);

  size_t actual;
  status =
      DdkGetMetadata(DEVICE_METADATA_BOARD_PRIVATE, metadata_bytes.data(), metadata_size, &actual);
  if (status != ZX_OK) {
    return status;
  }
  if (actual != metadata_size) {
    return ZX_ERR_INTERNAL;
  }

  const auto* device_list = reinterpret_cast<DeviceList*>(metadata_bytes.data());
  if (metadata_size < sizeof(DeviceList) + device_list->count * sizeof(DeviceEntry)) {
    return ZX_ERR_INTERNAL;
  }

  devices_.reserve(device_list->count, &ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  size_t metadata_offset = (device_list->count * sizeof(DeviceEntry)) + sizeof(DeviceList);
  for (size_t i = 0; i < device_list->count; i++) {
    const auto& entry = device_list->list[i];
    // Create the device.
    fpbus::Node device = {};
    device.name() = entry.name;
    device.vid() = entry.vid;
    device.pid() = entry.pid;
    device.did() = entry.did;

    // Create the metadata.
    fpbus::Metadata metadata = {};
    metadata.type() = DEVICE_METADATA_TEST;
    metadata.data() =
        std::vector<uint8_t>(metadata_bytes.data() + metadata_offset,
                             metadata_bytes.data() + metadata_offset + entry.metadata_size);
    metadata_offset += entry.metadata_size;

    // Store the metadata and link the device to it.
    device.metadata() = {};
    device.metadata()->emplace_back(std::move(metadata));

    devices_.push_back(std::move(device));
  }

  // Inform the platform bus of our bootloader info.
  // This is set to "coreboot" specifically for CrosDevicePartitionerTests.
  fpbus::BootloaderInfo bootloader_info;
  bootloader_info.vendor() = "coreboot";
  fidl::Arena<> fidl_arena;
  auto result = pbus_.buffer(fdf::Arena('BOOT'))
                    ->SetBootloaderInfo(fidl::ToWire(fidl_arena, bootloader_info));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: SetBootloaderInfo request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: SetBootloaderInfo failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "SetBootloaderInfo failed: %d", status);
    return status;
  }

  return ZX_OK;
}

int TestBoard::Thread() {
  for (const auto& device : devices_) {
    fidl::Arena<> fidl_arena;
    auto result = pbus_.buffer(fdf::Arena('ADDN'))->NodeAdd(fidl::ToWire(fidl_arena, device));
    if (!result.ok()) {
      zxlogf(ERROR, "%s: NodeAdd request failed: %s", __func__, result.FormatDescription().data());
    } else if (result->is_error()) {
      zxlogf(ERROR, "%s: NodeAdd failed: %s", __func__,
             zx_status_get_string(result->error_value()));
    }
  }
  return 0;
}

zx_status_t TestBoard::Start() {
  int rc = thrd_create_with_name(
      &thread_, [](void* arg) -> int { return reinterpret_cast<TestBoard*>(arg)->Thread(); }, this,
      "test-board-start-thread");
  if (rc != thrd_success) {
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

zx_status_t TestBoard::Create(void* ctx, zx_device_t* parent) {
  auto endpoints = fdf::CreateEndpoints<fpbus::PlatformBus>();
  if (endpoints.is_error()) {
    return endpoints.error_value();
  }

  zx_status_t status = device_connect_runtime_protocol(
      parent, fpbus::Service::PlatformBus::ServiceName, fpbus::Service::PlatformBus::Name,
      endpoints->server.TakeHandle().release());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to connect to platform bus: %s", zx_status_get_string(status));
    return status;
  }

  auto board = std::make_unique<TestBoard>(parent, std::move(endpoints->client));

  status = board->FetchAndDeserialize();
  if (status != ZX_OK) {
    zxlogf(ERROR, "TestBoard::Create: FetchAndDeserialize failed: %d", status);
    return status;
  }

  status = board->DdkAdd("test-board", DEVICE_ADD_NON_BINDABLE);
  if (status != ZX_OK) {
    zxlogf(ERROR, "TestBoard::Create: DdkAdd failed: %d", status);
    return status;
  }

  status = board->Start();
  if (status == ZX_OK) {
    // devmgr is now in charge of the device.
    [[maybe_unused]] auto* dummy = board.release();
  }

  return status;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = TestBoard::Create;
  return ops;
}();

}  // namespace board_test

ZIRCON_DRIVER(test_bus, board_test::driver_ops, "zircon", "0.1");
