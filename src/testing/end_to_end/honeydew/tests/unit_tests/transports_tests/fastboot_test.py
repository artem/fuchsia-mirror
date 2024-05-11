#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.fastboot.py."""

import ipaddress
import os
import subprocess
import sys
import unittest
from collections.abc import Callable
from importlib import resources
from typing import Any
from unittest import mock

from parameterized import param, parameterized

from honeydew import errors
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fastboot, ffx
from honeydew.utils import common

_IPV4: str = "11.22.33.44"
_IPV4_OBJ: ipaddress.IPv4Address = ipaddress.IPv4Address(_IPV4)

_USB_BASED_DEVICE_NAME: str = "fuchsia-d88c-799b-0e3a"
_USB_BASED_FASTBOOT_NODE_ID: str = "0B190YCABZZ2ML"

_TCP_BASED_DEVICE_NAME: str = "fuchsia-54b2-038b-6e90"
_TCP_IP_ADDRESS: str = "fe80::56b2:3ff:fe8b:6e90%enxa0cec8f442ce"
_TCP_BASED_FASTBOOT_NODE_ID: str = f"tcp:{_TCP_IP_ADDRESS}"

_USB_BASED_TARGET_WHEN_IN_FUCHSIA_MODE: dict[str, Any] = {
    "nodename": _USB_BASED_DEVICE_NAME,
    "rcs_state": "Y",
    "serial": _USB_BASED_FASTBOOT_NODE_ID,
    "target_type": "someproduct_latest_eng.someproduct",
    "target_state": "Product",
    "addresses": [
        "fe80::de1d:c975:e647:cf39%zx-d88c799b0e3b",
        "172.16.243.231",
    ],
    "is_default": True,
}

_USB_BASED_TARGET_WHEN_IN_FASTBOOT_MODE: dict[str, Any] = {
    "nodename": _USB_BASED_DEVICE_NAME,
    "rcs_state": "N",
    "serial": _USB_BASED_FASTBOOT_NODE_ID,
    "target_type": "someproduct_latest_eng.someproduct",
    "target_state": "Fastboot",
    "addresses": [],
    "is_default": True,
}

_TCP_BASED_TARGET_WHEN_IN_FUCHSIA_MODE: dict[str, Any] = {
    "nodename": _TCP_BASED_DEVICE_NAME,
    "rcs_state": "Y",
    "serial": "<unknown>",
    "target_type": "core.x64",
    "target_state": "Product",
    "addresses": ["fe80::881b:4248:1002:a7ce%enxa0cec8f442ce"],
    "is_default": True,
}

_TCP_BASED_TARGET_WHEN_IN_FASTBOOT_MODE: dict[str, Any] = {
    "nodename": _TCP_BASED_DEVICE_NAME,
    "rcs_state": "N",
    "serial": "<unknown>",
    "target_type": "core.x64",
    "target_state": "Fastboot",
    "addresses": [_TCP_IP_ADDRESS],
    "is_default": True,
}

_TCP_BASED_TARGET_WHEN_IN_FASTBOOT_MODE_WITH_TWO_IPS: dict[str, Any] = {
    "nodename": _TCP_BASED_DEVICE_NAME,
    "rcs_state": "N",
    "serial": "<unknown>",
    "target_type": "core.x64",
    "target_state": "Fastboot",
    "addresses": ["fe80::881b:4248:1002:a7ce%enxa0cec8f442ce", _TCP_IP_ADDRESS],
    "is_default": True,
}

_FFX_TARGET_LIST_WHEN_IN_FUCHSIA_MODE: list[dict[str, Any]] = [
    _USB_BASED_TARGET_WHEN_IN_FUCHSIA_MODE,
    _TCP_BASED_TARGET_WHEN_IN_FUCHSIA_MODE,
]

_INPUT_ARGS: dict[str, Any] = {
    "device_name": _USB_BASED_DEVICE_NAME,
    "fastboot_node_id": _USB_BASED_FASTBOOT_NODE_ID,
    "device_ip_v4": _IPV4_OBJ,
    "run_cmd": ["getvar", "hw-revision"],
    "subprocess_run_cmd": [
        "fastboot",
        "-s",
        _USB_BASED_FASTBOOT_NODE_ID,
        "getvar",
        "hw-revision",
    ],
}

