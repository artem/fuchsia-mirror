#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Implements BaseDriver for the local execution environment."""

from typing import Any, Dict, List, Optional

import yaml

import api_infra
import api_mobly
import api_ffx
import base_mobly_driver
import common


class LocalDriver(base_mobly_driver.BaseDriver):
    """Local Mobly test driver.

    This driver is used when executing Mobly tests in the local environment.
    In the local environment, it is assumed that users have full knowledge of
    the physical testbed that will be used during the Mobly test so LocalDriver
    allows for the Mobly |config_path| to be supplied directly by the user.
    """

    def __init__(
        self,
        ffx_path: str,
        multi_device: bool = False,
        log_path: Optional[str] = None,
        config_path: Optional[str] = None,
        params_path: Optional[str] = None,
    ) -> None:
        """Initializes the instance.

        Args:
          ffx_path: absolute path to the FFX binary.
          multi_device: whether the Mobly test requires 2+ devices to run.
          log_path: absolute path to directory for storing Mobly test output.
          config_path: absolute path to the Mobly test config file.
          params_path: absolute path to the Mobly test params file.

        Raises:
          KeyError if required environment variables not found.
        """
        super().__init__(
            ffx_path=ffx_path, log_path=log_path, params_path=params_path
        )
        self._multi_device = multi_device
        self._config_path = config_path
        self._ffx_client = api_ffx.FfxClient(ffx_path)

    def _get_test_targets(self) -> List[str]:
        """Returns Fuchsia target names to use in Mobly test.

        * If multi-device test, return all discovered target(s).
        * If single-device test and default device is not set, return all
          discovered target(s).
        * If single-device test and default device is set, return only default
          target(s).

        Returns:
          A list of Fuchsia target names.

        Raises:
          common.DriverException if device discovery command fails or no devices
            detected.
        """
        try:
            res: api_ffx.TargetListResult = self._ffx_client.target_list(
                # Run without isolate dir to access relevant "default" device.
                isolate_dir=None
            )
        except (api_ffx.CommandException, api_ffx.OutputFormatException) as e:
            raise common.DriverException(
                "Failed to enumarate local targets: {e}"
            )

        test_targets: List[str] = res.all_nodes
        if self._multi_device:
            print(f"Multi-device: test with all discovered target(s).")
        elif not res.default_nodes:
            print(f"No default target set: test with all discovered target(s).")
        else:
            print(f"Default target set: test with default target(s).")
            test_targets = res.default_nodes

        if len(test_targets) == 0:
            # Raise exception here because any meaningful Mobly test should run
            # against at least one Fuchsia target.
            raise common.DriverException("No devices found.")

        print(f"Target(s) to use in Mobly test: {test_targets}")
        return test_targets

    def _generate_config_from_env(self) -> api_mobly.MoblyConfigComponent:
        """Returns Mobly device config generated from local environment.

        Best effort config generation based on Fuchsia device discovery on local
        host.

        Returns:
          A list of Fuchsia target names.

        Raises:
          common.InvalidFormatException if unable to extract target names from
            device discovery output.
          common.DriverException if device discovery command fails or no devices
            detected.
        """
        mobly_controllers: List[str] = []
        for target in self._get_test_targets():
            mobly_controllers.append(
                {
                    "type": api_infra.FUCHSIA_DEVICE,
                    "name": target,
                    # Assume connected devices are provisioned with default
                    # Fuchsia.git SSH credentials.
                    "ssh_private_key": "~/.ssh/fuchsia_ed25519",
                }
            )

        return api_mobly.new_testbed_config(
            testbed_name="GeneratedLocalTestbed",
            log_path=self._log_path,
            ffx_path=self._ffx_path,
            mobly_controllers=mobly_controllers,
            test_params_dict={},
            botanist_honeydew_map={},
        )

    def generate_test_config(self, transport: Optional[str] = None) -> str:
        """Returns a Mobly test config in YAML format.

        The Mobly test config is a required input file of any Mobly tests.
        It includes information on the DUT(s) and specifies test parameters.

        Example output:
        ---
        TestBeds:
        - Name: SomeName
          Controllers:
            FuchsiaDevice:
            - name: fuchsia-1234-5678-90ab
          TestParams:
            param_1: "val_1"
            param_2: "val_2"

        If |params_path| is specified in LocalDriver(), then its content is
        added to the Mobly test config; otherwise, the test config is returned
        as-is but in YAML form.

        Args:
          transport: host->device transport type to use.

        Returns:
          A YAML string that represents a Mobly test config.

        Raises:
          common.InvalidFormatException if the test params or tb config files
            are not valid YAML documents.
          common.DriverException if Mobly config generation fails.
        """
        config: Dict[str, Any] = {}
        if self._config_path is None:
            print("Generating Mobly config from environment...")
            print("(To override, provide path to YAML via `config_yaml_path`)")
            try:
                config = self._generate_config_from_env()
            except (common.DriverException, common.InvalidFormatException) as e:
                raise common.DriverException(
                    f"Local config generation failed: {e}"
                )
        else:
            print("Using provided Mobly config YAML...")
            try:
                config = common.read_yaml_from_file(self._config_path)
            except (IOError, OSError) as e:
                raise common.DriverException(f"Local config parse failed: {e}")
            api_mobly.set_ffx_path(config, self._ffx_path)

        if self._params_path:
            test_params = common.read_yaml_from_file(self._params_path)
            config = api_mobly.get_config_with_test_params(config, test_params)

        if transport:
            api_mobly.set_transport(config, transport)

        return yaml.dump(config)

    def teardown(self, *args) -> None:
        pass
