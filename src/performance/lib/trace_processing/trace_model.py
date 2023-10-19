# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Trace model data structures."""

import copy
import enum
from typing import Any, Dict, Iterator, List, Optional, Self

import trace_processing.trace_time as trace_time


class InstantEventScope(enum.Enum):
    """Scope within a trace that an `InstantEvent` applies to."""

    THREAD = enum.auto()
    PROCESS = enum.auto()
    GLOBAL = enum.auto()


class FlowEventPhase(enum.Enum):
    """Phase within the control flow lifecycle that a `FlowEvent` applies to."""

    START = enum.auto()
    STEP = enum.auto()
    END = enum.auto()


class Event:
    """Base class for all trace events in a trace model.  Contains fields that
    are common to all trace event types.
    """

    def __init__(
        self,
        category: str,
        name: str,
        start: trace_time.TimePoint,
        pid: int,
        tid: int,
        args: Dict[str, Any] = {},
    ) -> None:
        self.category: str = category
        self.name: str = name
        self.start: trace_time.TimePoint = start
        self.pid: int = pid
        self.tid: int = tid
        # Any extra arguments that the event contains.
        self.args: Dict[str, Any] = args.copy()

    @classmethod
    def from_dict(cls, event_dict: Dict[str, Any]) -> Self:
        category: str = event_dict["cat"]
        name: str = event_dict["name"]
        start: trace_time.TimePoint = trace_time.TimePoint.from_epoch_delta(
            trace_time.TimeDelta.from_microseconds(event_dict["ts"])
        )
        pid: int = event_dict["pid"]
        tid: int = event_dict["tid"]
        args: Dict[str, Any] = event_dict.get("args", {})

        return cls(category, name, start, pid, tid, args)


class InstantEvent(Event):
    """An event that corresponds to a single moment in time."""

    INSTANT_EVENT_SCOPE_MAP: Dict[str, InstantEventScope] = {
        "g": InstantEventScope.GLOBAL,
        "p": InstantEventScope.PROCESS,
        "t": InstantEventScope.THREAD,
    }

    def __init__(self, scope: InstantEventScope, base: Event) -> None:
        super().__init__(
            base.category, base.name, base.start, base.pid, base.tid, base.args
        )
        self.scope: InstantEventScope = scope

    @classmethod
    def from_dict(cls, event_dict: Dict[str, Any]) -> Self:
        scope_key: str = "s"
        if scope_key not in event_dict:
            raise TypeError(
                f"Expected dictionary to have a field '{scope_key}' of type "
                f"str: {event_dict}"
            )
        if not isinstance(event_dict[scope_key], str):
            raise TypeError(
                f"Expected dictionary to have a field '{scope_key}' of type "
                f"str: {event_dict}"
            )
        scope_str: str = event_dict[scope_key]
        if scope_str not in cls.INSTANT_EVENT_SCOPE_MAP:
            raise TypeError(
                f"Expected '{scope_key}' (scope field) of dict to be one of "
                f"{list(cls.INSTANT_EVENT_SCOPE_MAP)}: {event_dict}"
            )
        scope: InstantEventScope = cls.INSTANT_EVENT_SCOPE_MAP[scope_str]

        return cls(scope, base=Event.from_dict(event_dict))


class CounterEvent(Event):
    """An event that tracks the count of some quantity."""

    def __init__(self, id: Optional[int], base: Event) -> None:
        super().__init__(
            base.category, base.name, base.start, base.pid, base.tid, base.args
        )
        self.id: Optional[int] = id

    @classmethod
    def from_dict(cls, event_dict: Dict[str, Any]) -> Self:
        id_key: str = "id"
        id: Optional[int] = None
        if id_key in event_dict:
            try:
                id = int(event_dict[id_key])
            except (TypeError, ValueError) as t:
                raise TypeError(
                    f"Expected '{id_key}' field to be an int or a string that "
                    f"parses as int: {event_dict}"
                ) from t

        return cls(id, base=Event.from_dict(event_dict))


