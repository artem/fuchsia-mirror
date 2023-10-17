# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import fuchsia_controller_py as fc

from ._ipc import GlobalHandleWaker
from ._ipc import HandleWaker


class AsyncSocket:
    """Represents an async socket.

    In 99% of cases it is recommended to use this instead of the standard fuchsia-controller Socket
    object. This has built-in support for handling waits.

    In the remaining 1% of cases it may be useful for the user to do a one-off attempt at reading a
    socket directly and immediately exiting in the event that ZX_ERR_SHOULD_WAIT is encountered.
    Such a case would likely involve adding some custom behavior to existing async code, like
    registering custom wakers. Another case would be where the user is _only_ writing to the socket,
    as writes in AsyncSocket are also synchronous and wrap Socket.write directly.

    Args:
        socket: The socket which will be asynchronously readable.
        waker: (Optional) the HandleWaker implementation (defaults to GlobalHandleWaker).
    """

    def __init__(self, socket: fc.Socket, waker: HandleWaker = None):
        self.socket = socket
        if waker is None:
            self.waker = GlobalHandleWaker()

    def __del__(self):
        if self.waker is not None:
            self.waker.unregister(self.socket)

    async def read(self) -> bytearray:
        """Attempts to read off of the socket.

        Returns:
            A byte array to the caller.

        Raises:
            ZxStatus exception outlining the specific failure of the underlying handle.
        """
        self.waker.register(self.socket)
        while True:
            try:
                result = self.socket.read()
                self.waker.unregister(self.socket)
                return result
            except fc.ZxStatus as e:
                if e.args[0] != fc.ZxStatus.ZX_ERR_SHOULD_WAIT:
                    self.waker.unregister(self.socket)
                    raise e
            await self.waker.wait_channel_ready(self.socket)

    def write(self, buf: bytes) -> None:
        """Does a blocking write on the socket.

        This is identical to calling the write function on the socket itself.

        Args:
            buf: The array of bytes (read-only) to write to the socket.

        Raises:
            ZxStatus exception on failure of the underlying handle.
        """
        self.socket.write(buf)
