#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.tracing.py."""

import base64
import os
import tempfile
import unittest
from typing import Any, Callable
from unittest import mock

from parameterized import parameterized, param

from honeydew import errors
from honeydew.affordances.sl4f import tracing as sl4f_tracing
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import sl4f as sl4f_transport


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_{test_label}"


# pylint: disable=protected-access
class TracingSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.sl4f.tracing.py."""

    def setUp(self) -> None:
        super().setUp()
        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice
        )
        self.tracing_obj = sl4f_tracing.Tracing(
            device_name="fuchsia-emulator",
            sl4f=self.sl4f_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )
        self.sl4f_obj.reset_mock()

    @parameterized.expand(
        [
            (
                {
                    "label": "with_no_categories_and_no_buffer_size",
                },
            ),
            (
                {
                    "label": "with_categories_and_buffer_size",
                    "categories": ["category1", "category2"],
                    "buffer_size": 1024,
                },
            ),
            (
                {
                    "label": "when_session_already_initialized",
                    "session_initialized": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_initialize(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for Tracing.initialize() method."""
        self.tracing_obj.initialize(
            categories=parameterized_dict.get("categories"),
            buffer_size=parameterized_dict.get("buffer_size"),
        )
        self.sl4f_obj.run.assert_called()

        # Check whether an `errors.FuchsiaStateError` exception is raised when
        # calling `initialize()` on a session that is already initialized.
        if parameterized_dict.get("session_initialized"):
            with self.assertRaises(errors.FuchsiaStateError):
                self.tracing_obj.initialize()

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_initialized",
                    "session_initialized": True,
                },
            ),
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_start(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for Tracing.start() method."""
        if not parameterized_dict.get("session_initialized"):
            with self.assertRaises(errors.FuchsiaStateError):
                self.tracing_obj.start()
        else:
            self.tracing_obj.initialize()
            self.tracing_obj.start()
            self.sl4f_obj.run.assert_any_call(method="tracing_facade.Start")

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                },
            ),
            (
                {
                    "label": "when_no_trace_is_not_started",
                    "session_initialized": True,
                    "tracing_active": False,
                },
            ),
            (
                {
                    "label": "when_session_is_initialized",
                    "session_initialized": True,
                    "tracing_active": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_stop(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for Tracing.stop() method."""
        if not parameterized_dict.get("session_initialized"):
            with self.assertRaises(errors.FuchsiaStateError):
                self.tracing_obj.stop()
        elif not parameterized_dict.get("tracing_active"):
            with self.assertRaises(errors.FuchsiaStateError):
                self.tracing_obj.stop()
        else:
            self.tracing_obj.initialize()
            self.tracing_obj.start()
            self.tracing_obj.stop()
            self.sl4f_obj.run.assert_any_call(method="tracing_facade.Stop")

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                },
            ),
            (
                {
                    "label": "with_no_download",
                    "session_initialized": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_terminate(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for Tracing.terminate() method."""

        if not parameterized_dict.get("session_initialized"):
            with self.assertRaises(errors.FuchsiaStateError):
                self.tracing_obj.terminate()
        else:
            # Initialize the tracing session.
            self.tracing_obj.initialize()
            self.tracing_obj.terminate()
            self.sl4f_obj.run.assert_any_call(
                method="tracing_facade.Terminate",
                params={"results_destination": "Ignore"},
            )

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                },
            ),
            (
                {
                    "label": "with_tracing_download_default_file_name",
                    "session_initialized": True,
                    "return_value": {
                        "data": base64.b64encode(
                            "samp_trace_data".encode("utf-8")
                        ),
                    },
                },
            ),
            (
                {
                    "label": "with_tracing_download_given_file_name",
                    "session_initialized": True,
                    "trace_file": "trace.fxt",
                    "return_value": {
                        "data": base64.b64encode(
                            "samp_trace_data".encode("utf-8")
                        ),
                    },
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_terminate_and_download(
        self, parameterized_dict: dict[str, Any]
    ) -> None:
        """Test for Tracing.terminate_and_download() method."""

        with tempfile.TemporaryDirectory() as tmpdir:
            if not parameterized_dict.get("session_initialized"):
                with self.assertRaises(errors.FuchsiaStateError):
                    self.tracing_obj.terminate_and_download(directory=tmpdir)
            else:
                trace_file: str = parameterized_dict.get("trace_file", "")
                # Initialize the tracing session.
                self.tracing_obj.initialize()
                return_value: str = parameterized_dict.get("return_value", "")
                self.sl4f_obj.run.return_value = return_value

                trace_path: str = self.tracing_obj.terminate_and_download(
                    directory=tmpdir, trace_file=trace_file
                )
                self.sl4f_obj.run.assert_any_call(
                    method="tracing_facade.Terminate"
                )

                # Check the return value of the terminate method.
                if trace_file:
                    self.assertEqual(trace_path, f"{tmpdir}/{trace_file}")
                else:
                    self.assertRegex(trace_path, f"{tmpdir}/trace_.*.fxt")

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_is_not_initialized",
                    "session_initialized": False,
                },
            ),
            (
                {
                    "label": "when_session_is_initialized",
                    "session_initialized": True,
                },
            ),
            (
                {
                    "label": "with_tracing_download_given_file_name",
                    "download_trace": True,
                    "trace_file": "trace.fxt",
                    "return_value": {
                        "data": base64.b64encode(
                            "samp_trace_data".encode("utf-8")
                        ),
                    },
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_trace_session(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for Tracing.trace_session() method."""
        if parameterized_dict.get("session_initialized"):
            self.tracing_obj.initialize()

        with tempfile.TemporaryDirectory() as tmpdir:
            return_value: str = parameterized_dict.get("return_value", "")
            self.sl4f_obj.run.return_value = return_value

            trace_file: str = parameterized_dict.get("trace_file", "")
            download_trace: bool = parameterized_dict.get(
                "download_trace", False
            )
            with self.tracing_obj.trace_session(
                download=download_trace, directory=tmpdir, trace_file=trace_file
            ):
                self.sl4f_obj.run.assert_any_call(method="tracing_facade.Start")

            self.sl4f_obj.run.assert_any_call(method="tracing_facade.Stop")

            if download_trace:
                self.sl4f_obj.run.assert_any_call(
                    method="tracing_facade.Terminate"
                )
                trace_path: str = os.path.join(tmpdir, trace_file)
                self.assertTrue(os.path.exists(trace_path))
            else:
                self.sl4f_obj.run.assert_any_call(
                    method="tracing_facade.Terminate",
                    params={"results_destination": "Ignore"},
                )


if __name__ == "__main__":
    unittest.main()
