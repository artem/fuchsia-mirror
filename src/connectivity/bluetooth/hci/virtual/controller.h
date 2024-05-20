// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_CONTROLLER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_CONTROLLER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "src/connectivity/bluetooth/hci/virtual/emulator.h"
#include "src/connectivity/bluetooth/hci/virtual/loopback.h"

namespace bt_hci_virtual {

// The VirtualController class implements the fuchsia.hardware.bluetooth.VirtualController API. It
// is used to created two devices: the EmulatorDevice and/or the LoopbackDevice. The EmulatorDevice
// is used for Bluetooth integration tests, and the LoopbackDevice is used by RootCanal for PTS-bot.
//
// VirtualController publishes itself as a DFv2 driver and starts a device node to bind to said
// driver. It can create a EmulatorDevice/LoopbackDevice which can then use the
// VirtualController's child node |virtual_controller_child_node_| to publish its own
// children node that would represent the EmulatorDevice/LoopbackDevice.
//
// EmulatorDevice/LoopbackDevice then implements and serves the FIDL protocols that their client
// needs. For more details, refer to go/bluetooth-virtual-driver-doc.
class VirtualController
    : public fdf::DriverBase,
      public fidl::WireAsyncEventHandler<fuchsia_driver_framework::NodeController>,
      public fidl::WireAsyncEventHandler<fuchsia_driver_framework::Node>,
      public fidl::WireServer<fuchsia_hardware_bluetooth::VirtualController> {
 public:
  explicit VirtualController(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  // fdf::DriverBase overrides:
  zx::result<> Start() override;

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::Node> metadata) override {}
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}

 private:
  // fuchsia_hardware_bluetooth::VirtualController overrides:
  void CreateEmulator(CreateEmulatorCompleter::Sync& completer) override;
  void CreateLoopbackDevice(CreateLoopbackDeviceRequestView request,
                            CreateLoopbackDeviceCompleter::Sync& completer) override;

  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::VirtualController> request);

  // Helpers functions to add DFv2 device nodes
  zx_status_t AddVirtualControllerChildNode(fuchsia_driver_framework::wire::NodeAddArgs args);
  zx_status_t AddLoopbackChildNode(fuchsia_driver_framework::wire::NodeAddArgs args);
  zx_status_t AddEmulatorChildNode(fuchsia_driver_framework::wire::NodeAddArgs args,
                                   EmulatorDevice* emulator_device);

  std::unique_ptr<EmulatorDevice> emulator_device_;
  std::unique_ptr<LoopbackDevice> loopback_device_;

  // VirtualController
  fidl::WireClient<fuchsia_driver_framework::Node> node_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> virtual_controller_child_node_;
  driver_devfs::Connector<fuchsia_hardware_bluetooth::VirtualController> devfs_connector_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::VirtualController>
      virtual_controller_binding_group_;

  // LoopbackDevice
  fidl::WireClient<fuchsia_driver_framework::NodeController> loopback_node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> loopback_child_node_;

  // EmulatorDevice
  fidl::WireClient<fuchsia_driver_framework::NodeController> emulator_node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> emulator_child_node_;
};

}  // namespace bt_hci_virtual

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_CONTROLLER_H_
