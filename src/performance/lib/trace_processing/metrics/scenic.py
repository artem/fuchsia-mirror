#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Scenic trace metrics."""

import logging
import statistics
from typing import Any, Iterator, Sequence, Tuple

from trace_processing import trace_metrics, trace_model, trace_utils


_LOGGER: logging.Logger = logging.getLogger("ScenicMetricsProcessor")
_AGGREGATE_METRICS_ONLY: str = "aggregateMetricsOnly"
_EVENT_CATEGORY: str = "gfx"
_SCENIC_START_EVENT_NAME: str = "ApplyScheduledSessionUpdates"
_SCENIC_RENDER_EVENT_NAME: str = "RenderFrame"
_DISPLAY_VSYNC_READY_EVENT_NAME: str = "Display::Fence::OnReady"


# DEPRECATED: Use ScenicMetricsProcessor instead.
# TODO(b/320778225): Remove once downstream callers are migrated.
def metrics_processor(
    model: trace_model.Model, extra_args: dict[str, Any]
) -> Sequence[trace_metrics.TestCaseResult]:
    """Computes how long Scenic takes for operations for the given trace.

    Args:
        model: The trace model to process.
        extra_args: Additional arguments to the processor.

    Returns:
        A list of computed metrics.
    """
    return ScenicMetricsProcessor(
        aggregates_only=extra_args.get(_AGGREGATE_METRICS_ONLY, False) is True
    ).process_metrics(model)


class ScenicMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes scenic metrics."""

    def __init__(self, aggregates_only: bool = True):
        """Constructor.

        Args:
            aggregates_only: When True, generates RenderCpu[Min|Max|Average|P*] and
                RenderTotal[Min|Max|Average|P*].
                Otherwise generates RenderCpu and RenderTotal with the raw values.
        """
        self.aggregates_only: bool = aggregates_only

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
        # This method looks for a possible race between trace event start in Scenic and magma.
        # We can safely skip these events. See https://fxbug.dev/322849857 for more details.
        model = trace_utils.adjust_to_common_process_start(
            model, [("scenic.cm", ""), ("driver_host.cm", "DeviceThread")]
        )

        all_events: Iterator[trace_model.Event] = model.all_events()
        # Since `filter_events()` returns an Iterator, make a local copy so we can iterate over the
        # events more than once.
        scenic_start_events = list(
            trace_utils.filter_events(
                all_events,
                category=_EVENT_CATEGORY,
                name=_SCENIC_START_EVENT_NAME,
                type=trace_model.DurationEvent,
            )
        )
        scenic_render_events: list[trace_model.Event] = []
        for e in scenic_start_events:
            following = trace_utils.get_nearest_following_event(
                e, _EVENT_CATEGORY, _SCENIC_RENDER_EVENT_NAME
            )
            if following is not None:
                scenic_render_events.append(following)

        vsync_ready_events: list[trace_model.Event] = []
        for e in scenic_start_events:
            following = trace_utils.get_nearest_following_event(
                e, _EVENT_CATEGORY, _DISPLAY_VSYNC_READY_EVENT_NAME
            )
            if following is not None:
                vsync_ready_events.append(following)

        if len(scenic_render_events) < 1 or len(vsync_ready_events) < 1:
            _LOGGER.info(
                "No render or vsync events are present. Perhaps the trace "
                "duration is too short to provide scenic render information"
            )
            return []

        cpu_render_times: list[float] = []
        for start_event, render_event in zip(
            scenic_start_events, scenic_render_events
        ):
            if render_event is None:
                continue
            assert isinstance(render_event, trace_model.DurationEvent)
            if not render_event.duration:
                continue
            cpu_render_times.append(
                (
                    render_event.start
                    + render_event.duration
                    - start_event.start
                ).to_milliseconds_f()
            )

        cpu_render_mean: float = statistics.mean(cpu_render_times)
        _LOGGER.info(f"Average CPU render time: {cpu_render_mean} ms")

        total_render_times: list[float] = []
        for start_event, vsync_event in zip(
            scenic_start_events, vsync_ready_events
        ):
            if vsync_event is None:
                continue
            total_render_times.append(
                (vsync_event.start - start_event.start).to_milliseconds_f()
            )

        total_render_mean: float = statistics.mean(total_render_times)
        _LOGGER.info(f"Average Total render time: {total_render_mean} ms")

        metrics_list: list[Tuple[str, list[float]]] = [
            ("RenderCpu", cpu_render_times),
            ("RenderTotal", total_render_times),
        ]

        test_case_results: list[trace_metrics.TestCaseResult] = []
        for name, values in metrics_list:
            if self.aggregates_only:
                test_case_results.extend(
                    trace_utils.standard_metrics_set(
                        values=values,
                        label_prefix=name,
                        unit=trace_metrics.Unit.milliseconds,
                    )
                )
            else:
                test_case_results.append(
                    trace_metrics.TestCaseResult(
                        name, trace_metrics.Unit.milliseconds, values
                    )
                )

        return test_case_results
