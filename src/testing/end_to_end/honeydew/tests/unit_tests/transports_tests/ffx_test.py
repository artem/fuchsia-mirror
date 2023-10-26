#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.ffx.py."""

import subprocess
from typing import Any
import unittest
from unittest import mock

from parameterized import parameterized

from honeydew import custom_types
from honeydew import errors
from honeydew.transports import ffx

# pylint: disable=protected-access
_SSH_ADDRESS = "fe80::3804:df7d:daa8:ce6c"
_SSH_ADDRESS_SCOPE = "qemu"
_SSH_PORT = 8022
_TARGET_SSH_ADDRESS = custom_types.TargetSshAddress(
    ip=f"{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}", port=_SSH_PORT
)

_FFX_TARGET_SHOW_OUTPUT: bytes = (
    r'[{"title":"Target","label":"target","description":"",'
    r'"child":[{"title":"Name","label":"name","description":"Target name.",'
    r'"value":"fuchsia-emulator"},{"title":"SSH Address",'
    r'"label":"ssh_address","description":"Interface address",'
    r'"value":'
    f'"{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}:{_SSH_PORT}"'
    r'}]},{"title":"Build",'
    r'"label":"build","description":"","child":[{"title":"Version",'
    r'"label":"version","description":"Build version.",'
    r'"value":"2023-02-01T17:26:40+00:00"},{"title":"Product",'
    r'"label":"product","description":"Product config.",'
    r'"value":"workstation_eng"},{"title":"Board","label":"board",'
    r'"description":"Board config.","value":"qemu-x64"},{"title":"Commit",'
    r'"label":"commit","description":"Integration Commit Date",'
    r'"value":"2023-02-01T17:26:40+00:00"}]}]'
).encode()

_FFX_TARGET_SHOW_JSON: list[dict[str, Any]] = [
    {
        "title": "Target",
        "label": "target",
        "description": "",
        "child": [
            {
                "title": "Name",
                "label": "name",
                "description": "Target name.",
                "value": "fuchsia-emulator",
            },
            {
                "title": "SSH Address",
                "label": "ssh_address",
                "description": "Interface address",
                "value": f"{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}:{_SSH_PORT}",
            },
        ],
    },
    {
        "title": "Build",
        "label": "build",
        "description": "",
        "child": [
            {
                "title": "Version",
                "label": "version",
                "description": "Build version.",
                "value": "2023-02-01T17:26:40+00:00",
            },
            {
                "title": "Product",
                "label": "product",
                "description": "Product config.",
                "value": "workstation_eng",
            },
            {
                "title": "Board",
                "label": "board",
                "description": "Board config.",
                "value": "qemu-x64",
            },
            {
                "title": "Commit",
                "label": "commit",
                "description": "Integration Commit Date",
                "value": "2023-02-01T17:26:40+00:00",
            },
        ],
    },
]

_FFX_TARGET_LIST_OUTPUT: str = (
    '[{"nodename":"fuchsia-emulator","rcs_state":"Y","serial":"<unknown>",'
    '"target_type":"workstation_eng.qemu-x64","target_state":"Product",'
    '"addresses":["fe80::6a47:a931:1e84:5077%qemu"],"is_default":true}]\n'
)

_FFX_TARGET_LIST_JSON: list[dict[str, Any]] = [
    {
        "nodename": "fuchsia-emulator",
        "rcs_state": "Y",
        "serial": "<unknown>",
        "target_type": "workstation_eng.qemu-x64",
        "target_state": "Product",
        "addresses": ["fe80::6a47:a931:1e84:5077%qemu"],
        "is_default": True,
    }
]

_INPUT_ARGS: dict[str, Any] = {
    "target": "fuchsia-emulator",
    "run_cmd": ffx._FFX_CMDS["TARGET_SHOW"],
}

_MOCK_ARGS: dict[str, Any] = {
    "ffx_target_show_output": _FFX_TARGET_SHOW_OUTPUT,
    "ffx_target_show_json": _FFX_TARGET_SHOW_JSON,
    "ffx_target_ssh_address_output": f"[{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}]:{_SSH_PORT}",
    "ffx_target_list_output": _FFX_TARGET_LIST_OUTPUT,
    "ffx_target_list_json": _FFX_TARGET_LIST_JSON,
}