class DurationEvent(Event):
    """An event which describes work that is happening synchronously on one
    thread.

    In the Fuchsia trace model, matching begin/end duration in the raw Chrome
    trace format are merged into a single `DurationEvent`. Chrome complete
    events become `DurationEvent`s as well. Dangling Chrome begin/end events
    (i.e. they don't have a matching end/begin event) are dropped.
    """

    def __init__(
        self,
        duration: Optional[trace_time.TimeDelta],
        parent: Optional[Self],
        child_durations: List[Self],
        child_flows: List["FlowEvent"],
        base: Event,
    ) -> None:
        super().__init__(
            base.category, base.name, base.start, base.pid, base.tid, base.args
        )
        self.duration: Optional[int] = duration
        self.parent: Optional[Self] = parent
        self.child_durations: List[Self] = child_durations
        self.child_flows: List["FlowEvent"] = child_flows

    @classmethod
    def from_dict(cls, event_dict: Dict[str, Any]) -> Self:
        duration_key: str = "dur"
        duration: Optional[trace_time.TimeDelta] = None
        microseconds: Optional[float | int] = event_dict.get(duration_key, None)
        if microseconds is not None:
            if not isinstance(microseconds, (int, float)):
                raise TypeError(
                    f"Expected dictionary to have a field '{duration_key}' of "
                    f"type float or int: {event_dict}"
                )
            duration = trace_time.TimeDelta.from_microseconds(
                float(microseconds)
            )

        return cls(
            duration=duration,
            parent=None,
            child_durations=[],
            child_flows=[],
            base=Event.from_dict(event_dict),
        )


class AsyncEvent(Event):
    """An event which describes work which is happening asynchronously and which
    may span multiple threads.

    Dangling Chrome async begin/end events are dropped.
    """

    def __init__(
        self, id: int, duration: Optional[trace_time.TimeDelta], base: Event
    ) -> None:
        super().__init__(
            base.category, base.name, base.start, base.pid, base.tid, base.args
        )
        self.id: int = id
        self.duration: Optional[trace_time.TimeDelta] = duration

    @classmethod
    def from_dict(cls, id: int, event_dict: Dict[str, Any]) -> Self:
        return cls(id, duration=None, base=Event.from_dict(event_dict))


class FlowEvent(Event):
    """An event which describes control flow handoffs between threads or across
    processes.

    Malformed flow events are dropped.  Malformed flow events could be any of:
      * A begin flow event with a (category, name, id) tuple already in progress
        (this is uncommon in practice).
      * A step flow event with no preceding (category, name, id) tuple.
      * An end flow event with no preceding (category, name, id) tuple.
    """

    FLOW_EVENT_PHASE_MAP: Dict[str, FlowEventPhase] = {
        "s": FlowEventPhase.START,
        "t": FlowEventPhase.STEP,
        "f": FlowEventPhase.END,
    }

    def __init__(
        self,
        id: str,
        phase: FlowEventPhase,
        enclosing_duration: Optional[DurationEvent],
        previous_flow: Optional[Self],
        next_flow: Optional[Self],
        base: Event,
    ) -> None:
        super().__init__(
            base.category, base.name, base.start, base.pid, base.tid, base.args
        )
        self.id: str = id
        self.phase: FlowEventPhase = phase
        self.enclosing_duration: Optional[DurationEvent] = enclosing_duration
        self.previous_flow: Optional[Self] = previous_flow
        self.next_flow: Optional[Self] = next_flow

    @classmethod
    def from_dict(
        cls,
        id: str,
        enclosing_duration: Optional[DurationEvent],
        event_dict: Dict[str, Any],
    ) -> Self:
        phase_key: str = "ph"
        if phase_key not in event_dict:
            raise TypeError(
                f"Expected dictionary to have a field '{phase_key}' of type "
                f"str: {event_dict}"
            )
        if not isinstance(event_dict[phase_key], str):
            raise TypeError(
                f"Expected dictionary to have a field '{phase_key}' of type "
                f"str: {event_dict}"
            )
        phase_str: str = event_dict[phase_key]
        if phase_str not in cls.FLOW_EVENT_PHASE_MAP:
            raise TypeError(
                f"Expected '{phase_key}' (phase field) of dict to be one of "
                f"{list(cls.FLOW_EVENT_PHASE_MAP)}: {event_dict}"
            )
        phase: FlowEventPhase = cls.FLOW_EVENT_PHASE_MAP[phase_str]

        return cls(
            id,
            phase,
            enclosing_duration,
            previous_flow=None,
            next_flow=None,
            base=Event.from_dict(event_dict),
        )


