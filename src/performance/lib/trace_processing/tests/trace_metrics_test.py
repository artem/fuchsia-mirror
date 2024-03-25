#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for trace metrics processors."""

import json
import os
import pathlib
from parameterized import parameterized, param
import tempfile
import unittest

import trace_processing.metrics.cpu as cpu_metrics
import trace_processing.metrics.fps as fps_metrics
import trace_processing.metrics.scenic as scenic_metrics
import trace_processing.trace_importing as trace_importing
import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model


# Boilerplate-busting constants:
U = trace_metrics.Unit
TCR = trace_metrics.TestCaseResult

_EMPTY_MODEL = trace_model.Model()


class TestCaseResultTest(unittest.TestCase):
    """Tests TestCaseResult"""

    def test_to_json(self) -> None:
        label = "L1"
        test_suite = "bar"

        result = TCR(
            label=label, unit=U.bytesPerSecond, values=[0, 0.1, 23.45, 6]
        )

        self.assertEqual(result.label, label)
        self.assertEqual(
            result.to_json(test_suite=test_suite),
            {
                "label": label,
                "test_suite": test_suite,
                "unit": "bytes/second",
                "values": [0, 0.1, 23.45, 6],
            },
        )

    def test_write_fuchsia_perf_json(self) -> None:
        test_suite = "ts"
        results = [
            TCR(label="l1", unit=U.percent, values=[1]),
            TCR(label="l2", unit=U.framesPerSecond, values=[2]),
        ]

        with tempfile.TemporaryDirectory() as tmpdir:
            actual_output_path = (
                pathlib.Path(tmpdir) / "actual_output.fuchsiaperf.json"
            )

            trace_metrics.TestCaseResult.write_fuchsiaperf_json(
                results,
                test_suite=test_suite,
                output_path=actual_output_path,
            )

            actual_output = json.loads(actual_output_path.read_text())
            self.assertEqual(
                actual_output, [r.to_json(test_suite) for r in results]
            )


