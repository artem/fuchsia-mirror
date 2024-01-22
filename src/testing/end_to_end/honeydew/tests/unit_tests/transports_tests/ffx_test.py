#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.ffx.py."""

import ipaddress
import subprocess
import unittest
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller
from parameterized import parameterized

from honeydew import custom_types, errors
from honeydew.transports import ffx

# pylint: disable=protected-access
_TARGET_NAME: str = "fuchsia-emulator"

_IPV6: str = "fe80::4fce:3102:ef13:888c%qemu"
_IPV6_OBJ: ipaddress.IPv6Address = ipaddress.IPv6Address(_IPV6)

_SSH_ADDRESS: ipaddress.IPv6Address = _IPV6_OBJ
_SSH_PORT = 8022
_TARGET_SSH_ADDRESS = custom_types.TargetSshAddress(
    ip=_SSH_ADDRESS, port=_SSH_PORT
)

_ISOLATE_DIR: str = "/tmp/isolate"
_LOGS_DIR: str = "/tmp/logs"
_BINARY_PATH: str = "ffx"
_LOGS_LEVEL: str = "debug"
_MDNS_ENABLED: bool = False
_SUBTOOLS_SEARCH_PATH: str = "/subtools"

_FFX_TARGET_SHOW_OUTPUT: bytes = (
    r'[{"title":"Target","label":"target","description":"",'
    r'"child":[{"title":"Name","label":"name","description":"Target name.",'
    r'"value":"fuchsia-emulator"},{"title":"SSH Address",'
    r'"label":"ssh_address","description":"Interface address",'
    r'"value":'
    f'"{_SSH_ADDRESS}:{_SSH_PORT}"'
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
                "value": _TARGET_NAME,
            },
            {
                "title": "SSH Address",
                "label": "ssh_address",
                "description": "Interface address",
                "value": f"{_SSH_ADDRESS}:{_SSH_PORT}",
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
        "nodename": _TARGET_NAME,
        "rcs_state": "Y",
        "serial": "<unknown>",
        "target_type": "workstation_eng.qemu-x64",
        "target_state": "Product",
        "addresses": ["fe80::6a47:a931:1e84:5077%qemu"],
        "is_default": True,
    }
]

_FFX_CONFIG_SET: list[str] = [
    "ffx",
    "--isolate-dir",
    _ISOLATE_DIR,
    "config",
    "set",
]

_INPUT_ARGS: dict[str, Any] = {
    "target_name": _TARGET_NAME,
    "target_ip_port": _TARGET_SSH_ADDRESS,
    "ffx_config": custom_types.FFXConfig(
        isolate_dir=fuchsia_controller.IsolateDir(_ISOLATE_DIR),
        logs_dir=_LOGS_DIR,
        binary_path=_BINARY_PATH,
        logs_level=_LOGS_LEVEL,
        mdns_enabled=_MDNS_ENABLED,
        subtools_search_path=_SUBTOOLS_SEARCH_PATH,
    ),
    "run_cmd": ffx._FFX_CMDS["TARGET_SHOW"],
}

