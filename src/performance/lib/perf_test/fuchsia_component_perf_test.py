# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Provides the implementation for simple performance tests which run a test
component that publishes a fuchsiaperf.json file.
"""

import os

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from perf_publish import publish
from perf_test_utils import utils
from mobly import test_runner


class FuchsiaComponentPerfTest(fuchsia_base_test.FuchsiaBaseTest):
    """
    Mobly test class allowing to run a test component and publish its fuchsiaperf data.

    Required Mobly Test Params:
        expected_metric_names_filepath (str): Name of the file with the metric allowlist.
        ffx_test_url (str): Test URL to execute via `ffx test run`.

    Optional Mobly Test Params:
        ffx_test_args(list[str]): Test options to supply to `ffx test run`
            Default: []
        test_component_args (list[str]): Options to supply to the test component.
            Default: []
        results_path_test_arg (str): The option to be used by the test for the results path.
            Important: this is just the option (ex: "--out"). The value will be passed by the test.
            Default: None
        process_runs (int): Number of times to run the test component.
            Default: 1
    """

    def test_fuchsia_component(self) -> None:
        """Run a test component in the device and publish its perf data.

        This function launches a test component in the device, collects its
        fuchsiaperf data and then publishes it ensuring that the expected
        metrics are present.
        """
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self.ffx_test_args: list[str] = self.user_params["ffx_test_args"]
        self.ffx_test_url: str = self.user_params["ffx_test_url"]
        self.expected_metric_names_filepath: str = self.user_params[
            "expected_metric_names_filepath"
        ]
        self.process_runs = self.user_params.get("process_runs", 1)
        self.results_path_test_arg = self.user_params.get(
            "results_path_test_arg"
        )
        self.test_component_args = self.user_params.get(
            "test_component_args", []
        )

        results_file_path: str = utils.DEFAULT_TARGET_RESULTS_PATH
        if self.results_path_test_arg:
            if self.results_path_test_arg.endswith("="):
                self.test_component_args.append(
                    f"{self.results_path_test_arg}{results_file_path}"
                )
            else:
                self.test_component_args += [
                    self.results_path_test_arg,
                    results_file_path,
                ]
        else:
            self.test_component_args.append(results_file_path)

        result_files: list[str] = utils.run_test_component(
            self.device.ffx,
            self.ffx_test_url,
            self.test_case_path,
            ffx_test_args=self.ffx_test_args,
            test_component_args=self.test_component_args,
            process_runs=self.process_runs,
        )

        publish.publish_fuchsiaperf(
            result_files,
            os.path.basename(self.expected_metric_names_filepath),
        )


if __name__ == "__main__":
    test_runner.main()
