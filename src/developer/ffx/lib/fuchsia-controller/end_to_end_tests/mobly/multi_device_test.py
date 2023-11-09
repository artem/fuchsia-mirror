# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import typing
import logging

import fidl.fuchsia_bluetooth as bluetooth
import fidl.fuchsia_bluetooth_sys as bluetooth_sys
from fuchsia_controller_py import Channel, ZxStatus
from mobly import asserts
from mobly import base_test
from mobly import test_runner
from mobly_controller import fuchsia_device
from mobly_controller.fuchsia_device import asynctest

PEER_MATCH_TIMEOUT_SECONDS: int = 120


class MultipleFuchsiaDevicesNotFound(Exception):
    """Raised when there are less than two Fuchsia devices available."""


class BluetoothDevice(object):
    """A wrapper around a FuchsiaDevice that adds support for bluetooth."""

    def __init__(self, device: fuchsia_device.FuchsiaDevice):
        self.device = device
        self.discoverable_token: Channel | None = None
        self.discovery_token: Channel | None = None
        self.peer_update_task: asyncio.Task[None] = None
        self.peer_update_queue: asyncio.Queue[bluetooth_sys.Peer] = None

    def connect_proxies(self):
        self.access_proxy = bluetooth_sys.Access.Client(
            self.device.ctx.connect_device_proxy(
                "core/bluetooth-core", bluetooth_sys.Access.MARKER
            )
        )
        self.host_watcher_proxy = bluetooth_sys.HostWatcher.Client(
            self.device.ctx.connect_device_proxy(
                "core/bluetooth-core", bluetooth_sys.HostWatcher.MARKER
            )
        )

    async def start_listeners(self):
        queue = asyncio.Queue()

        async def impl():
            while True:
                try:
                    results = await self.access_proxy.watch_peers()
                except ZxStatus as e:
                    if e.args[0] == ZxStatus.ZX_ERR_PEER_CLOSED:
                        break
                    raise e
                for peer in results.updated:
                    await queue.put(peer)

        self.peer_update_task = asyncio.get_running_loop().create_task(impl())
        self.peer_update_queue = queue

    def stop_listeners(self):
        self.peer_update_task.cancel()
        self.peer_update_task = None
        self.peer_update_queue = None

    async def get_next_peer_update(self):
        res = await self.peer_update_queue.get()
        self.peer_update_queue.task_done()
        return res

    async def set_discoverable(self, enabled: bool):
        if enabled:
            client, server = Channel.create()
            await self.access_proxy.make_discoverable(token=server.take())
            self.discoverable_token = client
        else:
            self.discoverable_token = None

    async def start_discovery(self):
        client, server = Channel.create()
        await self.access_proxy.start_discovery(token=server.take())
        self.discovery_token = client

    def stop_discovery(self):
        self.discover_token = None

    async def get_adapter_address(self) -> bluetooth.Address:
        while True:
            hosts_response = await self.host_watcher_proxy.watch()
            hosts = hosts_response.hosts
            if hosts:
                for host in hosts:
                    if host.addresses:
                        res = host.addresses[0]
                        return res
                raise RuntimeError(
                    "No addresses found in response: {hosts_response}"
                )
                break

    def cancel_peer_update_task(self):
        self.peer_update_task = None


class MultiDeviceTest(base_test.BaseTestClass):
    def _setup_device(
        self, device: fuchsia_device.FuchsiaDevice
    ) -> BluetoothDevice:
        device.set_ctx(self)
        res = BluetoothDevice(device)
        res.connect_proxies()
        return res

    def setup_class(self) -> None:
        self.fuchsia_devices: typing.List[fuchsia_device.FuchsiaDevice] = [
            self._setup_device(x)
            for x in self.register_controller(fuchsia_device)
        ]
        if len(self.fuchsia_devices) < 2:
            raise MutipleFuchsiaDevicesNotFound(
                "Two fuchsia devices are required to run this test."
            )
        self.initiator = self.fuchsia_devices[0]
        self.receiver = self.fuchsia_devices[1]

    async def _wait_for_matching_peer(
        self, receiver_address: bluetooth.Address
    ):
        while True:
            peer = await self.initiator.get_next_peer_update()
            logging.debug(f"Received peer update: {peer}")
            if (
                peer.address is not None
                and peer.address.bytes == receiver_address.bytes
                and peer.address.type == receiver_address.type
            ):
                break

    @asynctest
    async def test_discovery(self) -> None:
        for device in self.fuchsia_devices:
            await device.start_listeners()
            await device.start_discovery()
            await device.set_discoverable(True)
        receiver_address = await self.receiver.get_adapter_address()
        logging.info(f"Got receiver address: {receiver_address}")
        async with asyncio.timeout(PEER_MATCH_TIMEOUT_SECONDS):
            await self._wait_for_matching_peer(receiver_address)

    @asynctest
    async def teardown_test(self) -> None:
        for device in self.fuchsia_devices:
            device.stop_listeners()
            await device.set_discoverable(False)
            device.stop_discovery()
            device.cancel_peer_update_task()


if __name__ == "__main__":
    test_runner.main()
