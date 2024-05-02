# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Runs a stress test running echo 100x in parallel.

This test runs remote-control's echo method 100x in parallel against a Fuchsia
device.
"""

import asyncio
import typing

import fidl.fuchsia_developer_remotecontrol as remotecontrol
from mobly import asserts
from mobly import base_test
from mobly import test_runner
from mobly_controller import fuchsia_device
from mobly_controller.fuchsia_device import asynctest


class FuchsiaControllerTests(base_test.BaseTestClass):
    def setup_class(self) -> None:
        self.fuchsia_devices: typing.List[
            fuchsia_device.FuchsiaDevice
        ] = self.register_controller(fuchsia_device)
        self.device = self.fuchsia_devices[0]
        self.device.set_ctx(self)

    @asynctest
    async def test_remote_control_echo_100x_parallel(self) -> None:
        """Runs remote control proxy's "echo" method 100x in parallel."""
        if self.device.ctx is None:
            raise ValueError(f"Device: {self.device.target} has no context")
        ch = self.device.ctx.connect_remote_control_proxy()
        rcs = remotecontrol.RemoteControl.Client(ch)

        def echo(
            proxy: remotecontrol.RemoteControl.Client, s: str
        ) -> typing.Awaitable[typing.Any]:
            return proxy.echo_string(value=s)

        coros = [echo(rcs, f"foobar_{x}") for x in range(0, 100)]
        results = await asyncio.gather(*coros)
        for i, r in enumerate(results):
            asserts.assert_equal(r.response, f"foobar_{i}")


if __name__ == "__main__":
    test_runner.main()