class Thread:
    """A thread within a trace model."""

    def __init__(
        self,
        tid: int,
        name: Optional[str] = None,
        events: Optional[List[Event]] = None,
    ) -> None:
        self.tid: int = tid
        self.name: str = "" if name is None else name
        self.events: List[Event] = [] if events is None else events


class Process:
    """A process within a trace model."""

    def __init__(
        self,
        pid: int,
        name: Optional[str] = None,
        threads: Optional[List[Thread]] = None,
    ) -> None:
        self.pid: int = pid
        self.name: str = "" if name is None else name
        self.threads: List[Thread] = [] if threads is None else threads


class Model:
    """The root of the trace model."""

    def __init__(self) -> None:
        self.processes: List[Process] = []

    def all_events(self) -> Iterator[Event]:
        for process in self.processes:
            for thread in process.threads:
                for event in thread.events:
                    yield event

    def slice(
        self,
        start: Optional[trace_time.TimePoint] = None,
        end: Optional[trace_time.TimePoint] = None,
    ) -> Self:
        """Extract a sub-Model defined by a time interval.

        Args:
            start: Start of the time interval.  If None, the start time of the
                source Model is used.  Only trace events that begin at or after
                `start` will be included in the sub-model.
            end: End of the time interval.  If None, the end time of the source
                Model is used.  Only trace events that end at or before `end`
                will be included in the sub-model.
        """
        result = Model()

        # The various event types have references to other events, which we will
        # need to update so that all the relations in the new model stay within
        # the new model. This dict tracks for each event of the old model, which
        # event in the new model it corresponds to.
        new_event_map = {}

        # Step 1: Populate the model with new event objects. These events will
        # have references into the old model.
        for process in self.processes:
            new_process = Process(process.pid, process.name)
            result.processes.append(new_process)
            for thread in process.threads:
                new_thread = Thread(thread.tid, thread.name)
                new_process.threads.append(new_thread)
                for event in thread.events:
                    # Exclude any event that starts or ends outside of the
                    # specified range.
                    if (start is not None and event.start < start) or (
                        end is not None and event.start > end
                    ):
                        continue

                    # Exclude any event whose duration ends outside of the
                    # specified range.
                    if isinstance(event, (AsyncEvent, DurationEvent)):
                        if (
                            end is not None
                            and event.duration > end - event.start
                        ):
                            continue

                    new_event = copy.copy(event)
                    new_thread.events.append(new_event)
                    new_event.args = event.args.copy()
                    new_event_map[event] = new_event

        # Step 2: Replace all referenced events by their corresponding ones in
        # the new model.
        for process in result.processes:
            for thread in process.threads:
                for event in thread.events:
                    if isinstance(event, DurationEvent):
                        event.parent = new_event_map.get(event.parent, None)
                        event.child_durations = [
                            new_event_map[e] for e in event.child_durations
                        ]
                        event.child_flows = [
                            new_event_map[e] for e in event.child_flows
                        ]
                    elif isinstance(event, FlowEvent):
                        event.enclosing_duration = new_event_map.get(
                            event.enclosing_duration, None
                        )
                        event.previous_flow = new_event_map.get(
                            event.previous_flow, None
                        )
                        event.next_flow = new_event_map.get(
                            event.next_flow, None
                        )

        return result
