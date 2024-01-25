#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Bluetooth AVRCP Profile affordance."""

import abc
from typing import Any

from honeydew.interfaces.affordances.bluetooth import bluetooth_common
from honeydew.typing import bluetooth

BluetoothAvrcpCommand = bluetooth.BluetoothAvrcpCommand


class BluetoothAvrcp(bluetooth_common.BluetoothCommon):
    """Abstract base class for BluetoothAvrcp Profile affordance."""

    # List all the public methods
    @abc.abstractmethod
    def init_avrcp(self, target_id: str) -> None:
        """Initialize AVRCP service from the sink device."""

    @abc.abstractmethod
    def list_received_requests(self) -> list[Any]:
        """List received requests received from source device."""

    @abc.abstractmethod
    def publish_mock_player(self) -> None:
        """Publish the media session mock player."""

    @abc.abstractmethod
    def send_avrcp_command(self, command: BluetoothAvrcpCommand) -> None:
        """Send Avrcp command from the sink device."""

    @abc.abstractmethod
    def stop_mock_player(self) -> None:
        """Stop the media session mock player."""
