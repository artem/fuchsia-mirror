# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Screenshot affordance implementation using ffx."""

import logging
import os
import tempfile

from honeydew.interfaces.affordances.ui import screenshot
from honeydew.transports import ffx as ffx_transport
from honeydew.typing import screenshot_image

_LOGGER: logging.Logger = logging.getLogger(__name__)

_FFX_SCREENSHOT_CMD: list[str] = [
    "target",
    "screenshot",
    "--format",
    "png",
    "-d",
]


class Screenshot(screenshot.Screenshot):
    """Screenshot affordance implementation using FFX.

    Args:
        ffx: FFX transport.
    """

    def __init__(self, ffx: ffx_transport.FFX) -> None:
        self._ffx: ffx_transport.FFX = ffx

    def take(self) -> screenshot_image.ScreenshotImage:
        """Take a screenshot.

        Return:
            ScreenshotImage: the screenshot image.
        """

        with tempfile.TemporaryDirectory() as temp_dir:
            # ffx screenshot always outputs a file named screenshot.png
            path = os.path.join(temp_dir, "screenshot.png")
            self._ffx.run(cmd=_FFX_SCREENSHOT_CMD + [temp_dir])
            image = screenshot_image.ScreenshotImage.load_from_path(path)
            _LOGGER.debug("Screenshot taken")
            return image
