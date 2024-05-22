#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Example metric test for the perf metric publishing code."""

import json
import os
import random
from importlib.resources import as_file, files

from mobly import test_runner

import test_data
import perf_publish.publish as publish
from fuchsia_base_test import fuchsia_base_test


class ExampleMetricPublishing(fuchsia_base_test.FuchsiaBaseTest):
    """Example perf metric publishing tests"""

    def test_example_metric_publishing(self) -> None:
        """Example that ensures we correctly publish an example metric."""
        # TODO(b/307637269): improve this. This is an example not a test. We should place it in a
        # better location and potentially improve it.
        fuchsiaperf_data = [
            {
                "test_suite": "fuchsia.example",
                "label": "ExampleMetric1",
                "values": [10 + random.uniform(0, 1)],
                "unit": "ms",
            },
        ]
        test_perf_file = os.path.join(self.log_path, "test.fuchsiaperf.json")
        with open(test_perf_file, "w") as f:
            json.dump(fuchsiaperf_data, f, indent=4)

        with as_file(files(test_data).joinpath("fuchsia.example.txt")) as f:
            publish.publish_fuchsiaperf(
                [test_perf_file],
                str(f),
            )


if __name__ == "__main__":
    test_runner.main()
