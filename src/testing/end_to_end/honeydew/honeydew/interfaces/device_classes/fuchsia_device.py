#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Fuchsia device."""

import abc
from typing import Callable, Dict, Optional

from honeydew import custom_types
from honeydew.interfaces.affordances import session
from honeydew.interfaces.affordances import tracing
from honeydew.interfaces.affordances.bluetooth import bluetooth_gap
from honeydew.interfaces.auxiliary_devices import \
    power_switch as power_switch_interface
from honeydew.utils import properties

TIMEOUTS: Dict[str, float] = {
    "OFFLINE": 60,
    "ONLINE": 120,
}


class FuchsiaDevice(abc.ABC):
    """Abstract base class for Fuchsia device.

    This class contains abstract methods that are supported by every device
    running Fuchsia irrespective of the device type.
    """

    # List all the persistent properties in alphabetical order
    @properties.PersistentProperty
    @abc.abstractmethod
    def device_name(self) -> str:
        """Returns the name of the device.

        Returns:
            Name of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def device_type(self) -> str:
        """Returns the type of the device.

        Returns:
            Type of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of the device.
        """

    # List all the dynamic properties in alphabetical order
    @properties.DynamicProperty
    @abc.abstractmethod
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of the device.
        """

    # List all the affordances in alphabetical order
    @properties.Affordance
    @abc.abstractmethod
    def bluetooth_gap(self) -> bluetooth_gap.BluetoothGap:
        """Returns a BluetoothGap affordance object.

        Returns:
            bluetooth_gap.BluetoothGap object
        """

    @properties.Affordance
    @abc.abstractmethod
    def session(self) -> session.Session:
        """Returns a session affordance object.

        Returns:
            session.Session object
        """

    @properties.Affordance
    @abc.abstractmethod
    def tracing(self) -> tracing.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """

    # List all the public methods in alphabetical order
    @abc.abstractmethod
    def close(self) -> None:
        """Clean up method."""

    @abc.abstractmethod
    def health_check(self) -> None:
        """Ensure device is healthy."""

    @abc.abstractmethod
    def log_message_to_device(
            self, message: str, level: custom_types.LEVEL) -> None:
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.
        """

    @abc.abstractmethod
    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted."""

    @abc.abstractmethod
    def power_cycle(
            self,
            power_switch: power_switch_interface.PowerSwitch,
            outlet: Optional[int] = None) -> None:
        """Power cycle (power off, wait for delay, power on) the device.

        Args:
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.
        """

    @abc.abstractmethod
    def reboot(self) -> None:
        """Soft reboot the device."""

    @abc.abstractmethod
    def register_for_on_device_boot(self, fn: Callable[[], None]) -> None:
        """Register a function that will be called in on_device_boot."""

    @abc.abstractmethod
    def snapshot(
            self, directory: str, snapshot_file: Optional[str] = None) -> str:
        """Captures the snapshot of the device.

        Args:
            directory: Absolute path on the host where snapshot file need
                to be saved.

            snapshot_file: Name of the file to be used to save snapshot file.
                If not provided, API will create a name using
                "Snapshot_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            Absolute path of the snapshot file.
        """

    @abc.abstractmethod
    def wait_for_offline(self, timeout: float = TIMEOUTS["OFFLINE"]) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.
        """

    @abc.abstractmethod
    def wait_for_online(self, timeout: float = TIMEOUTS["ONLINE"]) -> None:
        """Wait for Fuchsia device to go online.

        Args:
            timeout: How long in sec to wait for device to go offline.
        """
