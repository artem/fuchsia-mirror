#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""SL4F based implementation for Bluetooth AVRCP Profile affordance."""

from enum import StrEnum

from honeydew.affordances.sl4f.bluetooth import bluetooth_common
from honeydew.interfaces.affordances.bluetooth.profiles import bluetooth_avrcp
from honeydew.typing import bluetooth


class Sl4fMethods(StrEnum):
    INIT_AVRCP = "avrcp_facade.AvrcpInit"
    LIST_RECEIVED_REQUESTS = "media_session_facade.ListReceivedRequests"
    PUBLISH_MOCK_PLAYER = "media_session_facade.PublishMockPlayer"
    SEND_AVRCP_COMMAND = "avrcp_facade.AvrcpSendCommand"
    STOP_MOCK_PLAYER = "media_session_facade.StopMockPlayer"


BluetoothAvrcpCommand = bluetooth.BluetoothAvrcpCommand


class BluetoothAvrcp(
    bluetooth_common.BluetoothCommon, bluetooth_avrcp.BluetoothAvrcp
):
    """SL4F based implementation for BluetoothAvrcp Profile affordance."""

    # List all the public methods
    def init_avrcp(self, target_id: str) -> None:
        """Initialize AVRCP service from the sink device.

        Args:
            target_id: id of source device to start AVRCP
        Raises:
            errors.Sl4fError: On failure.
        """
        self._sl4f.run(
            method=Sl4fMethods.INIT_AVRCP, params={"target_id": target_id}
        )

    def list_received_requests(self) -> list[object]:
        """List received requests received from source device.

        Returns:
            A list of the most recent commands received, where the last
            element in the list is the most recent command received. If no
            result then return empty list.
        Raises:
            errors.Sl4fError: On failure.
        """
        requests = self._sl4f.run(method=Sl4fMethods.LIST_RECEIVED_REQUESTS)
        return requests.get("result", [])

    def publish_mock_player(self) -> None:
        """Publish the media session mock player.

        Raises:
            errors.Sl4fError: On failure.
        """
        self._sl4f.run(method=Sl4fMethods.PUBLISH_MOCK_PLAYER)

    def send_avrcp_command(self, command: BluetoothAvrcpCommand) -> None:
        """Send Avrcp command from the sink device.

        Args:
            command: the command to send to the AVRCP service.

        Raises:
            errors.Sl4fError: On Failure.
        """
        self._sl4f.run(
            method=Sl4fMethods.SEND_AVRCP_COMMAND, params={"command": command}
        )

    def stop_mock_player(self) -> None:
        """Stop the media session mock player.

        Raises:
            errors.Sl4fError: On Failure.
        """
        self._sl4f.run(method=Sl4fMethods.STOP_MOCK_PLAYER)