_MOCK_ARGS: dict[str, Any] = {
    "ffx_target_show_output": _FFX_TARGET_SHOW_OUTPUT,
    "ffx_target_show_json": _FFX_TARGET_SHOW_JSON,
    "ffx_target_ssh_address_output": f"[{_SSH_ADDRESS}]:{_SSH_PORT}",
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


class FfxConfigTests(unittest.TestCase):
    """Unit tests for honeydew.transports.ffx.FfxConfig"""

    @mock.patch.object(
        ffx.subprocess,
        "check_call",
        autospec=True,
    )
    def test_setup(self, mock_subprocess_check_call) -> None:
        """Test case for ffx.FfxConfig.setup()"""

        ffx_config = ffx.FfxConfig()

        ffx_config.setup(
            binary_path=_BINARY_PATH,
            isolate_dir=_ISOLATE_DIR,
            logs_dir=_LOGS_DIR,
            logs_level=_LOGS_LEVEL,
            enable_mdns=_MDNS_ENABLED,
            subtools_search_path=_SUBTOOLS_SEARCH_PATH,
        )

        ffx_configs_calls = [
            mock.call(_FFX_CONFIG_SET + ["log.dir", _LOGS_DIR], timeout=10),
            mock.call(
                _FFX_CONFIG_SET + ["log.level", _LOGS_LEVEL.lower()], timeout=10
            ),
            mock.call(
                _FFX_CONFIG_SET
                + ["discovery.mdns.enabled", str(_MDNS_ENABLED).lower()],
                timeout=10,
            ),
            mock.call(
                _FFX_CONFIG_SET
                + ["ffx.subtool-search-paths", _SUBTOOLS_SEARCH_PATH],
                timeout=10,
            ),
        ]
        mock_subprocess_check_call.assert_has_calls(
            ffx_configs_calls, any_order=True
        )

        # Calling setup() again should fail
        with self.assertRaises(errors.FfxConfigError):
            ffx_config.setup(
                binary_path=_BINARY_PATH,
                isolate_dir=_ISOLATE_DIR,
                logs_dir=_LOGS_DIR,
                logs_level=_LOGS_LEVEL,
                enable_mdns=_MDNS_ENABLED,
                subtools_search_path=_SUBTOOLS_SEARCH_PATH,
            )

    @mock.patch.object(
        ffx.subprocess,
        "check_call",
        side_effect=subprocess.CalledProcessError(
            returncode=5,
            cmd="cmd",
            output="output",
        ),
        autospec=True,
    )
    def test_setup_raises_ffx_config_error(
        self, mock_subprocess_check_call
    ) -> None:
        """Test case for ffx.FfxConfig.setup() raises FfxConfigError"""

        ffx_config = ffx.FfxConfig()

        with self.assertRaises(errors.FfxConfigError):
            ffx_config.setup(
                binary_path=_BINARY_PATH,
                isolate_dir=_ISOLATE_DIR,
                logs_dir=_LOGS_DIR,
                logs_level=_LOGS_LEVEL,
                enable_mdns=_MDNS_ENABLED,
                subtools_search_path=_SUBTOOLS_SEARCH_PATH,
            )

        mock_subprocess_check_call.assert_called()

    @mock.patch.object(
        ffx.subprocess,
        "check_call",
        side_effect=subprocess.TimeoutExpired(cmd="cmd", timeout=5),
        autospec=True,
    )
    def test_setup_raises_timeout_error(
        self, mock_subprocess_check_call
    ) -> None:
        """Test case for ffx.FfxConfig.setup() raises subprocess.TimeoutExpired"""

        ffx_config = ffx.FfxConfig()

        with self.assertRaises(subprocess.TimeoutExpired):
            ffx_config.setup(
                binary_path=_BINARY_PATH,
                isolate_dir=_ISOLATE_DIR,
                logs_dir=_LOGS_DIR,
                logs_level=_LOGS_LEVEL,
                enable_mdns=_MDNS_ENABLED,
                subtools_search_path=_SUBTOOLS_SEARCH_PATH,
            )

        mock_subprocess_check_call.assert_called()

    @mock.patch.object(
        ffx.FfxConfig,
        "_run",
        autospec=True,
    )
    def test_close(self, mock_ffx_config_run) -> None:
        """Test case for ffx.FfxConfig.close()"""

        ffx_config = ffx.FfxConfig()

        # Call setup first before calling close
        ffx_config.setup(
            binary_path=_BINARY_PATH,
            isolate_dir=_ISOLATE_DIR,
            logs_dir=_LOGS_DIR,
            logs_level=_LOGS_LEVEL,
            enable_mdns=_MDNS_ENABLED,
            subtools_search_path=_SUBTOOLS_SEARCH_PATH,
        )
        mock_ffx_config_run.assert_called()

        ffx_config.close()

    def test_close_without_setup(self) -> None:
        """Test case for ffx.FfxConfig.close() without calling
        ffx.FfxConfig.setup()"""

        ffx_config = ffx.FfxConfig()

        # Calling setup() again should fail
        with self.assertRaises(errors.FfxConfigError):
            ffx_config.close()

    @mock.patch.object(
        ffx.FfxConfig,
        "_run",
        autospec=True,
    )
    def test_get_config(self, mock_ffx_config_run) -> None:
        """Test case for ffx.FfxConfig.get_config()"""

        ffx_config = ffx.FfxConfig()

        # Call setup first before calling close
        ffx_config.setup(
            binary_path=_BINARY_PATH,
            isolate_dir=_ISOLATE_DIR,
            logs_dir=_LOGS_DIR,
            logs_level=_LOGS_LEVEL,
            enable_mdns=_MDNS_ENABLED,
            subtools_search_path=_SUBTOOLS_SEARCH_PATH,
        )
        mock_ffx_config_run.assert_called()

        self.assertEqual(
            str(ffx_config.get_config()), str(_INPUT_ARGS["ffx_config"])
        )

    def test_get_config_without_setup(self) -> None:
        """Test case for ffx.FfxConfig.get_config() without calling
        ffx.FfxConfig.setup()"""

        ffx_config = ffx.FfxConfig()

        # Calling setup() again should fail
        with self.assertRaises(errors.FfxConfigError):
            ffx_config.get_config()


class FfxTests(unittest.TestCase):
    """Unit tests for honeydew.transports.ffx.FFX"""

    def setUp(self) -> None:
        super().setUp()

        self.ffx_obj_with_ip = ffx.FFX(
            target_name=_INPUT_ARGS["target_name"],
            target_ip_port=_INPUT_ARGS["target_ip_port"],
            config=_INPUT_ARGS["ffx_config"],
        )
        self.ffx_obj_wo_ip = ffx.FFX(
            target_name=_INPUT_ARGS["target_name"],
            config=_INPUT_ARGS["ffx_config"],
        )

    def test_ffx_init_with_ip_as_target_name(self) -> None:
        """Test case for ffx.FFX() when called with target_name=<ip>."""
        with self.assertRaises(ValueError):
            self.ffx_obj_with_ip = ffx.FFX(
                target_name=_IPV6,
                config=_INPUT_ARGS["ffx_config"],
            )

    @mock.patch.object(ffx.FFX, "wait_for_rcs_connection", autospec=True)
    def test_check_connection(self, mock_wait_for_rcs_connection) -> None:
        """Test case for check_connection()"""
        self.ffx_obj_with_ip.check_connection()

        mock_wait_for_rcs_connection.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "wait_for_rcs_connection",
        side_effect=errors.DeviceNotConnectedError(ffx._DEVICE_NOT_CONNECTED),
        autospec=True,
    )
    def test_check_connection_raises(
        self, mock_wait_for_rcs_connection
    ) -> None:
        """Test case for check_connection() raising errors.FfxConnectionError"""
        with self.assertRaises(errors.FfxConnectionError):
            self.ffx_obj_with_ip.check_connection()

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
            self.ffx_obj_with_ip.get_target_information(),
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
            self.ffx_obj_with_ip.get_target_information()

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
            self.ffx_obj_with_ip.get_target_information()

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
            self.ffx_obj_with_ip.get_target_list(),
            parameterized_dict["expected_value"],
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
            self.ffx_obj_with_ip.get_target_list()
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
            self.ffx_obj_with_ip.get_target_ssh_address(), _TARGET_SSH_ADDRESS
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
            self.ffx_obj_with_ip.get_target_ssh_address()

        mock_ffx_run.assert_called()

    @mock.patch.object(
        ffx.FFX,
        "get_target_information",
        return_value=_MOCK_ARGS["ffx_target_show_json"],
        autospec=True,
    )
    def test_get_target_type(self, mock_get_target_information) -> None:
        """Verify ffx.get_target_type returns target type of fuchsia device."""
        result: str = self.ffx_obj_with_ip.get_target_type()
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
            self.ffx_obj_with_ip.run(cmd=_INPUT_ARGS["run_cmd"]),
            _EXPECTED_VALUES["ffx_target_show_output"],
        )

        mock_subprocess_check_output.assert_called_with(
            [
                _BINARY_PATH,
                "-t",
                _IPV6,
                "--isolate-dir",
                _ISOLATE_DIR,
            ]
            + ffx._FFX_CMDS["TARGET_SHOW"],
            stderr=subprocess.STDOUT,
            timeout=10,
        )

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
            self.ffx_obj_with_ip.run(
                cmd=["test", "run", "my-test"], capture_output=False
            ),
            "",
        )

        mock_subprocess_check_output.assert_not_called()
        mock_subprocess_check_call.assert_called_with(
            [
                _BINARY_PATH,
                "-t",
                _IPV6,
                "--isolate-dir",
                _ISOLATE_DIR,
                "test",
                "run",
                "my-test",
            ],
            timeout=10,
        )

    @mock.patch.object(
        ffx.subprocess,
        "check_call",
        return_value=None,
        autospec=True,
    )
    def test_ffx_run_test_component(self, mock_subprocess_check_call) -> None:
        """Test case for ffx.run()"""
        self.assertEqual(
            self.ffx_obj_with_ip.run_test_component(
                "fuchsia-pkg://fuchsia.com/testing#meta/test.cm",
                ffx_test_args=["--foo", "bar"],
                test_component_args=["baz", "--x", "2"],
                capture_output=False,
            ),
            "",
        )

        mock_subprocess_check_call.assert_called_with(
            [
                _BINARY_PATH,
                "-t",
                _IPV6,
                "--isolate-dir",
                _ISOLATE_DIR,
                "test",
                "run",
                "fuchsia-pkg://fuchsia.com/testing#meta/test.cm",
                "--foo",
                "bar",
                "--",
                "baz",
                "--x",
                "2",
            ],
            timeout=10,
        )

    @mock.patch.object(
        ffx.subprocess,
        "Popen",
        return_value=None,
        autospec=True,
    )
    def test_ffx_popen(self, mock_subprocess_popen_call) -> None:
        """Test case for ffx.popen()"""
        self.assertEqual(
            self.ffx_obj_with_ip.popen(
                cmd=["a", "b", "c"],
                # Popen forwards arbitrary kvargs to subprocess.Popen
                text=True,  # example kvarg
                stdout="abc",  # another example kvarg
            ),
            None,
        )

        mock_subprocess_popen_call.assert_called_with(
            [
                _BINARY_PATH,
                "-t",
                _IPV6,
                "--isolate-dir",
                _ISOLATE_DIR,
            ]
            + ["a", "b", "c"],
            text=True,
            stdout="abc",
        )

    @parameterized.expand(
        [
            (
                {
                    "label": "DeviceNotConnectedError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="ffx -t fuchsia-emulator target show",
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
                        cmd="ffx -t fuchsia-emulator target show",
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
                        timeout=10, cmd="ffx -t fuchsia-emulator target show"
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
    def test_ffx_run_exceptions(
        self, parameterized_dict, mock_subprocess_check_output
    ) -> None:
        """Test case for ffx.run() raising different
        exceptions."""
        mock_subprocess_check_output.side_effect = parameterized_dict[
            "side_effect"
        ]

        with self.assertRaises(parameterized_dict["expected_error"]):
            self.ffx_obj_with_ip.run(cmd=_INPUT_ARGS["run_cmd"])

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
            self.ffx_obj_with_ip.run(
                cmd=_INPUT_ARGS["run_cmd"], exceptions_to_skip=[RuntimeError]
            ),
            "",
        )

        mock_subprocess_check_output.assert_called()

    @mock.patch.object(ffx.subprocess, "check_output", autospec=True)
    def test_add_target(self, mock_subprocess_check_output) -> None:
        """Test case for ffx_cli.add_target()."""
        self.ffx_obj_with_ip.add_target()

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
        mock_subprocess_check_output.side_effect = parameterized_dict[
            "side_effect"
        ]

        expected = parameterized_dict["expected"]

        with self.assertRaises(expected):
            self.ffx_obj_with_ip.add_target()

        mock_subprocess_check_output.assert_called_once()

    @mock.patch.object(
        ffx.FFX,
        "get_target_information",
        return_value=_MOCK_ARGS["ffx_target_show_json"],
        autospec=True,
    )
    def test_get_target_name(self, mock_ffx_get_target_information) -> None:
        """Verify get_target_name returns the name of the fuchsia device."""
        self.assertEqual(self.ffx_obj_with_ip.get_target_name(), _TARGET_NAME)

        mock_ffx_get_target_information.assert_called()

    @parameterized.expand(
        [
            ({"label": "empty_output", "side_effect": b"[]"},),
            (
                {
                    "label": "CalledProcessError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd=f"ffx -t '[{_SSH_ADDRESS}]:{_SSH_PORT}' target show",
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
            self.ffx_obj_with_ip.get_target_name()

        mock_ffx_get_target_information.assert_called_once()

    @mock.patch.object(ffx.FFX, "run", return_value="", autospec=True)
    def test_wait_for_rcs_connection(self, mock_ffx_run) -> None:
        """Test case for ffx.wait_for_rcs_connection()"""
        self.ffx_obj_with_ip.wait_for_rcs_connection()
        mock_ffx_run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "DeviceNotConnectedError",
                    "side_effect": errors.DeviceNotConnectedError(
                        "fuchsia-emulator is not connected to host"
                    ),
                    "expected_error": errors.DeviceNotConnectedError,
                },
            ),
            (
                {
                    "label": "FFXCommandError",
                    "side_effect": errors.FfxCommandError(
                        "command 'ffx -t fuchsia-emulator target wait' failed",
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
    @mock.patch.object(ffx.FFX, "run", autospec=True)
    def test_wait_for_rcs_connection_exceptions(
        self, parameterized_dict, mock_ffx_run
    ) -> None:
        """Test case for ffx.wait_for_rcs_connection() raising different
        exceptions."""
        mock_ffx_run.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(parameterized_dict["expected_error"]):
            self.ffx_obj_with_ip.wait_for_rcs_connection()

        mock_ffx_run.assert_called()

    @mock.patch.object(ffx.FFX, "run", return_value="", autospec=True)
    def test_wait_for_rcs_disconnection(self, mock_ffx_run) -> None:
        """Test case for ffx.wait_for_rcs_disconnection()"""
        self.ffx_obj_with_ip.wait_for_rcs_disconnection()
        mock_ffx_run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "DeviceNotConnectedError",
                    "side_effect": errors.DeviceNotConnectedError(
                        "fuchsia-emulator is not connected to host"
                    ),
                    "expected_error": errors.DeviceNotConnectedError,
                },
            ),
            (
                {
                    "label": "FFXCommandError",
                    "side_effect": errors.FfxCommandError(
                        "command 'ffx -t fuchsia-emulator target --wait "
                        "--down' failed",
                    ),
                    "expected_error": errors.FfxCommandError,
                },
            ),
            (
                {
                    "label": "TimeoutExpired",
                    "side_effect": subprocess.TimeoutExpired(
                        timeout=10,
                        cmd="ffx -t fuchsia-emulator target --wait --down",
                    ),
                    "expected_error": subprocess.TimeoutExpired,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(ffx.FFX, "run", autospec=True)
    def test_wait_for_rcs_disconnection_exceptions(
        self, parameterized_dict, mock_ffx_run
    ) -> None:
        """Test case for ffx.wait_for_rcs_disconnection() raising different
        exceptions."""
        mock_ffx_run.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(parameterized_dict["expected_error"]):
            self.ffx_obj_with_ip.wait_for_rcs_disconnection()

        mock_ffx_run.assert_called()