class MetricProcessorsTest(unittest.TestCase):
    """Tests for the various MetricProcessors."""

    def _load_model(self, model_file_name) -> trace_model.Model:
        # A second dirname is required to account for the .pyz archive which
        # contains the test and a third one since data is a sibling of the test.
        runtime_deps_path = os.path.join(
            os.path.dirname(os.path.dirname(os.path.dirname(__file__))),
            "runtime_deps",
        )
        return trace_importing.create_model_from_file_path(
            os.path.join(runtime_deps_path, model_file_name)
        )

    def test_process_and_save(self) -> None:
        test_suite = "ts"
        expected_results = [
            TCR(label="test", unit=U.countBiggerIsBetter, values=[1234, 5678])
        ]
        processor = trace_metrics.ConstantMetricsProcessor(
            results=expected_results
        )

        with tempfile.TemporaryDirectory() as tmpdir:
            output_path = (
                pathlib.Path(tmpdir) / "actual_output.fuchsiaperf.json"
            )
            processor.process_and_save_metrics(
                _EMPTY_MODEL, test_suite, output_path
            )
            actual_output = json.loads(output_path.read_text())
            self.assertEqual(
                actual_output, [r.to_json(test_suite) for r in expected_results]
            )

    def test_constant_processor(self) -> None:
        expected_results = [
            TCR(label="test", unit=U.countBiggerIsBetter, values=[1234, 5678])
        ]
        processor = trace_metrics.ConstantMetricsProcessor(
            results=expected_results
        )
        actual_results = processor.process_metrics(_EMPTY_MODEL)

        self.assertEqual(actual_results, expected_results)

    def test_processors_set(self) -> None:
        expected_results1 = [
            TCR(label="l1", unit=U.countBiggerIsBetter, values=[1234, 5678])
        ]
        expected_results2 = [
            TCR(label="l2", unit=U.framesPerSecond, values=[29.9])
        ]

        processor = trace_metrics.MetricsProcessorsSet(
            sub_processors=[
                trace_metrics.ConstantMetricsProcessor(
                    results=expected_results1
                ),
                trace_metrics.ConstantMetricsProcessor(
                    results=expected_results2
                ),
            ]
        )
        actual_results = processor.process_metrics(_EMPTY_MODEL)
        self.assertEqual(actual_results, expected_results1 + expected_results2)

    @parameterized.expand(
        [
            param(
                "cpu",
                processor=cpu_metrics.CpuMetricsProcessor(
                    aggregates_only=False
                ),
                model_file="cpu_metric.json",
                expected_results=[
                    TCR(label="CpuLoad", unit=U.percent, values=[43, 20]),
                ],
            ),
            param(
                "cpu_from_system_metrics_logger",
                processor=cpu_metrics.CpuMetricsProcessor(
                    aggregates_only=False
                ),
                model_file="cpu_metric_system_metrics_logger.json",
                expected_results=[
                    TCR(label="CpuLoad", unit=U.percent, values=[43, 20]),
                ],
            ),
            param(
                "cpu_aggregates",
                processor=cpu_metrics.CpuMetricsProcessor(aggregates_only=True),
                model_file="cpu_metric.json",
                expected_results=[
                    TCR(label="CpuP5", unit=U.percent, values=[21.15]),
                    TCR(label="CpuP25", unit=U.percent, values=[25.75]),
                    TCR(label="CpuP50", unit=U.percent, values=[31.5]),
                    TCR(label="CpuP75", unit=U.percent, values=[37.25]),
                    TCR(label="CpuP95", unit=U.percent, values=[41.85]),
                    TCR(label="CpuMin", unit=U.percent, values=[20]),
                    TCR(label="CpuMax", unit=U.percent, values=[43]),
                    TCR(label="CpuAverage", unit=U.percent, values=[31.5]),
                ],
            ),
            param(
                "cpu_aggregates_from_system_metrics_logger",
                processor=cpu_metrics.CpuMetricsProcessor(aggregates_only=True),
                model_file="cpu_metric_system_metrics_logger.json",
                expected_results=[
                    TCR(label="CpuP5", unit=U.percent, values=[21.15]),
                    TCR(label="CpuP25", unit=U.percent, values=[25.75]),
                    TCR(label="CpuP50", unit=U.percent, values=[31.5]),
                    TCR(label="CpuP75", unit=U.percent, values=[37.25]),
                    TCR(label="CpuP95", unit=U.percent, values=[41.85]),
                    TCR(label="CpuMin", unit=U.percent, values=[20]),
                    TCR(label="CpuMax", unit=U.percent, values=[43]),
                    TCR(label="CpuAverage", unit=U.percent, values=[31.5]),
                ],
            ),
            param(
                "fps",
                processor=fps_metrics.FpsMetricsProcessor(
                    aggregates_only=False
                ),
                model_file="fps_metric.json",
                expected_results=[
                    TCR(
                        label="Fps",
                        unit=U.framesPerSecond,
                        values=[10000000.0, 5000000.0],
                    )
                ],
            ),
            param(
                "fps_aggregates",
                processor=fps_metrics.FpsMetricsProcessor(aggregates_only=True),
                model_file="fps_metric.json",
                expected_results=[
                    TCR(
                        label="FpsP5",
                        unit=U.framesPerSecond,
                        values=[5250000.0],
                    ),
                    TCR(
                        label="FpsP25",
                        unit=U.framesPerSecond,
                        values=[6250000.0],
                    ),
                    TCR(
                        label="FpsP50",
                        unit=U.framesPerSecond,
                        values=[7500000.0],
                    ),
                    TCR(
                        label="FpsP75",
                        unit=U.framesPerSecond,
                        values=[8750000.0],
                    ),
                    TCR(
                        label="FpsP95",
                        unit=U.framesPerSecond,
                        values=[9750000.0],
                    ),
                    TCR(
                        label="FpsMin",
                        unit=U.framesPerSecond,
                        values=[5000000.0],
                    ),
                    TCR(
                        label="FpsMax",
                        unit=U.framesPerSecond,
                        values=[10000000.0],
                    ),
                    TCR(
                        label="FpsAverage",
                        unit=U.framesPerSecond,
                        values=[7500000.0],
                    ),
                ],
            ),
            param(
                "scenic",
                processor=scenic_metrics.ScenicMetricsProcessor(
                    aggregates_only=False
                ),
                model_file="scenic_metric.json",
                expected_results=[
                    TCR(
                        label="RenderCpu",
                        unit=U.milliseconds,
                        values=[0.09, 0.08, 0.1],
                    ),
                    TCR(
                        label="RenderTotal",
                        unit=U.milliseconds,
                        values=[0.11, 0.112],
                    ),
                ],
            ),
            param(
                "scenic_aggregates",
                processor=scenic_metrics.ScenicMetricsProcessor(
                    aggregates_only=True
                ),
                model_file="scenic_metric.json",
                expected_results=[
                    TCR(
                        label="RenderCpuP5", unit=U.milliseconds, values=[0.081]
                    ),
                    TCR(
                        label="RenderCpuP25",
                        unit=U.milliseconds,
                        values=[0.08499999999999999],
                    ),
                    TCR(
                        label="RenderCpuP50", unit=U.milliseconds, values=[0.09]
                    ),
                    TCR(
                        label="RenderCpuP75",
                        unit=U.milliseconds,
                        values=[0.095],
                    ),
                    TCR(
                        label="RenderCpuP95",
                        unit=U.milliseconds,
                        values=[0.099],
                    ),
                    TCR(
                        label="RenderCpuMin", unit=U.milliseconds, values=[0.08]
                    ),
                    TCR(
                        label="RenderCpuMax", unit=U.milliseconds, values=[0.1]
                    ),
                    TCR(
                        label="RenderCpuAverage",
                        unit=U.milliseconds,
                        values=[0.09],
                    ),
                    TCR(
                        label="RenderTotalP5",
                        unit=U.milliseconds,
                        values=[0.1101],
                    ),
                    TCR(
                        label="RenderTotalP25",
                        unit=U.milliseconds,
                        values=[0.1105],
                    ),
                    TCR(
                        label="RenderTotalP50",
                        unit=U.milliseconds,
                        values=[0.111],
                    ),
                    TCR(
                        label="RenderTotalP75",
                        unit=U.milliseconds,
                        values=[0.1115],
                    ),
                    TCR(
                        label="RenderTotalP95",
                        unit=U.milliseconds,
                        values=[0.1119],
                    ),
                    TCR(
                        label="RenderTotalMin",
                        unit=U.milliseconds,
                        values=[0.11],
                    ),
                    TCR(
                        label="RenderTotalMax",
                        unit=U.milliseconds,
                        values=[0.112],
                    ),
                    TCR(
                        label="RenderTotalAverage",
                        unit=U.milliseconds,
                        values=[0.111],
                    ),
                ],
            ),
        ]
    )
    def test_processor(
        self,
        _: str,
        processor: trace_metrics.MetricsProcessor,
        model_file: str,
        expected_results: list[TCR],
    ) -> None:
        """Tests a processor's outputs with a given input model loaded from a json file"""
        model = self._load_model(model_file)
        actual_results = processor.process_metrics(model)

        # Improves assertEqual output when comparing lists.
        self.maxDiff = 10000
        self.assertEquals(actual_results, expected_results)
