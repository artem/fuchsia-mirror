# bt-gap
This directory contains the implementation of the public Bluetooth
APIs (`fuchsia.bluetooth*`). The main pieces of this
implementation are:
- [HostDevice](src/host_device.rs):
  - Receives FIDL events from `bt-host`, and relays them to mods via
    HostDispatcher.
  - Provides thin wrappers over some of the [private Bluetooth API](/src/connectivity/bluetooth/fidl/host.fidl), for use by HostDispatcher.
- [AccessService](src/services/access.rs): Implements the `fuchsia.bluetooth.sys.Access`
   interface, calling into HostDispatcher for help.
- [PairingService](src/services/pairing.rs): Implements the `fuchsia.bluetooth.sys.Pairing`
   interface, calling into HostDispatcher for help.
- [HostDispatcher](src/host_dispatcher.rs):
  - Implements all stateful logic for the `fuchsia.bluetooth.sys.Access` interface.
  - Provides a Future to monitor `/dev/class/bt-host`, and react to the arrival
    and departure of Bluetooth hosts appropriately.
- [GenericAccessService](src/generic_access_service.rs):
  - Implements the GATT Generic Access Service, which tells peers the name and
    appearance of the device.
- [main](src/main.rs):
  - Binds the Access, Central, Pairing, Peripheral, and Server FIDL APIs to code within
    this component (`bt-gap`).
    - The Access API is bound to AccessService.
    - The Pairing API is bound to the PairingService.
    - Other APIs are proxied directly to their private API counterparts.
  - Instantiates the `/dev/class/bt-host`-monitoring future from HostDispatcher.
  - Configures an Executor to process API events and `/dev/class/bt-host` VFS events.

## Tests
### Unit Tests
Run Rust unit tests with:
```
$ fx test bt-gap-unittests
```
### Integration Tests
Integration tests that test bt-gap and bt-host are located at [bluetooth/tests](../../tests/)