_EXPECTED_VALUES: dict[str, Any] = {
    "ffx_target_show_output": _FFX_TARGET_SHOW_OUTPUT.decode(),
    "ffx_target_show_json": _FFX_TARGET_SHOW_JSON,
    "ffx_target_list_json": _FFX_TARGET_LIST_JSON,
}


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FfxCliTests(unittest.TestCase):
    """Unit tests for honeydew.transports.ffx.py."""

    def setUp(self) -> None:
        super().setUp()

        self.ffx_obj = ffx.FFX(target=_INPUT_ARGS["target"])

    def test_ffx_setup(self) -> None:
        """Test case for ffx.setup()."""
        ffx.setup(logs_dir="/tmp/ffx_logs/")

        # calling setup again should fail
        with self.assertRaises(errors.FfxCommandError):
            ffx.setup(logs_dir="/tmp/ffx_logs_2/")

    def test_ffx_get_config(self) -> None:
        """Test case for ffx.get_config()."""
        # Ensure ffx.get_config() will return valid FFXConfig when called after
        # ffx.setup()
        ffx.setup(logs_dir="/tmp/ffx_logs/")
        ffx_config = ffx.get_config()
        self.assertEqual(ffx_config.logs_dir, "/tmp/ffx_logs/")

        # Ensure ffx.get_config() will return empty FFXConfig when called after
        # ffx.close()
        ffx.close()
        ffx_config = ffx.get_config()
        self.assertEqual(ffx_config, custom_types.FFXConfig())

    def test_ffx_close(self) -> None:
        """Test case for ffx.close()."""
        ffx.close()

    @mock.patch.object(ffx.FFX, "wait_for_rcs_connection", autospec=True)
    def test_check_connection(self, mock_wait_for_rcs_connection) -> None:
        """Test case for check_connection()"""
        self.ffx_obj.check_connection()

        mock_wait_for_rcs_connection.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "run",
        return_value=_MOCK_ARGS["ffx_target_show_output"],
        autospec=True,
    )
    def test_get_target_information_when_connected(self, mock_ffx_run) -> None:
        """Verify get_target_information() succeeds when target is connected to
        host."""
        self.assertEqual(
            self.ffx_obj.get_target_information(),
            _EXPECTED_VALUES["ffx_target_show_json"],
        )

        mock_ffx_run.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "run",
        side_effect=subprocess.TimeoutExpired(
            timeout=10, cmd="ffx -t fuchsia-emulator target show"
        ),
        autospec=True,
    )
    def test_get_target_information_raises_timeout_expired(
        self, mock_ffx_run
    ) -> None:
        """Verify get_target_information raising subprocess.TimeoutExpired."""
        with self.assertRaises(subprocess.TimeoutExpired):
            self.ffx_obj.get_target_information()

        mock_ffx_run.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "run",
        side_effect=errors.FfxCommandError(
            "ffx -t fuchsia-emulator target show failed"
        ),
        autospec=True,
    )
    def test_get_target_information_raises_ffx_command_error(
        self, mock_ffx_run
    ) -> None:
        """Verify get_target_information raising FfxCommandError."""
        with self.assertRaises(errors.FfxCommandError):
            self.ffx_obj.get_target_information()

        mock_ffx_run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "when_no_devices_connected",
                    "return_value": "[]\n",
                    "expected_value": [],
                },
            ),
            (
                {
                    "label": "when_one_device_connected",
                    "return_value": _MOCK_ARGS["ffx_target_list_output"],
                    "expected_value": _EXPECTED_VALUES["ffx_target_list_json"],
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        ffx.FFX,
        "run",
        return_value=_MOCK_ARGS["ffx_target_list_output"],
        autospec=True,
    )
    def test_get_target_list(self, parameterized_dict, mock_ffx_run) -> None:
        """Test case for get_target_list()."""
        mock_ffx_run.return_value = parameterized_dict["return_value"]
        self.assertEqual(
            self.ffx_obj.get_target_list(), parameterized_dict["expected_value"]
        )

        mock_ffx_run.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "run",
        side_effect=errors.FfxCommandError("ffx target list failed"),
        autospec=True,
    )
    def test_get_target_list_exception(self, mock_ffx_run) -> None:
        """Test case for get_target_list() raising exception."""
        with self.assertRaises(errors.FfxCommandError):
            self.ffx_obj.get_target_list()
        mock_ffx_run.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "run",
        return_value=_MOCK_ARGS["ffx_target_ssh_address_output"],
        autospec=True,
    )
    def test_get_target_ssh_address(self, mock_ffx_run) -> None:
        """Verify get_target_ssh_address returns SSH information of the fuchsia
        device."""
        self.assertEqual(
            self.ffx_obj.get_target_ssh_address(), _TARGET_SSH_ADDRESS
        )
        mock_ffx_run.assert_called()

    @parameterized.expand(
        [
            ({"label": "empty_output", "side_effect": b"[]"},),
            (
                {
                    "label": "FfxCommandError",
                    "side_effect": errors.FfxCommandError(
                        "ffx -t fuchsia-emulator target show failed"
                    ),
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(ffx.FFX, "run", autospec=True)
    def test_get_target_ssh_address_exception(
        self, parameterized_dict, mock_ffx_run
    ) -> None:
        """Verify get_target_ssh_address raise exception in failure cases."""
        mock_ffx_run.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.FfxCommandError):
            self.ffx_obj.get_target_ssh_address()

        mock_ffx_run.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "get_target_information",
        return_value=_MOCK_ARGS["ffx_target_show_json"],
        autospec=True,
    )
    def test_get_target_type(self, mock_get_target_information) -> None:
        """Verify ffx.get_target_type returns target type of fuchsia device."""
        result: str = self.ffx_obj.get_target_type()
        expected: str = _FFX_TARGET_SHOW_JSON[1]["child"][2]["value"]

        self.assertEqual(result, expected)

        mock_get_target_information.assert_called()

    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        return_value=_MOCK_ARGS["ffx_target_show_output"],
        autospec=True,
    )
    def test_ffx_run(self, mock_subprocess_check_output) -> None:
        """Test case for ffx.run()"""
        self.assertEqual(
            self.ffx_obj.run(cmd=_INPUT_ARGS["run_cmd"]),
            _EXPECTED_VALUES["ffx_target_show_output"],
        )

        mock_subprocess_check_output.assert_called()

    @mock.patch.object(
        ffx.subprocess,
        "check_call",
        return_value=None,
        autospec=True,
    )
    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        return_value=None,
        autospec=True,
    )
    def test_ffx_run_no_capture_output(
        self, mock_subprocess_check_output, mock_subprocess_check_call
    ) -> None:
        """Test case for ffx.run()"""
        self.assertEqual(
            self.ffx_obj.run(
                cmd=["test", "run", "my-test"], capture_output=False
            ),
            "",
        )
        mock_subprocess_check_output.assert_not_called()
        mock_subprocess_check_call.assert_called_with(
            [
                "ffx",
                "-t",
                "fuchsia-emulator",
                "--config",
                "{}",
                "test",
                "run",
                "my-test",
            ],
            timeout=10,
        )

    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        side_effect=subprocess.TimeoutExpired(
            timeout=10, cmd="ffx -t fuchsia-emulator target show"
        ),
        autospec=True,
    )
    def test_ffx_run_timeout_expired_exception(
        self, mock_subprocess_check_output
    ) -> None:
        """Test case for ffx.run() raises subprocess.TimeoutExpired"""
        with self.assertRaises(subprocess.TimeoutExpired):
            self.ffx_obj.run(cmd=_INPUT_ARGS["run_cmd"])

        mock_subprocess_check_output.assert_called()

    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        side_effect=subprocess.CalledProcessError(
            returncode=1,
            cmd="ffx -t fuchsia-emulator target show",
            output="command output and error",
        ),
        autospec=True,
    )
    def test_ffx_run_ffx_command_error_exception(
        self, mock_subprocess_check_output
    ) -> None:
        """Test case for ffx.run() raises errors.FfxCommandError"""
        with self.assertRaises(errors.FfxCommandError):
            self.ffx_obj.run(cmd=_INPUT_ARGS["run_cmd"])

        mock_subprocess_check_output.assert_called()

    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        side_effect=RuntimeError("error"),
        autospec=True,
    )
    def test_ffx_run_with_exceptions_to_skip(
        self, mock_subprocess_check_output
    ) -> None:
        """Test case for ffx.run() when called with exceptions_to_skip."""
        self.assertEqual(
            self.ffx_obj.run(
                cmd=_INPUT_ARGS["run_cmd"], exceptions_to_skip=[RuntimeError]
            ),
            "",
        )

        mock_subprocess_check_output.assert_called()

    @mock.patch.object(ffx.subprocess, "check_output", autospec=True)
    def test_add_target(self, mock_subprocess_check_output) -> None:
        """Test case for ffx_cli.add_target()."""
        ip_port: custom_types.IpPort = custom_types.IpPort.parse(
            "127.0.0.1:8082"
        )
        ffx.FFX.add_target(target_ip_port=ip_port)

        mock_subprocess_check_output.assert_called_once()

    @parameterized.expand(
        [
            (
                {
                    "label": "CalledProcessError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="ffx target add 127.0.0.1:8082",
                        output="command output and error",
                    ),
                    "expected": errors.FfxCommandError,
                },
            ),
            (
                {
                    "label": "TimeoutExpired",
                    "side_effect": subprocess.TimeoutExpired(
                        timeout=10, cmd="ffx target add 127.0.0.1:8082"
                    ),
                    "expected": subprocess.TimeoutExpired,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(ffx.subprocess, "check_output", autospec=True)
    def test_add_target_exception(
        self, parameterized_dict, mock_subprocess_check_output
    ) -> None:
        """Verify ffx_cli.add_target raise exception in failure cases."""
        ip_port: custom_types.IpPort = custom_types.IpPort.parse(
            "127.0.0.1:8082"
        )
        mock_subprocess_check_output.side_effect = parameterized_dict[
            "side_effect"
        ]

        expected = parameterized_dict["expected"]

        with self.assertRaises(expected):
            ffx.FFX.add_target(target_ip_port=ip_port)

        mock_subprocess_check_output.assert_called_once()

    @mock.patch.object(
        ffx.FFX,
        "get_target_information",
        return_value=_MOCK_ARGS["ffx_target_show_json"],
        autospec=True,
    )
    def test_get_target_name(self, mock_ffx_get_target_information) -> None:
        """Verify get_target_name returns the name of the fuchsia device."""
        ip_port: custom_types.IpPort = custom_types.IpPort.parse(
            f"[{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}]:{_SSH_PORT}"
        )
        self.ffx_obj = ffx.FFX(target=str(ip_port))

        self.assertEqual(self.ffx_obj.get_target_name(), "fuchsia-emulator")

        mock_ffx_get_target_information.assert_called()

    @parameterized.expand(
        [
            ({"label": "empty_output", "side_effect": b"[]"},),
            (
                {
                    "label": "CalledProcessError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd=f"ffx -t '[{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}]:"
                        f"{_SSH_PORT}' target show",
                    ),
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        ffx.FFX,
        "get_target_information",
        return_value=_MOCK_ARGS["ffx_target_show_output"],
        autospec=True,
    )
    def test_get_target_name_exception(
        self, parameterized_dict, mock_ffx_get_target_information
    ) -> None:
        """Verify get_target_ssh_address raise exception in failure cases."""
        mock_ffx_get_target_information.side_effect = parameterized_dict[
            "side_effect"
        ]

        with self.assertRaises(errors.FfxCommandError):
            self.ffx_obj.get_target_name()

        mock_ffx_get_target_information.assert_called_once()

    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        return_value=b"",
        autospec=True,
    )
    def test_wait_for_rcs_connection(
        self, mock_subprocess_check_output
    ) -> None:
        """Test case for ffx.wait_for_rcs_connection()"""
        self.ffx_obj.wait_for_rcs_connection()

        mock_subprocess_check_output.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "DeviceNotConnectedError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="ffx -t fuchsia-emulator target wait",
                        output=ffx._DEVICE_NOT_CONNECTED,
                    ),
                    "expected_error": errors.DeviceNotConnectedError,
                },
            ),
            (
                {
                    "label": "FFXCommandError_because_of_CalledProcessError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="ffx -t fuchsia-emulator target wait",
                        output="command output and error",
                    ),
                    "expected_error": errors.FfxCommandError,
                },
            ),
            (
                {
                    "label": "FFXCommandError_because_of_non_CalledProcessError",
                    "side_effect": RuntimeError(
                        "some error",
                    ),
                    "expected_error": errors.FfxCommandError,
                },
            ),
            (
                {
                    "label": "TimeoutExpired",
                    "side_effect": subprocess.TimeoutExpired(
                        timeout=10, cmd="ffx -t fuchsia-emulator target wait"
                    ),
                    "expected_error": subprocess.TimeoutExpired,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        autospec=True,
    )
    def test_wait_for_rcs_connection_exceptions(
        self, parameterized_dict, mock_subprocess_check_output
    ) -> None:
        """Test case for ffx.wait_for_rcs_connection() raising different
        exceptions."""
        mock_subprocess_check_output.side_effect = parameterized_dict[
            "side_effect"
        ]

        with self.assertRaises(parameterized_dict["expected_error"]):
            self.ffx_obj.wait_for_rcs_connection()

        mock_subprocess_check_output.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "when_rcs_connection_disconnected",
                    "subprocess_side_effect": [
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="cmd",
                            output="error",
                            stderr="stderr",
                        ),
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="cmd",
                            output=ffx._DEVICE_NOT_CONNECTED,
                            stderr="stderr",
                        ),
                    ],
                },
            ),
            (
                {
                    "label": "when_rcs_connection_timedout",
                    "subprocess_side_effect": [
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="cmd",
                            output="error",
                            stderr="stderr",
                        ),
                        subprocess.TimeoutExpired(
                            timeout=5,
                            cmd="cmd",
                        ),
                    ],
                },
            ),
            (
                {
                    "label": "when_rcs_connection_disconnected_after_few_attempts",
                    "subprocess_side_effect": [
                        RuntimeError("error"),
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="cmd",
                            output=ffx._DEVICE_NOT_CONNECTED,
                            stderr="stderr",
                        ),
                    ],
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch("time.sleep", autospec=True)
    @mock.patch("time.time", side_effect=[0, 1, 2, 3, 4, 5], autospec=True)
    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        autospec=True,
    )
    def test_wait_for_rcs_disconnection(
        self,
        parameterized_output,
        mock_subprocess_check_output,
        mock_time,
        mock_sleep,
    ) -> None:
        """Test case for ffx.wait_for_rcs_disconnection()"""
        mock_subprocess_check_output.side_effect = parameterized_output[
            "subprocess_side_effect"
        ]
        self.ffx_obj.wait_for_rcs_disconnection(timeout=5)

        mock_subprocess_check_output.assert_called()
        mock_time.assert_called()
        mock_sleep.assert_called()

    @mock.patch("time.sleep", autospec=True)
    @mock.patch("time.time", side_effect=[0, 1, 2, 3, 4, 5], autospec=True)
    @mock.patch.object(
        ffx.subprocess,
        "check_output",
        side_effect=subprocess.CalledProcessError(returncode=1, cmd="cmd"),
        autospec=True,
    )
    def test_wait_for_rcs_disconnection_exception(
        self, mock_subprocess_check_output, mock_time, mock_sleep
    ) -> None:
        """Test case for ffx.wait_for_rcs_disconnection() raising
        FfxCommandError"""
        with self.assertRaises(errors.FfxCommandError):
            self.ffx_obj.wait_for_rcs_disconnection(timeout=5)

        mock_subprocess_check_output.assert_called()
        mock_time.assert_called()
        mock_sleep.assert_called()
