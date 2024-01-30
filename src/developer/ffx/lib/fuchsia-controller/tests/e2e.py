# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import os
import os.path
import platform
import subprocess
import sys
import typing
import unittest

import fidl.fuchsia_developer_ffx as ffx_fidl
from fidl_codec import encode_fidl_message
from fidl_codec import method_ordinal
from fuchsia_controller_py import Channel
from fuchsia_controller_py import Context
from fuchsia_controller_py import IsolateDir


SDK_ROOT = "./sdk/exported/core"
# For Linux this handles the gamut of options.
# This map is derived from //build/rbe/fuchsia.py
PLATFORM_TYPE = {
    "x86_64": "x64",
    "arm64": "arm64",
}


def _locate_ffx_binary(sdk_manifest: str) -> str:
    """Locates the ffx binary from a given SDK root."""
    return os.path.join(
        SDK_ROOT,
        "tools",
        PLATFORM_TYPE[platform.machine()],
        "ffx",
    )


FFX_PATH = _locate_ffx_binary(SDK_ROOT)


class EndToEnd(unittest.IsolatedAsyncioTestCase):
    def _get_default_config(self) -> typing.Dict[str, str]:
        return {"sdk.root": SDK_ROOT}

    def _get_isolate_dir(self) -> IsolateDir:
        isolation_path = None
        tmp_path = os.getenv("TEST_UNDECLARED_OUTPUTS_DIR")
        if tmp_path:
            isolation_path = os.path.join(tmp_path, "isolate")
        return IsolateDir(dir=isolation_path)

    def _make_ctx(self):
        return Context(
            config=self._get_default_config(),
            isolate_dir=self._get_isolate_dir(),
        )

    def test_target_add_invalid_args(self):
        ctx = self._make_ctx()
        with self.assertRaises(ValueError):
            ctx.target_add("this is definitely not an IP", False)

    def test_target_add_nowait(self):
        ctx = self._make_ctx()
        ctx.target_add("127.0.0.1", False)

    def test_config_get_nonexistent(self):
        ctx = self._make_ctx()
        self.assertEqual(ctx.config_get_string("foobarzzzzzzo==?"), None)

    def test_config_get_exists(self):
        config = self._get_default_config()
        key = "foobar"
        expect = "bazmumble"
        config[key] = expect
        ctx = Context(config=config, isolate_dir=self._get_isolate_dir())
        self.assertEqual(ctx.config_get_string(key), expect)

    def test_config_get_too_long(self):
        config = self._get_default_config()
        key = "foobarzington"
        expect = "b" * 50000
        config[key] = expect
        ctx = Context(config=config, isolate_dir=self._get_isolate_dir())
        with self.assertRaises(BufferError):
            ctx.config_get_string(key)

    async def test_echo_daemon(self):
        ctx = self._make_ctx()
        echo_proxy = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        expected = "this is an echo test"
        result = await echo_proxy.echo_string(value=expected)
        self.assertEqual(result.response, expected)

    async def test_echo_daemon_parallel(self):
        ctx = self._make_ctx()
        echo_proxy = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        expected1 = "this is an echo test1"
        expected2 = "22222this is an echo test2"
        expected3 = "frobination incoming. Heed the call of the frobe"
        loop = asyncio.get_running_loop()
        result1 = loop.create_task(echo_proxy.echo_string(value=expected1))
        result2 = loop.create_task(echo_proxy.echo_string(value=expected2))
        result3 = loop.create_task(echo_proxy.echo_string(value=expected3))
        results_list = await asyncio.gather(result1, result2, result3)
        self.assertEqual(results_list[0].response, expected1)
        self.assertEqual(results_list[1].response, expected2)
        self.assertEqual(results_list[2].response, expected3)
        self.assertEqual(len(echo_proxy.pending_txids), 0)
        self.assertEqual(len(echo_proxy.staged_messages), 0)

    def test_sending_and_later_awaiting_protocol_method_call(self):
        ctx = self._make_ctx()
        (ch0, ch1) = Channel.create()
        echo_proxy1 = ffx_fidl.Echo.Client(ch0)
        echo_proxy2 = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        echo_proxy3 = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        coro = echo_proxy1.echo_string(value="foo")
        buf, _ = ch1.read()
        txid = int.from_bytes(buf[0:4], sys.byteorder)
        encoded_bytes, _ = encode_fidl_message(
            object=ffx_fidl.EchoEchoStringRequest(value="foo"),
            library="fuchsia.developer.ffx",
            type_name="fuchsia.developer.ffx/EchoEchoStringRequest",
            txid=txid,
            ordinal=method_ordinal(
                protocol="fuchsia.developer.ffx/Echo", method="EchoString"
            ),
        )
        ordinal = method_ordinal(
            protocol="fuchsia.developer.ffx/Echo", method="EchoString"
        )
        self.assertEqual(buf, encoded_bytes)
        msg = encode_fidl_message(
            object=ffx_fidl.EchoEchoStringResponse(response="otherthing"),
            library="fuchsia.developer.ffx",
            type_name="fuchsia.developer.ffx/EchoEchoStringResponse",
            txid=txid,
            ordinal=method_ordinal(
                protocol="fuchsia.developer.ffx/Echo", method="EchoString"
            ),
        )
        ch1.write(msg)
        res = asyncio.run(echo_proxy2.echo_string(value="bar"))
        self.assertEqual(res.response, "bar")
        res = asyncio.run(echo_proxy2.echo_string(value="baz"))
        self.assertEqual(res.response, "baz")
        result = asyncio.run(coro)
        self.assertEqual(result.response, "otherthing")

    async def test_client_sends_message_before_coro_await(self):
        (ch0, ch1) = Channel.create()
        echo_proxy = ffx_fidl.Echo.Client(ch0)
        coro = echo_proxy.echo_string(value="foo")
        buf, _ = ch1.read()
        txid = int.from_bytes(buf[0:4], sys.byteorder)
        encoded_bytes, _ = encode_fidl_message(
            object=ffx_fidl.EchoEchoStringRequest(value="foo"),
            library="fuchsia.developer.ffx",
            type_name="fuchsia.developer.ffx/EchoEchoStringRequest",
            txid=txid,
            ordinal=method_ordinal(
                protocol="fuchsia.developer.ffx/Echo", method="EchoString"
            ),
        )
        self.assertEqual(buf, encoded_bytes)
        msg = encode_fidl_message(
            object=ffx_fidl.EchoEchoStringResponse(response="otherthing"),
            library="fuchsia.developer.ffx",
            type_name="fuchsia.developer.ffx/EchoEchoStringResponse",
            txid=txid,
            ordinal=method_ordinal(
                protocol="fuchsia.developer.ffx/Echo", method="EchoString"
            ),
        )
        ch1.write(msg)
        result = await coro
        self.assertEqual(result.response, "otherthing")

    def test_context_creation_no_config_but_target(self):
        """This test simply ensures passing a target does not cause an error."""
        _ctx = Context(target="foo")

    def test_context_creation_duplicate_target_raises_exception(self):
        with self.assertRaises(RuntimeError):
            _ctx = Context(target="foo", config={"target.default": "bar"})

    def test_context_creation_no_args(self):
        Context()

    def test_setting_fidl_clients(self):
        """Previously a classmethod was setting the handle.

        This ensures these aren't being set globally."""
        ctx = self._make_ctx()
        e1 = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        e2 = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        self.assertNotEqual(e1.channel.as_int(), e2.channel.as_int())

    def test_running_multiple_commands(self):
        """Test for original revert.

        CL in question was:
        https://fuchsia-review.git.corp.google.com/q/commit:4b7548f1411968d5ed2c852291422011cff2efba
        """
        ctx = self._make_ctx()
        e = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        res = asyncio.run(e.echo_string(value="foo"))
        self.assertEqual(res.response, "foo")
        res = asyncio.run(e.echo_string(value="barzzz"))
        self.assertEqual(res.response, "barzzz")

    async def test_connect_daemon_without_autostart(self):
        isolate_dir = self._get_isolate_dir()
        ffx_cmd = [
            FFX_PATH,
            "--isolate-dir",
            isolate_dir.directory(),
            "daemon",
            "echo",
        ]
        subprocess.check_call(ffx_cmd)
        ctx = Context(
            config={
                "daemon.autostart": "false",
            },
            isolate_dir=isolate_dir,
        )
        e = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER)
        )
        res = await e.echo_string(value="foo")
        self.assertEqual(res.response, "foo")
        res = await e.echo_string(value="barrrrzzz")
        self.assertEqual(res.response, "barrrrzzz")

    async def test_sending_fidl_protocol(self):
        tc_server, tc_client = Channel.create()
        list_server, list_client = Channel.create()
        tc_proxy = ffx_fidl.TargetCollection.Client(tc_client)
        query = ffx_fidl.TargetQuery(string_matcher="foobar")
        tc_proxy.list_targets(query=query, reader=list_client.take())
        buf, hdls = tc_server.read()
        self.assertEqual(len(hdls), 1)
        new_list_client = Channel(hdls[0])
        new_list_client.write((bytearray([5, 6, 7]), []))
        list_buf, list_hdls = list_server.read()
        self.assertEqual(len(list_hdls), 0)
        self.assertEqual(list_buf, bytearray([5, 6, 7]))
