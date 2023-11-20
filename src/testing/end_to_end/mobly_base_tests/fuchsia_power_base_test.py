#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia power measurement test class.

It is assumed that this is a hybrid test, with a host-side and a single target-side component.
"""

import logging
import os
import signal
import subprocess
import time

from fuchsia_base_hybrid_test_lib import fuchsia_hybrid_base_test
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaPowerBaseTest(fuchsia_hybrid_base_test.FuchsiaHybridBaseTest):
    """Fuchsia power measurement base test class.

    Single device hybrid test with power measurement.

    Attributes:
        fuchsia_devices: List of FuchsiaDevice objects.
        test_case_path: Directory pointing to a specific test case artifacts.
        snapshot_on: `snapshot_on` test param value converted into SnapshotOn
            Enum.
        power_trace_path: Path to power trace CSV.
        metric_name: Name of power metric being measured.

    Required Mobly Test Params:
        ffx_test_options (list[str]): Test options to supply to `ffx test run`
        ffx_test_url (str): Test URL to execute via `ffx test run`
        timeout_sec (int): Test timeout.
        power_metric (str): Name of power metric being measured.
    """

    def __init__(self, config):
        super().__init__(config)
        self.metric_name = self.user_params["power_metric"]
        self.power_trace_path = os.path.join(
            self.log_path, f"{self.metric_name}_power_trace.csv"
        )

    def _find_measurepower_path(self):
        path = os.environ.get("MEASUREPOWER_PATH")
        if not path:
            raise RuntimeError("MEASUREPOWER_PATH env variable must be set")
        return path

    def _wait_first_sample(self, proc):
        for i in range(10):
            if proc.poll():
                stdout = proc.stdout.read()
                stderr = proc.stderr.read()
                raise RuntimeError(
                    f"Measure power failed to start with status "
                    f"{proc.returncode} stdout: {stdout} "
                    f"stderr: {stderr}"
                )
            if (
                os.path.isfile(self.power_trace_path)
                and os.path.getsize(self.power_trace_path) > 0
            ):
                return
            time.sleep(1)
        raise RuntimeError(
            f"Timed out while waiting to start power measurement"
        )

    def _start_power_measurement(self):
        measurepower_path = self._find_measurepower_path()
        cmd = [
            measurepower_path,
            "-format",
            "csv",
            "-out",
            self.power_trace_path,
        ]
        _LOGGER.info(f"STARTING POWER MEASUREMENT: {cmd}")
        return subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )

    def _stop_power_measurement(self, proc: subprocess.Popen):
        _LOGGER.info(f"STOPPING POWER MEASUREMENT (process {proc.pid})")
        proc.send_signal(signal.SIGINT)
        result = proc.wait(60)
        if result:
            stdout = proc.stdout.read()
            stderr = proc.stderr.read()
            raise RuntimeError(
                f"Measure power failed with status "
                f"{proc.returncode} stdout: {stdout} "
                f"stderr: {stderr}"
            )

    def test_launch_hermetic_test(self) -> None:
        """Executes a target-side workload while collecting power measurements.

        Power measurement result is streamed to |self.power_trace_path|.
        """
        with self._start_power_measurement() as proc:
            self._wait_first_sample(proc)
            super().test_launch_hermetic_test()
            self._stop_power_measurement(proc)


if __name__ == "__main__":
    test_runner.main()
