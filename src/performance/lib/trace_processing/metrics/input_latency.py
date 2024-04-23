#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""input trace metrics."""

import logging
import statistics
from typing import Any, Dict, Iterable, Iterator, List

import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import trace_processing.trace_utils as trace_utils


_LOGGER: logging.Logger = logging.getLogger("InputLatencyMetricsProcessor")
_AGGREGATE_METRICS_ONLY: str = "aggregateMetricsOnly"
_CATEGORY_INPUT: str = "input"
# TODO: This tracing event is sent when input pipeline sends input events to
# scenic. We should find a better event to include the process time in input
# pipeline. Maybe "touch-binding-process-report".
_INPUT_EVENT_NAME: str = "presentation_on_event"
_CATEGORY_GFX: str = "gfx"
_DISPLAY_VSYNC_EVENT_NAME: str = "Display::Controller::OnDisplayVsync"


def metrics_processor(
    model: trace_model.Model, extra_args: Dict[str, Any]
) -> List[trace_metrics.TestCaseResult]:
    """Computes latency from input reach to input pipeline to vsync.

    Args:
        model: The trace model to process.
        extra_args: Additional arguments to the processor.

    Returns:
        A list of computed metrics.
    """

    return InputLatencyMetricsProcessor(
        aggregates_only=extra_args.get(_AGGREGATE_METRICS_ONLY, False),
    ).process_metrics(model)


class InputLatencyMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes input latency metrics."""

    def __init__(self, aggregates_only: bool = True):
        """Constructor.

        Args:
            aggregates_only: When True, generates InputLatencyMin,
                InputLatencyMax, InputLatencyAverage and
                InputLatencyP* (percentiles).
                Otherwise generates InputLatency metric with all
                InputLatency values.
        """
        self._aggregates_only: bool = aggregates_only

    def process_metrics(
        self, model: trace_model.Model
    ) -> List[trace_metrics.TestCaseResult]:
        all_events: Iterator[trace_model.Event] = model.all_events()
        input_events: Iterable[trace_model.Event] = trace_utils.filter_events(
            all_events,
            category=_CATEGORY_INPUT,
            name=_INPUT_EVENT_NAME,
            type=trace_model.DurationEvent,
        )

        latencies: List[float] = []

        for e in input_events:
            vsync = trace_utils.get_nearest_following_event(
                e, _CATEGORY_GFX, _DISPLAY_VSYNC_EVENT_NAME
            )

            if vsync is None:
                continue

            latency = vsync.start - e.start
            latencies.append(latency.to_milliseconds_f())

        latency_mean: float = statistics.mean(latencies)
        _LOGGER.info(f"Average Present Latency: {latency_mean}")

        if self._aggregates_only:
            return trace_utils.standard_metrics_set(
                values=latencies,
                label_prefix="InputLatency",
                unit=trace_metrics.Unit.milliseconds,
            )
        else:
            return [
                trace_metrics.TestCaseResult(
                    "total_input_latency",
                    trace_metrics.Unit.milliseconds,
                    latencies,
                ),
            ]
