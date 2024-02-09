#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for FFX transport."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing import custom_types

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FFXTransportTests(fuchsia_base_test.FuchsiaBaseTest):
    """FFX transport tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_check_connection(self) -> None:
        """Test case for FFX.check_connection()."""
        self.device.ffx.check_connection()

    def test_get_target_information(self) -> None:
        """Test case for FFX.get_target_information()."""
        self.device.ffx.get_target_information()

    def test_get_target_list(self) -> None:
        """Test case for FFX.get_target_list()."""
        asserts.assert_true(
            len(self.device.ffx.get_target_list()) >= 1,
            msg=f"{self.device.device_name} is not connected",
        )

    def test_get_target_name(self) -> None:
        """Test case for FFX.get_target_name()."""
        asserts.assert_equal(
            self.device.ffx.get_target_name(), self.device.device_name
        )

    def test_get_target_ssh_address(self) -> None:
        """Test case for FFX.get_target_ssh_address()."""
        asserts.assert_is_instance(
            self.device.ffx.get_target_ssh_address(),
            custom_types.TargetSshAddress,
        )

    def test_get_target_type(self) -> None:
        """Test case for FFX.get_target_type()."""
        target_type: str = self.device.ffx.get_target_type()
        # Note - If "target_type" is specified in "expected_values" in
        # params.yml then compare with it.
        if self.user_params["expected_values"] and self.user_params[
            "expected_values"
        ].get("target_type"):
            asserts.assert_equal(
                target_type, self.user_params["expected_values"]["target_type"]
            )
        else:
            asserts.assert_is_not_none(target_type)
            asserts.assert_is_instance(target_type, str)

    def test_ffx_run(self) -> None:
        """Test case for FFX.run()."""
        cmd: list[str] = ["target", "ssh", "ls"]
        self.device.ffx.run(cmd)

    def test_ffx_run_subtool(self) -> None:
        """Test case for FFX.run() with a subtool.

        This test requires the test to have `test_data_deps=["//src/developer/ffx/tools/power:ffx_power_test_data"]`
        to ensure the subtool exists.
        """
        cmd: list[str] = ["power", "help"]
        self.device.ffx.run(cmd)

    def test_wait_for_rcs_connection(self) -> None:
        """Test case for FFX.wait_for_rcs_connection()."""
        self.device.ffx.wait_for_rcs_connection()

    def test_ffx_run_test_component(self) -> None:
        """Test case for FFX.run_test_component()."""
        output: str = self.device.ffx.run_test_component(
            "fuchsia-pkg://fuchsia.com/hello-world-rust-tests#meta/hello-world-rust-tests.cm",
        )
        asserts.assert_in("PASSED", output)


if __name__ == "__main__":
    test_runner.main()
