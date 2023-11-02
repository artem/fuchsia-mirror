#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth Gap Test"""
import logging
import time
from typing import List, Tuple

from bluetooth_utils_lib import bluetooth_utils
from fuchsia_base_test import fuchsia_base_test
from honeydew.typing.bluetooth import (
    BluetoothAcceptPairing,
    BluetoothConnectionType,
)
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class MultipleFuchsiaDevicesNotFound(Exception):
    """When there are less than two Fuchsia devices available."""


class BluetoothGapTest(fuchsia_base_test.FuchsiaBaseTest):
    def pre_run(self) -> None:
        """Mobly method used to generate the test cases at run time."""
        test_arg_tuple_list: List[Tuple[int]] = []

        for iteration in range(1, int(self.user_params["num_iterations"]) + 1):
            test_arg_tuple_list.append((iteration,))

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self._name_func,
            arg_sets=test_arg_tuple_list,
        )

    def setup_class(self) -> None:
        """Initialize all DUT(s)"""
        super().setup_class()
        if len(self.fuchsia_devices) < 2:
            raise MultipleFuchsiaDevicesNotFound(
                "Two FuchsiaDevices are" "required to run BluetoothGapTest"
            )
        self.initiator = self.fuchsia_devices[0]
        self.receiver = self.fuchsia_devices[1]

    def _test_logic(self, iteration: int) -> None:
        """Test Logic for Bluetooth Sample Test
        1. Turn on BT discoverability on both devices
        2. Retrieve the receiver's BT address
        3. Enable Pairing mode for both Initiator and Receiver
        3. Receive all advertising BT devices on initiator side.
        4. Check that the receiver is advertising to initiator.
        5. Initiate pairing from initiator to receiver.
        6. Verify that pairing was successful.
        7. Initiate connection from initiator to receiver.
        8. Verify that connection was successful.
        """

        _LOGGER.info("Starting the Bluetooth Gap test iteration# %s", iteration)
        _LOGGER.info("Initializing Bluetooth and setting discoverability")
        self._set_discoverability_on()
        # TODO(b/309011914): Remove sleep once polling for discoverability is added.
        time.sleep(3)

        receiver_address = (
            self.receiver.bluetooth_gap.get_active_adapter_address()
        )
        _LOGGER.info("Receiver address: %s", receiver_address)
        self.initiator.bluetooth_gap.accept_pairing(
            input_mode=BluetoothAcceptPairing.DEFAULT_INPUT_MODE,
            output_mode=BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE,
        )
        self.receiver.bluetooth_gap.accept_pairing(
            input_mode=BluetoothAcceptPairing.DEFAULT_INPUT_MODE,
            output_mode=BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE,
        )
        _LOGGER.info(
            "Sleep for 5 seconds to wait for dut to listen for receiever"
        )
        time.sleep(5)

        known_device = self.initiator.bluetooth_gap.get_known_remote_devices()
        receiver_address_converted = bluetooth_utils.sl4f_bt_mac_address(
            mac_address=receiver_address
        )
        identifier = bluetooth_utils.retrieve_device_id(
            data=known_device, reverse_hex_address=receiver_address_converted
        )
        _LOGGER.info("Identifier: %s", identifier)
        _LOGGER.info("Attempting to initiate pairing")
        self.initiator.bluetooth_gap.pair_device(
            identifier=identifier,
            connection_type=BluetoothConnectionType.CLASSIC,
        )
        time.sleep(5)

        self.initiator.bluetooth_gap.connect_device(
            identifier=identifier,
            connection_type=BluetoothConnectionType.CLASSIC,
        )
        asserts.assert_true(
            bluetooth_utils.verify_bt_pairing(
                identifier=identifier, device=self.initiator
            ),
            msg="Receiver was not paired.",
        )
        time.sleep(5)

        _LOGGER.info("Attempting to start connection")
        self.initiator.bluetooth_gap.connect_device(
            identifier=identifier,
            connection_type=BluetoothConnectionType.CLASSIC,
        )
        asserts.assert_true(
            bluetooth_utils.verify_bt_connection(
                identifier=identifier, device=self.initiator
            ),
            msg="Receiver was not connected.",
        )
        _LOGGER.info(
            "Pairing and Connection complete. "
            "Successfully ended the Bluetooth GAP test iteration# %s",
            iteration,
        )

    def teardown_test(self) -> None:
        """Teardown Test logic
        1. Forget all paired devices from initiator.
        2. Forget all paired devices from receiver.
        3. Turn off discoverability on initiator.
        4. Turn off discoverability on receiver.
        """

        _LOGGER.info("Removing all paired devices and " "turning off Bluetooth")
        bluetooth_utils.forget_all_bt_devices(self.initiator)
        bluetooth_utils.forget_all_bt_devices(self.receiver)
        self.initiator.bluetooth_gap.set_discoverable(False)
        self.receiver.bluetooth_gap.set_discoverable(False)
        return super().teardown_class()

    def _name_func(self, iteration: int) -> str:
        """This function generates the names of each test case based on each
        argument set.

        The name function should have the same signature as the actual test
        logic function.

        Returns:
            Test case name
        """
        return f"test_bluetooth_gap_test_{iteration}"

    def _set_discoverability_on(self) -> None:
        """Turns on discoverability for the devices."""
        self.initiator.bluetooth_gap.request_discovery(True)
        self.initiator.bluetooth_gap.set_discoverable(True)
        self.receiver.bluetooth_gap.request_discovery(True)
        self.receiver.bluetooth_gap.set_discoverable(True)


if __name__ == "__main__":
    test_runner.main()