_MOCK_ARGS: dict[str, Any] = {
    "ffx_target_info_when_in_fuchsia_mode": _USB_BASED_TARGET_WHEN_IN_FUCHSIA_MODE,
    "ffx_target_info_when_in_fastboot_mode": _USB_BASED_TARGET_WHEN_IN_FASTBOOT_MODE,
    "fastboot_getvar_hw_revision": b"hw-revision: core.x64-b4\nFinished. Total time: 0.000s\n",
}

_EXPECTED_VALUES: dict[str, Any] = {
    "fastboot_run_getvar_hw_revision": ["hw-revision: core.x64-b4"],
}


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_{test_label}"


# pylint: disable=protected-access
class FastbootTests(unittest.TestCase):
    """Unit tests for honeydew.transports.fastboot.py."""

    def setUp(self) -> None:
        super().setUp()

        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice
        )

        self.ffx_obj = mock.MagicMock(spec=ffx.FFX)

        self.fastboot_obj = fastboot.Fastboot(
            device_name=_INPUT_ARGS["device_name"],
            device_ip=_INPUT_ARGS["device_ip_v4"],
            reboot_affordance=self.reboot_affordance_obj,
            fastboot_node_id=_INPUT_ARGS["fastboot_node_id"],
            ffx_transport=self.ffx_obj,
        )

    @mock.patch(
        "importlib.resources.files",
        autospec=True,
    )
    def test_get_fastboot_binary_with_env_var_success(
        self, mock_files: mock.Mock
    ) -> None:
        """Test case for _get_fastboot_binary() when environment
        variable override is provided"""
        with mock.patch.dict(
            os.environ, {"HONEYDEW_FASTBOOT_OVERRIDE": "fastboot"}, clear=True
        ):
            bin_name = fastboot._get_fastboot_binary()
            self.assertEqual(bin_name, "fastboot")
            mock_files.assert_not_called()

    @mock.patch.object(resources, "files", autospec=True)
    @mock.patch.object(resources, "as_file", autospec=True)
    @mock.patch("atexit.register", autospec=True)
    @mock.patch("shutil.copy2", autospec=True)
    @mock.patch("tempfile.NamedTemporaryFile", autospec=True)
    def test_get_fastboot_binary_with_resource_success(
        self,
        mock_tmp_file: mock.Mock,
        mock_copy: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Test case for _get_fastboot_binary() when fastboot data exists"""
        sys.modules["honeydew.data"] = mock.Mock()
        with mock.patch.dict(os.environ, {}, clear=True):
            mock_fd = mock.Mock()
            mock_fd.name = "tmpfastboot"
            mock_tmp_file.return_value = mock_fd
            bin_name = fastboot._get_fastboot_binary()
            self.assertEqual(bin_name, "tmpfastboot")
            mock_copy.assert_called_with(mock.ANY, bin_name)

    @mock.patch.object(resources, "as_file", autospec=True)
    def test_get_fastboot_binary_with_resource_fail(
        self, mock_as_file: mock.Mock
    ) -> None:
        """Test case for _get_fastboot_binary() when fastboot data does not
        exist"""
        mock_file = mock.Mock()
        mock_file.stat.side_effect = ImportError
        mock_as_file.return_value.__enter__.return_value = mock_file
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(errors.HoneydewDataResourceError):
                fastboot._get_fastboot_binary()

    def test_node_id_when_fastboot_node_id_passed(self) -> None:
        """Testcase for Fastboot.node_id when `fastboot_node_id` arg was passed
        during initialization"""
        self.assertEqual(
            self.fastboot_obj.node_id, _INPUT_ARGS["fastboot_node_id"]
        )

    @mock.patch.object(
        fastboot.Fastboot,
        "wait_for_fuchsia_mode",
        side_effect=errors.FfxCommandError("error"),
        autospec=True,
    )
    def test_boot_to_fastboot_mode_when_not_in_fuchsia_mode(
        self, mock_wait_for_fuchsia_mode: mock.Mock
    ) -> None:
        """Test case for Fastboot.boot_to_fastboot_mode() when device is not in
        fuchsia mode"""
        with self.assertRaises(errors.FuchsiaStateError):
            self.fastboot_obj.boot_to_fastboot_mode()

        mock_wait_for_fuchsia_mode.assert_called()

    @mock.patch.object(
        fastboot.Fastboot, "wait_for_fastboot_mode", autospec=True
    )
    @mock.patch.object(
        fastboot.Fastboot,
        "wait_for_fuchsia_mode",
        autospec=True,
    )
    def test_boot_to_fastboot_mode_when_in_fuchsia_mode(
        self,
        mock_wait_for_fuchsia_mode: mock.Mock,
        mock_wait_for_fastboot_mode: mock.Mock,
    ) -> None:
        """Test case for Fastboot.boot_to_fastboot_mode() when device is not in
        fuchsia mode"""
        self.fastboot_obj.boot_to_fastboot_mode()

        mock_wait_for_fuchsia_mode.assert_called()
        mock_wait_for_fastboot_mode.assert_called()

    @mock.patch.object(
        fastboot.Fastboot,
        "wait_for_fuchsia_mode",
        autospec=True,
    )
    def test_boot_to_fastboot_mode_failed(
        self, mock_wait_for_fuchsia_mode: mock.Mock
    ) -> None:
        """Test case for Fastboot.boot_to_fastboot_mode() raising an
        exception"""
        self.ffx_obj.run.side_effect = errors.FfxCommandError("error")

        with self.assertRaises(errors.FastbootCommandError):
            self.fastboot_obj.boot_to_fastboot_mode()

        mock_wait_for_fuchsia_mode.assert_called()

    @mock.patch.object(
        fastboot.Fastboot,
        "is_in_fastboot_mode",
        return_value=False,
        autospec=True,
    )
    def test_boot_to_fuchsia_mode_when_not_in_fastboot_mode(
        self, mock_is_in_fastboot_mode: mock.Mock
    ) -> None:
        """Test case for Fastboot.boot_to_fuchsia_mode() when device is not in
        fastboot mode"""
        with self.assertRaises(errors.FuchsiaStateError):
            self.fastboot_obj.boot_to_fuchsia_mode()

        mock_is_in_fastboot_mode.assert_called()

    @mock.patch.object(
        fastboot.Fastboot, "wait_for_fuchsia_mode", autospec=True
    )
    @mock.patch.object(fastboot.Fastboot, "run", autospec=True)
    @mock.patch.object(
        fastboot.Fastboot,
        "is_in_fastboot_mode",
        return_value=True,
        autospec=True,
    )
    def test_boot_to_fuchsia_mode_when_in_fastboot_mode(
        self,
        mock_is_in_fastboot_mode: mock.Mock,
        mock_fastboot_run: mock.Mock,
        mock_wait_for_fuchsia_mode: mock.Mock,
    ) -> None:
        """Test case for Fastboot.boot_to_fuchsia_mode() when device is in
        fastboot mode"""
        self.fastboot_obj.boot_to_fuchsia_mode()

        mock_is_in_fastboot_mode.assert_called()
        mock_fastboot_run.assert_called()
        mock_wait_for_fuchsia_mode.assert_called()

    @mock.patch.object(
        fastboot.Fastboot,
        "run",
        side_effect=errors.FastbootCommandError("error"),
        autospec=True,
    )
    @mock.patch.object(
        fastboot.Fastboot,
        "is_in_fastboot_mode",
        return_value=True,
        autospec=True,
    )
    def test_boot_to_fuchsia_mode_failed(
        self, mock_is_in_fastboot_mode: mock.Mock, mock_fastboot_run: mock.Mock
    ) -> None:
        """Test case for Fastboot.boot_to_fuchsia_mode() raising an exception"""
        with self.assertRaises(errors.FastbootCommandError):
            self.fastboot_obj.boot_to_fuchsia_mode()

        mock_is_in_fastboot_mode.assert_called()
        mock_fastboot_run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "when_device_is_in_fuchsia_mode",
                    "ffx_target_info": _MOCK_ARGS[
                        "ffx_target_info_when_in_fuchsia_mode"
                    ],
                    "expected": False,
                },
            ),
            (
                {
                    "label": "when_device_is_in_fastboot_mode_mode",
                    "ffx_target_info": _MOCK_ARGS[
                        "ffx_target_info_when_in_fastboot_mode"
                    ],
                    "expected": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(fastboot.Fastboot, "_get_target_info", autospec=True)
    def test_is_in_fastboot_mode(
        self,
        parameterized_dict: dict[str, Any],
        mock_get_target_info: mock.Mock,
    ) -> None:
        """Test case for Fastboot.is_in_fastboot_mode()"""
        mock_get_target_info.side_effect = [
            parameterized_dict["ffx_target_info"]
        ]
        self.assertEqual(
            self.fastboot_obj.is_in_fastboot_mode(),
            parameterized_dict["expected"],
        )
        mock_get_target_info.assert_called()

    @mock.patch.object(
        fastboot.Fastboot,
        "_get_target_info",
        side_effect=errors.FfxCommandError("error"),
        autospec=True,
    )
    def test_is_in_fastboot_mode_exception(
        self, mock_get_target_info: mock.Mock
    ) -> None:
        """Test case for Fastboot.is_in_fastboot_mode() raising
        FastbootCommandError."""
        with self.assertRaises(errors.FastbootCommandError):
            self.fastboot_obj.is_in_fastboot_mode()
        mock_get_target_info.assert_called()

    @mock.patch.object(
        fastboot.Fastboot,
        "is_in_fastboot_mode",
        return_value=False,
        autospec=True,
    )
    def test_run_when_not_in_fastboot_mode(
        self, mock_is_in_fastboot_mode: mock.Mock
    ) -> None:
        """Test case for Fastboot.run() when device is not in fastboot mode."""
        with self.assertRaises(errors.FuchsiaStateError):
            self.fastboot_obj.run(cmd=_INPUT_ARGS["run_cmd"])
        mock_is_in_fastboot_mode.assert_called()

    @mock.patch.object(
        subprocess,
        "check_output",
        return_value=_MOCK_ARGS["fastboot_getvar_hw_revision"],
        autospec=True,
    )
    @mock.patch.object(
        fastboot.Fastboot,
        "is_in_fastboot_mode",
        return_value=True,
        autospec=True,
    )
    def test_run_when_in_fastboot_mode_success(
        self,
        mock_is_in_fastboot_mode: mock.Mock,
        mock_subprocess_check_output: mock.Mock,
    ) -> None:
        """Test case for Fastboot.run() when device is in fastboot mode and
        returns success."""
        self.assertEqual(
            self.fastboot_obj.run(cmd=_INPUT_ARGS["run_cmd"]),
            _EXPECTED_VALUES["fastboot_run_getvar_hw_revision"],
        )
        mock_is_in_fastboot_mode.assert_called()
        mock_subprocess_check_output.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "TimeoutExpired",
                    "check_output": subprocess.TimeoutExpired(
                        timeout=10, cmd=_INPUT_ARGS["subprocess_run_cmd"]
                    ),
                    "expected_exception": subprocess.TimeoutExpired,
                },
            ),
            (
                {
                    "label": "FastbootCommandError_because_of_FileNotFoundError",
                    "check_output": FileNotFoundError(
                        "No such file or directory: 'fastbot'"
                    ),
                    "expected_exception": errors.FastbootCommandError,
                },
            ),
            (
                {
                    "label": "FastbootCommandError_because_of_CalledProcessError",
                    "check_output": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="fastboot devices",
                        output="command output and error",
                    ),
                    "expected_exception": errors.FastbootCommandError,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(subprocess, "check_output", autospec=True)
    @mock.patch.object(
        fastboot.Fastboot,
        "is_in_fastboot_mode",
        return_value=True,
        autospec=True,
    )
    def test_run_when_in_fastboot_mode_exceptions(
        self,
        parameterized_dict: dict[str, Any],
        mock_is_in_fastboot_mode: mock.Mock,
        mock_subprocess_check_output: mock.Mock,
    ) -> None:
        """Test case for Fastboot.run() when device is in fastboot mode and
        returns in exceptions."""
        mock_subprocess_check_output.side_effect = parameterized_dict[
            "check_output"
        ]

        with self.assertRaises(parameterized_dict["expected_exception"]):
            self.fastboot_obj.run(cmd=_INPUT_ARGS["run_cmd"])

        mock_is_in_fastboot_mode.assert_called()
        mock_subprocess_check_output.assert_called()

    @mock.patch.object(
        subprocess,
        "check_output",
        side_effect=RuntimeError("error"),
        autospec=True,
    )
    @mock.patch.object(
        fastboot.Fastboot,
        "is_in_fastboot_mode",
        return_value=True,
        autospec=True,
    )
    def test_run_when_in_fastboot_mode_with_exceptions_to_skip(
        self,
        mock_is_in_fastboot_mode: mock.Mock,
        mock_subprocess_check_output: mock.Mock,
    ) -> None:
        """Test case for Fastboot.run() when device is in fastboot mode and
        called with exceptions_to_skip."""
        self.assertEqual(
            self.fastboot_obj.run(
                cmd=_INPUT_ARGS["run_cmd"], exceptions_to_skip=[RuntimeError]
            ),
            [],
        )

        mock_is_in_fastboot_mode.assert_called()
        mock_subprocess_check_output.assert_called()

    def test_get_fastboot_node_with_fastboot_node_id_arg(self) -> None:
        """Test case for Fastboot._get_fastboot_node() when called with
        fastboot_node_id arg."""
        self.fastboot_obj._get_fastboot_node(
            fastboot_node_id=_USB_BASED_FASTBOOT_NODE_ID
        )
        self.assertEqual(
            self.fastboot_obj._fastboot_node_id, _USB_BASED_FASTBOOT_NODE_ID
        )

    @mock.patch.object(
        fastboot.Fastboot,
        "_get_target_info",
        return_value=_USB_BASED_TARGET_WHEN_IN_FUCHSIA_MODE,
        autospec=True,
    )
    def test_get_fastboot_node_without_fastboot_node_id_arg_usb_based(
        self, mock_fastboot_get_target_info: mock.Mock
    ) -> None:
        """Test case for Fastboot._get_fastboot_node() when called without
        fastboot_node_id arg for a USB based fastboot device."""
        self.fastboot_obj._get_fastboot_node()
        self.assertEqual(
            self.fastboot_obj._fastboot_node_id, _USB_BASED_FASTBOOT_NODE_ID
        )
        mock_fastboot_get_target_info.assert_called()

    @mock.patch.object(fastboot.Fastboot, "boot_to_fuchsia_mode", autospec=True)
    @mock.patch.object(
        fastboot.Fastboot, "_wait_for_valid_tcp_address", autospec=True
    )
    @mock.patch.object(
        fastboot.Fastboot, "boot_to_fastboot_mode", autospec=True
    )
    @mock.patch.object(
        fastboot.Fastboot,
        "_get_target_info",
        side_effect=[
            _TCP_BASED_TARGET_WHEN_IN_FUCHSIA_MODE,
            _TCP_BASED_TARGET_WHEN_IN_FASTBOOT_MODE,
        ],
        autospec=True,
    )
    def test_get_fastboot_node_without_fastboot_node_id_arg_tcp_based(
        self,
        mock_fastboot_get_target_info: mock.Mock,
        mock_boot_to_fastboot_mode: mock.Mock,
        mock_wait_for_valid_tcp_address: mock.Mock,
        mock_boot_to_fuchsia_mode: mock.Mock,
    ) -> None:
        """Test case for Fastboot._get_fastboot_node() when called without
        fastboot_node_id arg for a TCP based fastboot device."""
        self.fastboot_obj._get_fastboot_node()
        self.assertEqual(
            self.fastboot_obj._fastboot_node_id, _TCP_BASED_FASTBOOT_NODE_ID
        )

        self.assertEqual(mock_fastboot_get_target_info.call_count, 2)
        mock_boot_to_fastboot_mode.assert_called()
        mock_wait_for_valid_tcp_address.assert_called()
        mock_boot_to_fuchsia_mode.assert_called()

    @mock.patch.object(
        fastboot.Fastboot,
        "_get_target_info",
        side_effect=errors.FfxCommandError("error"),
        autospec=True,
    )
    def test_get_fastboot_node_without_fastboot_node_id_arg_exception(
        self, mock_fastboot_get_target_info: mock.Mock
    ) -> None:
        """Test case for Fastboot._get_fastboot_node() when called without
        fastboot_node_id arg results in an exception."""
        with self.assertRaises(errors.FuchsiaDeviceError):
            self.fastboot_obj._get_fastboot_node()
        mock_fastboot_get_target_info.assert_called()

    def test_get_target_info_when_connected(self) -> None:
        """Test case for Fastboot._get_target_info() when device is
        connected."""
        self.ffx_obj.get_target_list.return_value = (
            _FFX_TARGET_LIST_WHEN_IN_FUCHSIA_MODE
        )
        self.assertEqual(
            self.fastboot_obj._get_target_info(),
            _USB_BASED_TARGET_WHEN_IN_FUCHSIA_MODE,
        )

    def test_get_target_info_when_not_connected(self) -> None:
        """Test case for Fastboot._get_target_info() when device is not
        connected."""
        self.ffx_obj.get_target_list.return_value = []
        with self.assertRaises(errors.FfxCommandError):
            self.fastboot_obj._get_target_info()

    @parameterized.expand(
        [
            (
                {
                    "label": "single_ip_address",
                    "get_target_info": _TCP_BASED_TARGET_WHEN_IN_FASTBOOT_MODE,
                    "expected": True,
                },
            ),
            (
                {
                    "label": "multiple_ip_address",
                    "get_target_info": _TCP_BASED_TARGET_WHEN_IN_FASTBOOT_MODE_WITH_TWO_IPS,
                    "expected": False,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        fastboot.Fastboot,
        "_get_target_info",
        return_value=_USB_BASED_TARGET_WHEN_IN_FUCHSIA_MODE,
        autospec=True,
    )
    def test_is_a_single_ip_address(
        self,
        parameterized_dict: dict[str, Any],
        mock_fastboot_get_target_info: mock.Mock,
    ) -> None:
        """Test case for Fastboot._is_a_single_ip_address()"""
        mock_fastboot_get_target_info.return_value = parameterized_dict[
            "get_target_info"
        ]
        self.assertEqual(
            self.fastboot_obj._is_a_single_ip_address(),
            parameterized_dict["expected"],
        )
        mock_fastboot_get_target_info.assert_called()

    @mock.patch.object(common, "wait_for_state", autospec=True)
    def test_wait_for_fastboot_mode_success(
        self, mock_wait_for_state: mock.Mock
    ) -> None:
        """Test case for Fastboot.wait_for_fastboot_mode() success case."""
        self.fastboot_obj.wait_for_fastboot_mode()
        mock_wait_for_state.assert_called()

    @mock.patch.object(
        common,
        "wait_for_state",
        side_effect=errors.HoneydewTimeoutError("error"),
        autospec=True,
    )
    def test_wait_for_fastboot_mode_exception(
        self, mock_wait_for_state: mock.Mock
    ) -> None:
        """Test case for Fastboot.wait_for_fastboot_mode() failure case."""
        with self.assertRaises(errors.FuchsiaDeviceError):
            self.fastboot_obj.wait_for_fastboot_mode()
        mock_wait_for_state.assert_called()

    def test_wait_for_fuchsia_mode_success(self) -> None:
        """Test case for Fastboot.wait_for_fuchsia_mode() success case."""
        self.fastboot_obj.wait_for_fuchsia_mode()

    def test_wait_for_fuchsia_mode_exception(self) -> None:
        """Test case for Fastboot.wait_for_fuchsia_mode() failure case."""
        self.ffx_obj.wait_for_rcs_connection.side_effect = (
            errors.HoneydewTimeoutError("error")
        )
        with self.assertRaises(errors.FuchsiaDeviceError):
            self.fastboot_obj.wait_for_fuchsia_mode()

    @mock.patch.object(common, "wait_for_state", autospec=True)
    def test_wait_for_valid_tcp_address_success(
        self, mock_wait_for_state: mock.Mock
    ) -> None:
        """Test case for Fastboot._wait_for_valid_tcp_address() success case."""
        self.fastboot_obj._wait_for_valid_tcp_address()
        mock_wait_for_state.assert_called()

    @mock.patch.object(
        common,
        "wait_for_state",
        side_effect=errors.HoneydewTimeoutError("error"),
        autospec=True,
    )
    def test_wait_for_valid_tcp_address_exception(
        self, mock_wait_for_state: mock.Mock
    ) -> None:
        """Test case for Fastboot._wait_for_valid_tcp_address() failure case."""
        with self.assertRaises(errors.FuchsiaDeviceError):
            self.fastboot_obj._wait_for_valid_tcp_address()
        mock_wait_for_state.assert_called()
