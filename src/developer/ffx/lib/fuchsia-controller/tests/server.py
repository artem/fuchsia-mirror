# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import unittest
import asyncio
import fidl.fuchsia_developer_ffx as ffx
import os
import sys
import tempfile
import os.path
import asyncio
from fidl_codec import encode_fidl_message, method_ordinal
from fuchsia_controller_py import Context, IsolateDir, Channel, ZxStatus


class TestEchoer(ffx.Echo.Server):

    def echo_string(self, request: ffx.EchoEchoStringRequest):
        return ffx.EchoEchoStringResponse(response=request.value)


class AsyncEchoer(ffx.Echo.Server):

    async def echo_string(self, request: ffx.EchoEchoStringRequest):
        await asyncio.sleep(0.1)  # This isn't necessary, but it is fun.
        return ffx.EchoEchoStringResponse(response=request.value)


class TargetCollectionReaderImpl(ffx.TargetCollectionReader.Server):

    def __init__(self, channel: Channel, target_list):
        super().__init__(channel)
        self.target_list = target_list

    def next(self, request: ffx.TargetCollectionReaderNextRequest):
        if not request.entry:
            raise super().StopService()
        self.target_list.extend(request.entry)


class TargetCollectionImpl(ffx.TargetCollection.Server):

    async def list_targets(
            self, request: ffx.TargetCollectionListTargetsRequest):
        reader = ffx.TargetCollectionReader.Client(request.reader)
        await reader.next(
            entry=[
                ffx.TargetInfo(nodename="foo"),
                ffx.TargetInfo(nodename="bar"),
            ])
        await reader.next(entry=[
            ffx.TargetInfo(nodename="baz"),
        ])
        await reader.next(entry=[])


class ServerTests(unittest.IsolatedAsyncioTestCase):

    async def test_echo_server_sync(self):
        (tx, rx) = Channel.create()
        server = TestEchoer(rx)
        client = ffx.Echo.Client(tx)
        task = asyncio.get_running_loop().create_task(server.serve())
        res = await client.echo_string(value="foobar")
        self.assertEqual(res.response, "foobar")

    async def test_echo_server_async(self):
        (tx, rx) = Channel.create()
        server = AsyncEchoer(rx)
        client = ffx.Echo.Client(tx)
        task = asyncio.get_running_loop().create_task(server.serve())
        res = await client.echo_string(value="foobar")
        self.assertEqual(res.response, "foobar")

    async def test_not_implemented(self):
        (tx, rx) = Channel.create()
        server = ffx.Echo.Server(rx)
        client = ffx.Echo.Client(tx)
        task = asyncio.get_running_loop().create_task(server.serve())
        # The first thing that will happen is the server will receive the
        # request, then attempt to call the corresponding function. Since it is not implemented,
        # An exception will be raised, closing the channel. The client will then receive a
        # PEER_CLOSED error. In order to diagnose the root cause the task must then be awaited.
        with self.assertRaises(ZxStatus):
            try:
                res = await client.echo_string(value="foobar")
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e
        self.assertTrue(task.done())
        with self.assertRaises(NotImplementedError):
            await task

    async def test_target_iterator(self):
        (reader_client_channel, reader_server_channel) = Channel.create()
        target_list = []
        server = TargetCollectionReaderImpl(reader_server_channel, target_list)
        (tc_client_channel, tc_server_channel) = Channel.create()
        target_collection_server = TargetCollectionImpl(tc_server_channel)
        loop = asyncio.get_running_loop()
        reader_task = loop.create_task(server.serve())
        tc_task = loop.create_task(target_collection_server.serve())
        tc_client = ffx.TargetCollection.Client(tc_client_channel)
        tc_client.list_targets(
            query=ffx.TargetQuery(), reader=reader_client_channel.take())
        done, pending = await asyncio.wait(
            [reader_task, tc_task], return_when=asyncio.FIRST_COMPLETED)
        # This will just surface exceptions if they happen. For correct behavior this should just
        # return the result of the reader task.
        done.pop().result()
        self.assertEqual(len(target_list), 3)
        foo_targets = [x for x in target_list if x.nodename == "foo"]
        self.assertEqual(len(foo_targets), 1)
        bar_targets = [x for x in target_list if x.nodename == "bar"]
        self.assertEqual(len(bar_targets), 1)
        baz_targets = [x for x in target_list if x.nodename == "baz"]
        self.assertEqual(len(baz_targets), 1)
