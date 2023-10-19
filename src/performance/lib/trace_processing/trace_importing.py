# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Logic to deserialize a trace Model from JSON."""

import json
from collections import defaultdict
from typing import Any, Dict, List, Optional, Self, TextIO, Tuple

import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time


class _FlowKey:
    """A helper struct to group flow events."""

    def __init__(
        self, category: Optional[str], name: Optional[str], pid: int, id: int
    ) -> None:
        self.category: Optional[str] = category
        self.name: Optional[str] = name
        self.pid: int = pid  # Only used for 'local' flow ids.
        self.id: int = id

    @classmethod
    def from_trace_event(cls, trace_event: trace_model.Event) -> Self:
        category: Optional[str] = trace_event.get("cat")
        name: Optional[str] = trace_event.get("name")
        # _FlowKey is globally scoped unless specifically local.
        pid: int = 0

        # Helper to convert an object into a string.
        def as_string_id(obj: object):
            if isinstance(obj, str):
                return obj
            elif isinstance(obj, int):
                return str(obj)
            elif isinstance(obj, float):
                if obj.isNaN():
                    raise TypeError("Got NaN double for id field value")
                elif obj % 1.0 != 0.0:
                    raise TypeError(
                        f"Got float with non-zero decimal place ({obj}) for id "
                        f"field value"
                    )
                else:
                    return str(int(obj))
            else:
                raise TypeError(
                    f"Got unexpected type {obj.__class__.__name__} for id "
                    f"field value: {obj}"
                )

        id = None
        if "id" in trace_event:
            id = as_string_id(trace_event["id"])
        elif "id2" in trace_event:
            id2 = trace_event["id2"]
            if "local" in id2:
                pid = trace_event["pid"]
                id = as_string_id(id2["local"])
            elif "global" in id2:
                id = as_string_id(id2["global"])
        if id is None:
            raise Exception(f"Could not find id in {trace_event}")

        return cls(category, name, pid, id)

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, _FlowKey):
            return False

        return (
            self.category == other.category
            and self.name == other.name
            and self.id == other.id
            and self.pid == other.pid
        )

    def __hash__(self) -> int:
        result = 17
        result = 37 * result + hash(self.category)
        result = 37 * result + hash(self.name)
        result = 37 * result + hash(self.id)
        result = 37 * result + hash(self.pid)
        return result


class _AsyncKey:
    """A helper struct to group async events."""

    def __init__(
        self, category: Optional[str], name: Optional[str], pid: int, id: int
    ) -> None:
        self.category: Optional[str] = category
        self.name: Optional[str] = name
        self.pid: int = pid
        self.id: int = id

    @classmethod
    def from_trace_event(cls, trace_event) -> Self:
        category: Optional[str] = trace_event.get("cat", None)
        name: Optional[str] = trace_event.get("name", None)
        pid: int = trace_event["pid"]

        # Helper to parse an object into an int, returning None if the object is
        # not parseable.
        def try_parse_int(s: object):
            try:
                return int(s, 0)  # 0 base allows guessing hex, binary, etc
            except (TypeError, ValueError):
                return None

        id: Optional[int] = None
        if "id" in trace_event:
            if isinstance(trace_event["id"], int):
                id = trace_event["id"]
            elif isinstance(trace_event["id"], str):
                id = try_parse_int(trace_event["id"])
        elif "id2" in trace_event:
            id2 = trace_event["id2"]
            if "local" in id2:
                # 'local' id2 means scoped to the process.
                if isinstance(id2["local"], int):
                    id = id2["local"]
                elif isinstance(id2["local"], str):
                    id = try_parse_int(id2["local"])
            elif "global" in id2:
                pid = 0
                if isinstance(id2["global"], int):
                    id = id2["global"]
                elif isinstance(id2["global"], str):
                    id = try_parse_int(id2["global"])
        if id is None:
            raise Exception(f"Could not find id in {trace_event}")

        return cls(category, name, pid, id)

    def __eq__(self, other) -> bool:
        if not isinstance(other, _AsyncKey):
            return False

        return (
            self.pid == other.pid
            and self.category == other.category
            and self.name == other.name
            and self.id == other.id
        )

    def __hash__(self) -> int:
        result = 17
        result = 37 * result + hash(self.pid)
        result = 37 * result + hash(self.category)
        result = 37 * result + hash(self.name)
        result = 37 * result + hash(self.id)
        return result


