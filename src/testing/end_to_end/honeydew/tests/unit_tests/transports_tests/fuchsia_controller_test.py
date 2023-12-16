#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.fuchsia_controller.py."""

import ipaddress
import unittest
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller

from honeydew import custom_types, errors
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import (
    fuchsia_controller as fuchsia_controller_transport,
)

_IPV4: str = "11.22.33.44"
_IPV4_OBJ: ipaddress.IPv4Address = ipaddress.IPv4Address(_IPV4)

_DEVICE_NAME: str = "fuchsia-emulator"

_INPUT_ARGS: dict[str, Any] = {
    "device_name": _DEVICE_NAME,
    "device_ip_v4": _IPV4_OBJ,
    "BuildInfo": custom_types.FidlEndpoint(
        "/core/build-info", "fuchsia.buildinfo.Provider"
    ),
}

_MOCK_ARGS: dict[str, Any] = {
    "ffx_config": custom_types.FFXConfig(
        isolate_dir=fuchsia_controller.IsolateDir("/tmp/isolate"),
        logs_dir="/tmp/logs",
    ),
}


# pylint: disable=protected-access
class FuchsiaControllerTests(unittest.TestCase):
    """Unit tests for honeydew.transports.fuchsia_controller.py."""

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller.Context,
        "target_wait",
        autospec=True,
    )
    @mock.patch.object(
        ffx_transport,
        "get_config",
        return_value=custom_types.FFXConfig(
            isolate_dir=_MOCK_ARGS["ffx_config"].isolate_dir,
            logs_dir=_MOCK_ARGS["ffx_config"].logs_dir,
        ),
        autospec=True,
    )
    def setUp(self, mock_get_config, mock_target_wait) -> None:
        super().setUp()

        self.fuchsia_controller_obj_wo_device_ip = (
            fuchsia_controller_transport.FuchsiaController(
                device_name=_INPUT_ARGS["device_name"],
            )
        )
        mock_get_config.assert_called_once()
        mock_target_wait.assert_called_once()

        mock_get_config.reset_mock()
        mock_target_wait.reset_mock()
        self.fuchsia_controller_obj_with_device_ip = (
            fuchsia_controller_transport.FuchsiaController(
                device_name=_INPUT_ARGS["device_name"],
                device_ip=_INPUT_ARGS["device_ip_v4"],
            )
        )
        mock_get_config.assert_called_once()
        mock_target_wait.assert_called_once()

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller.Context,
        "target_wait",
        autospec=True,
    )
    @mock.patch.object(
        ffx_transport,
        "get_config",
        return_value=custom_types.FFXConfig(
            isolate_dir=_MOCK_ARGS["ffx_config"].isolate_dir,
            logs_dir=_MOCK_ARGS["ffx_config"].logs_dir,
        ),
        autospec=True,
    )
    def test_create_context(self, mock_get_config, mock_target_wait) -> None:
        """Test case for fuchsia_controller_transport.create_context()."""
        self.fuchsia_controller_obj_with_device_ip.create_context()

        mock_get_config.assert_called_once()
        mock_target_wait.assert_called_once()

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller,
        "Context",
        side_effect=fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS
        ),
        autospec=True,
    )
    def test_create_context_creation_error(self, mock_fc_context) -> None:
        """Verify create_context() when the fuchsia controller Context creation
        raises an error."""
        with self.assertRaises(errors.FuchsiaControllerError):
            self.fuchsia_controller_obj_with_device_ip.create_context()

        mock_fc_context.assert_called()

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller.Context,
        "connect_device_proxy",
        autospec=True,
    )
    def test_connect_device_proxy(self, mock_fc_connect_device_proxy) -> None:
        """Test case for fuchsia_controller_transport.connect_device_proxy()"""
        self.fuchsia_controller_obj_with_device_ip.connect_device_proxy(
            _INPUT_ARGS["BuildInfo"]
        )

        mock_fc_connect_device_proxy.assert_called()

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller.Context,
        "connect_device_proxy",
        side_effect=fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS
        ),
        autospec=True,
    )
    def test_connect_device_proxy_error(
        self, mock_fc_connect_device_proxy
    ) -> None:
        """Test case for fuchsia_controller_transport.connect_device_proxy()"""
        with self.assertRaises(errors.FuchsiaControllerError):
            self.fuchsia_controller_obj_with_device_ip.connect_device_proxy(
                _INPUT_ARGS["BuildInfo"]
            )

        mock_fc_connect_device_proxy.assert_called()

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller.Context,
        "target_wait",
        autospec=True,
    )
    def test_check_connection(self, mock_target_wait) -> None:
        """Testcase for FuchsiaController.check_connection()"""
        self.fuchsia_controller_obj_with_device_ip.check_connection()

        mock_target_wait.assert_called()

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller.Context,
        "target_wait",
        side_effect=RuntimeError("error"),
        autospec=True,
    )
    def test_check_connection_raises(self, mock_target_wait) -> None:
        """Testcase for FuchsiaController.check_connection() raises
        errors.FuchsiaControllerConnectionError"""
        with self.assertRaises(errors.FuchsiaControllerConnectionError):
            self.fuchsia_controller_obj_with_device_ip.check_connection()

        mock_target_wait.assert_called()
