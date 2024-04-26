# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Real time clock (RTC) affordance using the FuchsiaController."""

import asyncio
import datetime

import fidl.fuchsia_hardware_rtc as frtc
import fuchsia_controller_py

from honeydew import errors
from honeydew.interfaces.affordances import rtc
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import (
    fuchsia_controller as fuchsia_controller_lib,
)
from honeydew.typing import custom_types

CAPABILITY = "fuchsia.hardware.rtc.Service/default/device"


class Rtc(rtc.Rtc):
    """Affordance for the fuchsia.hardware.rtc.Device protocol."""

    # TODO(b/316959472) Use toolbox once RTC service lands in the toolbox realm.
    #
    # Service moniker for the NXP PCF8563 chip on a Khadas vim3 board.
    # Currently, this is board-specific. Once the RTC protocol lands and is
    # routable from the toolbox realm, this affordance can be made
    # board-agnostic.
    MONIKER = "/bootstrap/pkg-drivers:dev.sys.platform.05_00_2.i2c-0.aml-i2c.i2c.i2c-0-81"

    def __init__(
        self,
        fuchsia_controller: fuchsia_controller_lib.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        """Initializer."""
        self._controller = fuchsia_controller

        # This needs to be called once upon __init__(), and any time the device
        # is rebooted. On reboot, the connection is lost and needs to be
        # re-established.
        self._connect_proxy()
        reboot_affordance.register_for_on_device_boot(self._connect_proxy)

    def _connect_proxy(self) -> None:
        """Connect the RTC Device protocol proxy."""
        ep = custom_types.FidlEndpoint(self.__class__.MONIKER, CAPABILITY)
        self._proxy: frtc.Device.Client = frtc.Device.Client(
            self._controller.connect_device_proxy(ep)
        )

    # Protocol methods.
    def get(self) -> datetime.datetime:
        """See base class."""
        try:
            time = asyncio.run(self._proxy.get())
        except fuchsia_controller_py.ZxStatus as status:
            msg = f"Device.Get() error {status}"
            raise errors.HoneydewRtcError(msg) from status

        response = time.response.rtc
        return datetime.datetime(
            response.year,
            response.month,
            response.day,
            response.hours,
            response.minutes,
            response.seconds,
        )

    def set(self, time: datetime.datetime) -> None:
        """See base class."""
        ftime = frtc.Time(
            time.second, time.minute, time.hour, time.day, time.month, time.year
        )

        try:
            result = asyncio.run(self._proxy.set(rtc=ftime))
        except fuchsia_controller_py.ZxStatus as status:
            msg = f"Device.Set() error {status}"
            raise errors.HoneydewRtcError(msg) from status

        if result.response.status != fuchsia_controller_py.ZxStatus.ZX_OK:
            msg = f"Device.Set() error {result.response.status}"
            raise errors.HoneydewRtcError(msg)