def create_model_from_file_path(path: str) -> trace_model.Model:
    """Create a Model from a file path.

    Args:
        path: The path to the file.

    Returns:
        A Model object.
    """

    with open(path, "r") as file:
        return create_model_from_file(file)


def create_model_from_file(file: TextIO) -> trace_model.Model:
    """Create a Model from a file.

    Args:
        file: The file to read.

    Returns:
        A Model object.
    """

    return create_model_from_json(json.load(file))


def create_model_from_string(json_string: str) -> trace_model.Model:
    """Create a Model from a raw JSON string of trace data.

    Args:
        json_string: The JSON string to parse.

    Returns:
        A Model object.
    """

    json_object: Dict[str, Any] = json.loads(json_string)
    return create_model_from_json(json_object)


def create_model_from_json(root_object: Dict[str, Any]) -> trace_model.Model:
    """Creates a Model from a JSON dictionary.

    Args:
        root_object: A JSON dictionary representing the trace data.

    Returns:
        A Model object.
    """

    # A helper lambda to assert that expected fields in a JSON trace event are
    # present and are of the correct type.  If any of these fields are missing
    # or is of a different type than what is asserted here, then the JSON trace
    # event is considered to be malformed.
    def check_trace_event(json_trace_event: Dict[str, Any]):
        if not (
            "ph" in json_trace_event and isinstance(json_trace_event["ph"], str)
        ):
            raise TypeError(
                f"Expected {json_trace_event} to have field 'ph' of type str"
            )
        if json_trace_event["ph"] != "M" and not (
            "cat" in json_trace_event
            and isinstance(json_trace_event["cat"], str)
        ):
            raise TypeError(
                f"Expected {json_trace_event} to have field 'cat' of type str"
            )
        if not (
            "name" in json_trace_event
            and isinstance(json_trace_event["name"], str)
        ):
            raise TypeError(
                f"Expected {json_trace_event} to have field 'name' of type str"
            )
        if json_trace_event["ph"] != "M" and not (
            "ts" in json_trace_event
            and isinstance(json_trace_event["ts"], (float, int))
        ):
            raise TypeError(
                f"Expected {json_trace_event} to have field 'ts' of type float "
                f"or int"
            )
        if not (
            "pid" in json_trace_event
            and isinstance(json_trace_event["pid"], int)
        ):
            raise TypeError(
                f"Expected {json_trace_event} to have field 'pid' of type int"
            )
        if not (
            "tid" in json_trace_event
            and isinstance(json_trace_event["tid"], (float, int))
        ):
            raise TypeError(
                f"Expected {json_trace_event} to have field 'tid' of type "
                f"float or int"
            )
        if "args" in json_trace_event:
            if not isinstance(json_trace_event["args"], dict):
                raise TypeError(
                    f"Expected {json_trace_event} with 'args' field to have "
                    f"'args' field of type dict"
                )

    # A helper lambda to add duration events to the appropriate duration stack
    # and do the appropriate duration/flow graph setup.  It is used for both
    # begin/end pairs and complete events.
    def add_to_duration_stack(
        duration_event: trace_model.DurationEvent,
        duration_stack: List[trace_model.DurationEvent],
    ):
        duration_stack.append(duration_event)
        if len(duration_stack) > 1:
            top = duration_stack[-1]
            top_parent = duration_stack[-2]
            top.parent = top_parent
            top_parent.child_durations.append(duration_event)

    # Obtain the overall list of trace events.
    if not (
        "traceEvents" in root_object
        and isinstance(root_object["traceEvents"], list)
    ):
        raise TypeError(
            f"Expected {root_object} to have field 'traceEvents' of type List"
        )
    trace_events: List[Dict[str, Any]] = root_object["traceEvents"].copy()

    # Add synthetic end events for each complete event in the trace data to
    # assist with maintaining each thread's duration stack.  This isn't strictly
    # necessary, however it makes the duration stack bookkeeping simpler.
    for trace_event in root_object["traceEvents"]:
        if trace_event["ph"] == "X":
            synthetic_end_event: Dict[str, Any] = trace_event.copy()
            synthetic_end_event["ph"] = "fuchsia_synthetic_end"
            synthetic_end_event["ts"] = trace_event["ts"] + trace_event["dur"]
            trace_events.append(synthetic_end_event)

    # Sort the events by their timestamp.  We need to iterate through the events
    # in sorted order to compute things such as duration stacks and flow
    # sequences. Events without timestamps (e.g. Chrome's metadata events) are
    # sorted to the beginning.
    #
    # We need to use a stable sort here, which fortunately `list.sort` is. If we
    # use a non-stable sort, zero-length duration events of type ph='X' are not
    # handled properly, because the 'fuchsia_synthetic_end' events can get
    # sorted before their corresponding beginning events.
    trace_events.sort(key=lambda x: x.get("ts", 0))

    # Maintains the current duration stack for each track.
    duration_stacks: Dict[
        Tuple[int, int], List[trace_model.DurationEvent]
    ] = defaultdict(list)
    # Maintains in progress async events.
    live_async_events: Dict[_AsyncKey, trace_model.AsyncEvent] = {}
    # Maintains in progress flow sequences.
    live_flows: Dict[_FlowKey, trace_model.FlowEvent] = {}
    # Flows with "next slide" binding that are waiting to be bound.
    unbound_flow_events: Dict[
        Tuple[int, int], List[trace_model.FlowEvent]
    ] = defaultdict(list)
    # Final list of events to be written into the Model.
    result_events: List[trace_model.Event] = []

    dropped_flow_event_counter: int = 0
    dropped_async_event_counter: int = 0
    # TODO(fxbug.dev/41309): Support nested async events.  In the meantime, just
    # drop them.
    dropped_nested_async_event_counter: int = 0

    pid_to_name: Dict[Optional[int], str] = {}
    tid_to_name: Dict[Optional[int], str] = {}

    # Create result events from trace events.
    for trace_event in trace_events:
        # Guarantees trace event will have certain fields present, so they are
        # not checked below.
        check_trace_event(trace_event)

        phase: str = trace_event["ph"]
        pid: int = trace_event["pid"]
        tid: int = int(trace_event["tid"])
        track_key: Tuple[int, int] = (pid, tid)
        duration_stack: List[trace_model.DurationEvent] = duration_stacks[
            track_key
        ]

        if phase in ("X", "B"):
            duration_event: trace_model.DurationEvent = (
                trace_model.DurationEvent.from_dict(trace_event)
            )
            if track_key in unbound_flow_events:
                for flow_event in unbound_flow_events[track_key]:
                    flow_event.enclosing_duration = duration_event
                unbound_flow_events[track_key].clear()
            add_to_duration_stack(duration_event, duration_stack)
            if phase == "X":
                result_events.append(duration_event)
        elif phase == "E":
            if duration_stack:
                popped: trace_model.DurationEvent = duration_stack.pop()
                popped.duration = (
                    trace_time.TimePoint.from_epoch_delta(
                        trace_time.TimeDelta.from_microseconds(
                            trace_event["ts"]
                        )
                    )
                    - popped.start
                )
                if "args" in trace_event:
                    popped.args = {**popped.args, **trace_event["args"]}
                result_events.append(popped)
        elif phase == "fuchsia_synthetic_end":
            assert duration_stack
            duration_stack.pop()
        elif phase == "b":
            async_key: _AsyncKey = _AsyncKey.from_trace_event(trace_event)
            async_event: trace_model.AsyncEvent = (
                trace_model.AsyncEvent.from_dict(async_key.id, trace_event)
            )
            live_async_events[async_key] = async_event
        elif phase == "e":
            async_key: _AsyncKey = _AsyncKey.from_trace_event(trace_event)
            async_event: Optional[
                trace_model.AsyncEvent
            ] = live_async_events.pop(async_key, None)
            if async_event is not None:
                async_event.duration = (
                    trace_time.TimePoint.from_epoch_delta(
                        trace_time.TimeDelta.from_microseconds(
                            trace_event["ts"]
                        )
                    )
                    - async_event.start
                )
                if "args" in trace_event:
                    async_event.args = {
                        **async_event.args,
                        **trace_event["args"],
                    }
            else:
                dropped_async_event_counter += 1
                continue
            result_events.append(async_event)
        elif phase == "i" or phase == "I":
            instant_event: trace_model.InstantEvent = (
                trace_model.InstantEvent.from_dict(trace_event)
            )
            result_events.append(instant_event)
        elif phase == "s" or phase == "t" or phase == "f":
            binding_point: Optional[str] = None
            if "bp" in trace_event:
                if trace_event["bp"] == "e":
                    binding_point = "enclosing"
                else:
                    raise TypeError(
                        f"Found unexpected value in bp field of {trace_event}"
                    )
            elif phase == "s" or phase == "t":
                binding_point = "enclosing"
            elif phase == "f":
                binding_point = "next"

            flow_key: _FlowKey = _FlowKey.from_trace_event(trace_event)
            previous_flow: Optional[trace_model.FlowEvent] = None
            if phase == "s":
                if flow_key in live_flows:
                    dropped_flow_event_counter += 1
                    continue
            elif phase == "t" or phase == "f":
                previous_flow = live_flows.get(flow_key, None)
                if previous_flow is None:
                    dropped_flow_event_counter += 1
                    continue

            if not duration_stack:
                dropped_flow_event_counter += 1
                continue

            enclosing_duration: Optional[trace_model.DurationEvent] = (
                duration_stack[-1] if binding_point == "enclosing" else None
            )
            flow_event: trace_model.FlowEvent = trace_model.FlowEvent.from_dict(
                flow_key.id, enclosing_duration, trace_event
            )
            if binding_point == "enclosing":
                enclosing_duration.child_flows.append(flow_event)
            else:
                unbound_flow_events[track_key].append(flow_event)

            if previous_flow is not None:
                previous_flow.next_flow = flow_event
            flow_event.previous_flow = previous_flow

            if phase == "s" or phase == "t":
                live_flows[flow_key] = flow_event
            else:
                live_flows.pop(flow_key)
            result_events.append(flow_event)
        elif phase == "C":
            counter_event: trace_model.CounterEvent = (
                trace_model.CounterEvent.from_dict(trace_event)
            )
            result_events.append(counter_event)
        elif phase == "n":
            # TODO(fxbug.dev/41309): Support nested async events.  In the
            # meantime, just drop them.
            dropped_nested_async_event_counter += 1
        elif phase == "M":
            # Chrome metadata events. These define process and thread names,
            # similar to the Fuchsia systemTraceEvents.
            if trace_event["name"] == "process_name":
                # If trace_event contains args, those are verified to be of type
                # dict in check_trace_event.
                if (
                    "args" not in trace_event
                    or "name" not in trace_event["args"]
                ):
                    raise TypeError(
                        f"{trace_event} is a process_name metadata event but "
                        f"doesn't have a name argument"
                    )
                pid_to_name[pid] = trace_event["args"]["name"]
            if trace_event["name"] == "thread_name":
                # If trace_event contains args, those are verified to be of type
                # dict in check_trace_event.
                if (
                    "args" not in trace_event
                    or "name" not in trace_event["args"]
                ):
                    raise TypeError(
                        f"{trace_event} is a thread_name metadata event but "
                        f"doesn't have a name argument"
                    )
                tid_to_name[tid] = trace_event["args"]["name"]
        elif phase in ("R", "(", ")", "O", "N", "D", "S", "T", "p", "F"):
            # Ignore some phases that are in Chrome traces that we don't yet
            # have use cases for.
            #
            # These are:
            # * 'R' - Mark events, similar to instants created by the Navigation
            #         Timing API
            # * '(', ')' - Context events
            # * 'O', 'N', 'D' - Object events
            # * 'S', 'T', 'p', 'F' - Legacy async events
            pass
        else:
            raise TypeError(
                f"Encountered unknown phase {phase} from {trace_event}"
            )

    # Sort events by their start timestamp.
    #
    # We need a stable sort here, which fortunately `list.sort` is.  This is
    # required to preserve the ordering of events when they share the same start
    # timestamp. Such events are more likely to occur on systems with a low
    # timer resolution.
    result_events.sort(key=lambda x: x.start)

    # Print warnings about anomalous conditions in the trace.
    live_duration_events_count = sum(len(ds) for ds in duration_stacks.values())
    if live_duration_events_count > 0:
        print(
            f"Warning, finished processing trace events with "
            f"{live_duration_events_count} in progress duration events"
        )
    if live_async_events:
        print(
            f"Warning, finished processing trace events with "
            f"{len(live_async_events)} in progress async events"
        )
    if live_flows:
        print(
            f"Warning, finished processing trace events with {len(live_flows)} "
            f"in progress flow events"
        )
    if dropped_async_event_counter > 0:
        print(f"Warning, dropped {dropped_async_event_counter} async events")
    if dropped_flow_event_counter > 0:
        print(f"Warning, dropped {dropped_flow_event_counter} flow events")
    if dropped_nested_async_event_counter > 0:
        print(
            f"Warning, dropped {dropped_nested_async_event_counter} nested "
            f"async events"
        )

    # Process system trace events.
    if "systemTraceEvents" in root_object:
        if not isinstance(root_object["systemTraceEvents"], dict):
            raise TypeError(
                "Expected field 'systemTraceEvents' to be of type dict"
            )

        system_trace_events_list = root_object["systemTraceEvents"]
        if not (
            "type" in system_trace_events_list
            and isinstance(system_trace_events_list["type"], str)
        ):
            raise TypeError(
                f"Expected {system_trace_events_list} to have field 'type' of "
                f"type str"
            )
        if not system_trace_events_list["type"] == "fuchsia":
            raise TypeError(
                f"Expected {system_trace_events_list} to have field 'type' "
                f"equal to value 'fuchsia'"
            )
        if not (
            "events" in system_trace_events_list
            and isinstance(system_trace_events_list["events"], list)
        ):
            raise TypeError(
                f"Expected {system_trace_events_list} to have field 'events' "
                f"of type list"
            )

        system_trace_events = system_trace_events_list["events"]
        for system_trace_event in system_trace_events:
            if not (
                "ph" in system_trace_event
                and isinstance(system_trace_event["ph"], str)
            ):
                raise TypeError(
                    f"Expected {system_trace_event} to have field 'ph' of type "
                    f"String"
                )

            phase: str = system_trace_event["ph"]
            if phase == "p":
                if not (
                    "pid" in system_trace_event
                    and isinstance(system_trace_event["pid"], int)
                ):
                    raise TypeError(
                        f"Expected {system_trace_event} to have field 'pid' "
                        f"of type int"
                    )
                if not (
                    "name" in system_trace_event
                    and isinstance(system_trace_event["name"], str)
                ):
                    raise TypeError(
                        f"Expected {system_trace_event} to have field 'name' "
                        f"of type str"
                    )

                pid = system_trace_event["pid"]
                name = system_trace_event["name"]
                pid_to_name[pid] = name
            elif phase == "t":
                if not (
                    "pid" in system_trace_event
                    and isinstance(system_trace_event["pid"], int)
                ):
                    raise TypeError(
                        f"Expected {system_trace_event} to have field 'pid' "
                        f"of type int"
                    )
                if not (
                    "name" in system_trace_event
                    and isinstance(system_trace_event["name"], str)
                ):
                    raise TypeError(
                        f"Expected {system_trace_event} to have field 'name' "
                        f"of type str"
                    )
                if not (
                    "tid" in system_trace_event
                    and isinstance(system_trace_event["tid"], (float, int))
                ):
                    raise TypeError(
                        f"Expected {system_trace_event} to have field 'tid' of "
                        f"type int or float"
                    )

                tid = int(system_trace_event["tid"])
                name = system_trace_event["name"]
                tid_to_name[tid] = name
            elif phase == "k" or phase == "w":
                # CPU events are currently ignored.  It would be interesting to
                # support these in the future so we can track CPU durations in
                # addition to wall durations.
                pass
            else:
                print(f"Unknown phase {phase} from {system_trace_event}")

    # Construct the map of Processes.
    processes: Dict[int, trace_model.Process] = {}
    for event in result_events:
        if event.pid not in processes:
            processes[event.pid] = trace_model.Process(pid=event.pid)

        process: trace_model.Process = processes[event.pid]
        threads: List[trace_model.Thread] = [
            thread for thread in process.threads if thread.tid == event.tid
        ]

        assert len(threads) <= 1
        if len(threads) == 0:
            thread: trace_model.Thread = trace_model.Thread(tid=event.tid)
            if event.tid in tid_to_name:
                thread.name = tid_to_name[event.tid]
            process.threads.append(thread)
            threads.append(thread)
        threads[0].events.append(event)

    # Construct the final Model.
    model = trace_model.Model()
    for pid, process in sorted(processes.items()):
        process.threads.sort(key=lambda thread: thread.tid)
        if process.pid in pid_to_name:
            process.name = pid_to_name[process.pid]
        model.processes.append(process)

    return model
