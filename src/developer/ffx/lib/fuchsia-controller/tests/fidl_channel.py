# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

from fuchsia_controller_py import Channel
from fuchsia_controller_py import Event
from fuchsia_controller_py import Handle
from fuchsia_controller_py import Socket
from fuchsia_controller_py import ZxStatus


class ChannelTests(unittest.TestCase):
    """Channel tests."""

    def test_event_access_denied(self):
        e = Event()
        with self.assertRaises(ZxStatus):
            try:
                # Attempt USER_0 signal.
                e.signal_peer(0, 1 << 24)
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_ACCESS_DENIED)
                raise e

    def test_eventpair_peer_closed(self):
        e1, e2 = Event.create()
        del e2
        with self.assertRaises(ZxStatus):
            try:
                # Attempt USER_0 signal.
                e1.signal_peer(0, 1 << 24)
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e

    def test_as_int(self):
        self.assertEqual(Handle(1).as_int(), 1)
        self.assertEqual(Channel(2).as_int(), 2)
        self.assertEqual(Socket(3).as_int(), 3)

    def test_channel_write_then_read(self):
        (a, b) = Channel.create()
        a.write((bytearray([1, 2, 3]), []))
        buf, hdls = b.read()
        self.assertEqual(buf, bytearray([1, 2, 3]))

    def test_channel_write_fails_when_closed(self):
        (a, b) = Channel.create()
        del b
        with self.assertRaises(ZxStatus):
            try:
                a.write((bytearray([1, 2, 3]), []))
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e

    def test_channel_passing(self):
        (a, b) = Channel.create()
        (c, d) = Channel.create()
        # This is using 'take' rather than 'as_int' as using 'as_int' would cause a double-close
        # error on a channel that has already been closed.
        a.write((bytearray(), [(0, c.take(), 0, 0, 0)]))
        _, hdls = b.read()
        self.assertEqual(len(hdls), 1)
        new_c = Channel(hdls[0])
        new_c.write((bytearray([1, 2, 3]), []))
        buf, d_hdls = d.read()
        self.assertEqual(buf, bytearray([1, 2, 3]))
