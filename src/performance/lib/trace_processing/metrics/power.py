#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Power trace metrics."""

import dataclasses
import itertools
import logging
from typing import Iterable, Iterator, Sequence

from trace_processing import trace_metrics, trace_model, trace_time, trace_utils

_LOGGER: logging.Logger = logging.getLogger(__name__)


@dataclasses.dataclass
class PowerMetricSample:
    """A sample of collected power metrics.

    Args:
      timestamp: timestamp of sample in nanoseconds since epoch.
      voltage: voltage in Volts.
      current: current in milliAmpere.
      raw_aux: (optional) The raw 16 bit fine aux channel reading from a Monsoon power monitor.  Can
               optionally be used for log synchronization and alignment
    """

    timestamp: int
    voltage: float
    current: float
    raw_aux: int | None

    @property
    def power(self) -> float:
        """Power in Watts."""
        return self.voltage * self.current * 1e-3


def _running_avg(avg: float, value: float, count: int) -> float:
    return avg + (value - avg) / count


@dataclasses.dataclass
class AggregatePowerMetrics:
    """Aggregate power metrics representation.

    Represents aggregated metrics over a number of power metrics samples.

    Args:
      sample_count: number of power metric samples.
      max_power: maximum power in Watts over all samples.
      mean_power: average power in Watts over all samples.
      min_power: minimum power in Watts over all samples.
    """

    sample_count: int = 0
    max_power: float = float("-inf")
    mean_power: float = 0
    min_power: float = float("inf")

    def process_sample(self, sample: PowerMetricSample) -> None:
        """Process a sample of power metrics.

        Args:
            sample: A sample of power metrics.
        """
        self.sample_count += 1
        self.max_power = max(self.max_power, sample.power)
        self.mean_power = _running_avg(
            self.mean_power, sample.power, self.sample_count
        )
        self.min_power = min(self.min_power, sample.power)

    def to_fuchsiaperf_results(
        self, tag: str = ""
    ) -> list[trace_metrics.TestCaseResult]:
        """Converts Power metrics to fuchsiaperf JSON object.

        Returns:
          List of JSON object.
        """
        results: list[trace_metrics.TestCaseResult] = [
            trace_metrics.TestCaseResult(
                label="MinPower" + tag,
                unit=trace_metrics.Unit.watts,
                values=[self.min_power],
            ),
            # TODO(cmasone): Add MedianPower metrics
            trace_metrics.TestCaseResult(
                label="MeanPower" + tag,
                unit=trace_metrics.Unit.watts,
                values=[self.mean_power],
            ),
            trace_metrics.TestCaseResult(
                label="MaxPower" + tag,
                unit=trace_metrics.Unit.watts,
                values=[self.max_power],
            ),
        ]
        return results


class PowerMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes power consumption metrics."""

    def find_test_start(
        self, model: trace_model.Model
    ) -> trace_time.TimePoint | None:
        """Identify the point at which the test workload began.

        In order to sync power measurements with a system trace, our test harness
        runs a process on-device that generates structured CPU load. This process
        is named `load_generator.cm`. The first TimePoint after all the threads of
        this process exit is the first moment that data should be used for metrics
        calculation.

        Args:
            model: In-memory representation of a merged power and system trace.

        Returns:
            The first TimePoint after power/system trace sync signals complete.
        """
        load_generator: trace_model.Process | None = None
        for proc in model.processes:
            if proc.name and proc.name.startswith("load_generator"):
                load_generator = proc
                break

        if not load_generator:
            return None

        load_generator_threads = [t.tid for t in load_generator.threads]

        def is_last_generator_thread_record(
            r: trace_model.ContextSwitch,
        ) -> bool:
            return (
                r.outgoing_tid in load_generator_threads
                and r.outgoing_state
                == trace_model.ThreadState.ZX_THREAD_STATE_DEAD
            )

        records = sorted(
            filter(
                is_last_generator_thread_record,
                trace_utils.filter_records(
                    itertools.chain.from_iterable(
                        model.scheduling_records.values()
                    ),
                    trace_model.ContextSwitch,
                ),
            ),
            key=lambda r: r.start,
        )
        return records[-1].start if records else None

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
        """Calculate power metrics, excluding power/trace sync signals.

        In order to sync power measurements with a system trace, our test harness
        generates some structured CPU load (and corresponding power consumption data).
        This portion of the data must be excluded from metrics calculation.

        Args:
            model: In-memory representation of a merged power and system trace.

        Returns:
            Set of metrics results for this test case.
        """
        test_start = self.find_test_start(model)
        if test_start is None:
            _LOGGER.info(
                "No load_generator scheduling records present. Power data may not have been "
                "merged into the model."
            )
            return []

        post_sync_model = model.slice(test_start)
        metrics_events = trace_utils.filter_events(
            post_sync_model.all_events(),
            category="Metrics",
            name="Metrics",
            type=trace_model.CounterEvent,
        )
        power_metrics = AggregatePowerMetrics()
        for me in metrics_events:
            # These args are set in append_power_data()
            # found in //src/tests/end_to_end/power/power_test_utils.py
            if "Voltage" in me.args and "Current" in me.args:
                sample = PowerMetricSample(
                    timestamp=me.start.to_epoch_delta().to_nanoseconds(),
                    voltage=float(me.args["Voltage"]),
                    current=float(me.args["Current"]),
                    raw_aux=int(me.args["Raw Aux"])
                    if "Raw_Aux" in me.args
                    else None,
                )
                power_metrics.process_sample(sample)

        return power_metrics.to_fuchsiaperf_results(tag="_by_model")
