# Bluetooth Virtual Driver

## About

The Bluetooth virtual driver is a Bluetooth controller emulator that allows
system Bluetooth components to be tested in integration against the Bluetooth
HCI protocol.

The virtual driver currently supports the LoopbackDevice and EmulatorDevice.

The driver is created by the VirtualController class. VirtualController uses
DFv2 to create a virtual driver node (bt-hci-virtual) that can then have
children nodes (LoopbackDevice and/or EmulatorDevice). This allows the DFv2
logic to be contained to VirtualController.

## LoopbackDevice

LoopbackDevice is used by RootCanal, which is used by PTS-bot. The step numbers
below are correlated with the numbers in the diagram.

### Steps

1. `ffx emu start` creates bt-hci-emulator device
2. VirtualController driver binds to bt-hci-emulator device (specified in
   [bt-hci-virtual.bind](//src/connectivity/bluetooth/hci/virtual/bt-hci-virtual.bind))
3. VirtualController driver is started by DFv2
4. VirtualController is added as a device node (bt-hci-virtual) to the node
   graph inside `/dev/sys/platform/00:00:30`
5. RootCanal detects bt-hci-virtual in `/dev/sys/platform/00:00:30` and calls
   [`create_loopback_device()`](https://source.corp.google.com/h/turquoise-internal/turquoise/+/main:src/connectivity/bluetooth/tools/bt-rootcanal/src/main.rs;l=91)
6. bt-hci-virtual uses the LoopbackDevice class to add a device node (loopback)
   to the node graph inside `/dev/class/bt-hci`
7. bt-init detects the LoopbackDevice in `/dev/class/bt-hci` and creates a new
   bt-host component

```
                                  2. bt-hci-virtual.bind dictates
        ┌───────────────────────┐ that bt-hci-virtual will bind   ┌───────────────────┐
        │    bt-hci-emulator    ├─────────────────────────────────│ VirtualController |
        │ 1. Created by ffx emu │ to bt-hci-emulator              └─────────┬─────────┘
        └───────────┬───────────┘                                           │
                    │                                                       │
                    │                                                       │
                    ▼                           3. DriverBase::Start()      |
┌─────────────────────────────────────────┐◄────────────────────────────────┘
│             bt-hci-virtual              │
│ 4. Added to /dev/sys/platform/00:00:30/ │   5. create_loopback_device()
└───────────────────┬─────────────────────┘◄────────────────────────────────┐
                    │                                                       │
                    │                                                       |
                    ▼                                                ┌──────────────┐
    ┌───────────────────────────────┐                                │ bt-rootcanal │
    │          loopback             │                                |    Server    |
    │ 6. Added to /dev/class/bt-hci │                                └──────────────┘
    └───────────────┬───────────────┘
                    │
                    │
                    ▼
        ┌────────────────────┐
        │      bt-host       │
        └────────────────────┘
```

## EmulatorDevice

EmulatorDevice is used for Bluetooth integration tests. The
[fuchsia.hardware.bluetooth.Emulator](//sdk/fidl/fuchsia.hardware.bluetooth/virtual.fidl)
API is used to establish and interact with the EmulatorDevice. The step numbers
below are correlated with the numbers in the diagram. Please note that the term
“emulator” is heavily overloaded, so it’s important to be precise when using the
term.

### Steps

1. `ffx emu start` creates bt-hci-emulator device
2. VirtualController driver binds to bt-hci-emulator device (specified in
   [bt-hci-virtual.bind](//src/connectivity/bluetooth/hci/virtual/bt-hci-virtual.bind))
3. VirtualController driver is started by DFv2
4. VirtualController is added as a device node (bt-hci-virtual) to the node
   graph inside `/dev/sys/platform/00:00:30`
5. The hci-emulator-client detects bt-hci-virtual in
   `/dev/sys/platform/00:00:30` and calls
   [`create_emulator()`](https://source.corp.google.com/h/turquoise-internal/turquoise/+/main:src/connectivity/bluetooth/testing/hci-emulator-client/src/lib.rs;l=199)
6. bt-hci-virtual uses the EmulatorDevice class to add a device node (emulator)
   to the node graph inside `/dev/class/bt-emulator`
7. Use the Emulator.Publish method publishes a bt-hci device as a child of the
   EmulatorDevice
8. The EmulatorDevice adds a device node (bt-hci-device) to the node graph
   inside `/dev/class/bt-hci`
9. Other Emulator API calls can be made
10. bt-init detects bt-hci-device in `/dev/class/bt-hci` and creates a new
   bt-host component using a FakeController to mimic the BT controller

```
                                  2. bt-hci-virtual.bind dictates
        ┌───────────────────────┐ that bt-hci-virtual will bind   ┌───────────────────┐
        │    bt-hci-emulator    ├─────────────────────────────────│ VirtualController |
        │ 1. Created by ffx emu │ to bt-hci-emulator              └─────────┬─────────┘
        └───────────┬───────────┘                                           │
                    │                                                       │
                    │                                                       │
                    ▼                           3. DriverBase::Start()      |
┌─────────────────────────────────────────┐◄────────────────────────────────┘
│             bt-hci-virtual              │
│ 4. Added to /dev/sys/platform/00:00:30/ │      5. create_emulator()
└───────────────────┬─────────────────────┘◄────────────────────────────────┐
                    │                                                       │
                    │                                                       |
┌───────────────────────────────────────────┐                               |
| EmulatorDevice class                      |                               |
|                   |                       |                               |
|                   ▼                       |                    ┌─────────────────────┐
|   ┌────────────────────────────────────┐  |                    │ hci-emulator-client │
|   │            emulator                │  |                    └──────────┬──────────┘
|   │ 6. Added to /dev/class/bt-emulator │  |     7. Emulator::Publish()    |
|   │                                 │◄─|───────────────────────────────┘
|   └───────────────┬────────────────────┘◄─|───────────────────────────────┐
|                   │                       |  9 . Other Emulator API calls |
|                   │                       |                               |
|                   ▼                       |                    ┌──────────────────────┐
|   ┌───────────────────────────────┐       |                    | BT Integration Tests |
|   │          bt-hci-device        │       |                    └──────────────────────┘
|   │ 8. Added to /dev/class/bt-hci |       |
|   └───────────────┬───────────────┘       |
|                   │                       |
|                   │                       |
|                   ▼                       |
|       ┌────────────────────┐              |
|       │      bt-host       │              |
|       │  ┌──────────────┐  │              |
|       |  |FakeController|  |              |
|       |  └──────────────┘  |              |
|       └────────────────────┘              |
└───────────────────────────────────────────┘
```

## Library Utilities

TODO(<https://fxbug.dev/42162739>): Document Rust library utilities.
