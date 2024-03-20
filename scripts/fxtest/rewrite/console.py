# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
from collections import defaultdict
from dataclasses import dataclass
import datetime
from itertools import chain
import os
import time
import typing

import args
import event
import statusinfo
import termout


@dataclass
class DurationInfo:
    """Tracks an individual event duration.

    Events with started=True denote the beginning of a duration and must be
    matched by an event with ending=True and the same Id.

    Instances of this class track durations over time for display purposes.
    """

    # A formatted name for the duration, used as a label.
    name: str

    # The start monotonic time.
    start_monotonic: float

    # The optional parent of the duration.
    parent: event.Id | None

    # The number of expected children. If set, we can show a progress bar.
    expected_child_tasks: int = 0

    # If True, hide children of this duration from the display.
    hide_children: bool = False


class ConsoleState:
    """Holder for all console output state.

    Attributes:
        root_path: The root Fuchsia directory. Starts empty until an
            event containing it is processed.
        active_durations: Map from Id to DurationInfo for durations
            that have not yet ended.
        complete_durations: Map from Id to DurationInfo for durations
            that have ended.
        end_duration: The elapsed time for the entire run, measured
            as the difference between the start and end of GLOBAL_RUN_ID.
            Only set after the global run has ended.
        test_results: Map from status to a set of tests with that
            status. This is the canonical result list for all tests.
    """

    def __init__(self) -> None:
        self.root_path: str | None = None
        self.active_durations: dict[event.Id, DurationInfo] = dict()
        self.complete_durations: dict[event.Id, DurationInfo] = dict()
        self.end_duration: float | None = None
        self.test_results: dict[
            event.TestSuiteStatus, typing.Set[str]
        ] = defaultdict(set)


async def console_printer(
    recorder: event.EventRecorder,
    flags: args.Flags,
    do_status_output_event: asyncio.Event,
) -> None:
    """Asynchronous future that implements console printing.

    Continually reads events from the given recorder and presents status
    updates to the terminal. This is the main output routine for fx test.

    Output is controlled by the given flags.

    This routine implements continual clearing and updating of a status
    bar at the bottom of the user's terminal. This behavior is controlled
    by the `do_status_output_event` asyncio.Event, which is set only when
    continual status output is both desired and available.

    Args:
        recorder (event.EventRecorder): Source of events to display.
        flags (args.Flags): Command line flags to control formatting.
        do_status_output_event (asyncio.Event): Display updating
            status bar only if this is set.
    """

    print("")  # To align with the original tool's output.

    state = ConsoleState()
    print_queue: asyncio.Queue[list[str]] = asyncio.Queue()

    # Spawn an asynchronous task to actually process incoming events.
    # The rest of this method simply displays the status output and prints
    # lines that are requested by the other task.
    event_loop = asyncio.create_task(
        _console_event_loop(recorder, flags, state, print_queue)
    )

    # Keep pumping events until there will be no more.
    while not event_loop.done() or not print_queue.empty():
        # First try to get some lines that need to be printed.
        # If there is nothing to print by the time we need to refresh, timeout
        # and refresh the output.
        try:
            lines_to_print = await asyncio.wait_for(
                print_queue.get(), flags.status_delay
            )
        except asyncio.TimeoutError:
            lines_to_print = []

        if do_status_output_event.is_set():
            status_lines = _create_status_lines_from_state(flags, state)

            # Print status output, leaving an extra line to separate
            # from prepended lines.
            termout.write_lines(
                [""] + status_lines[: flags.status_lines], lines_to_print
            )
        elif lines_to_print:
            print("\n".join(lines_to_print))

    # We are done with all events, clean up and exit.

    if do_status_output_event.is_set():
        # Clear status output.
        termout.write_lines([], [])

    if state.test_results:
        passed = len(state.test_results[event.TestSuiteStatus.PASSED])
        failed = len(state.test_results[event.TestSuiteStatus.FAILED]) + len(
            state.test_results[event.TestSuiteStatus.TIMEOUT]
        )
        skipped = len(state.test_results[event.TestSuiteStatus.SKIPPED])
        passed_text = pass_format(passed, flags.style)
        failed_text = fail_format(failed, flags.style)
        skipped_text = skip_format(skipped, flags.style)

        print(f"Results: {passed_text} {failed_text} {skipped_text}")

    print(
        statusinfo.dim(
            f"\nCompleted in {state.end_duration:.3f}s [{len(state.complete_durations)}/{len(state.complete_durations)} complete]",
            style=flags.style,
        )
    )

    if state.active_durations:
        print(
            statusinfo.error_highlight(
                "BUG: Durations still active at exit:", style=flags.style
            )
        )
        for id, duration in state.active_durations.items():
            print(f" {id} = {duration.__dict__}")

    await event_loop


@dataclass
class DurationPrintInfo:
    """Wrap information needed to print the status of a task duration."""

    # The DurationInfo we will print.
    info: DurationInfo

    # How far indented the duration should be.
    indent: int

    # If set, display a progress bar with this percent completion.
    progress: float | None = None


@dataclass
class TaskStatus:
    """Overall status of all tasks, for printing."""

    # Number of tasks currently running.
    tasks_running: int

    # Number of tasks that have completed
    tasks_complete: int

    # Number of tasks that are queued but have not started running yet.
    tasks_queued_but_not_running: int

    # Detailed information to print a status line for each in-progress duration.
    duration_infos: list[DurationPrintInfo]

    def total_tasks(self) -> int:
        return (
            self.tasks_running
            + self.tasks_complete
            + self.tasks_queued_but_not_running
        )


def _create_status_lines_from_state(
    flags: args.Flags, state: ConsoleState
) -> list[str]:
    """Process the overall console state into a list of lines to present to the user.

    Args:
        flags (args.Flags): Flags controlling output format.
        state (ConsoleState): The console state to process.

    Returns:
        list[str]: List of lines to present to the user.
    """

    # Process the state
    task_status = _produce_task_status_from_state(state)

    # Current time for duration displays.
    monotonic = time.monotonic()

    # Format the computed data as lines to print out.
    status_lines = _format_duration_lines(flags, task_status)

    # Show an overall duration timer if the global run is started.
    if event.GLOBAL_RUN_ID in state.active_durations:
        run_duration = f"[duration: {statusinfo.format_duration(datetime.timedelta(seconds=monotonic - state.active_durations[event.GLOBAL_RUN_ID].start_monotonic).total_seconds())}]"
    else:
        run_duration = ""

    # Show pass/fail counts if tests have started completing.
    pass_fail = ""
    if state.test_results:
        passed = len(state.test_results[event.TestSuiteStatus.PASSED])
        failed = len(state.test_results[event.TestSuiteStatus.FAILED]) + len(
            state.test_results[event.TestSuiteStatus.TIMEOUT]
        )
        skipped = len(state.test_results[event.TestSuiteStatus.SKIPPED])
        passed_text = pass_format(passed, flags.style)
        failed_text = fail_format(failed, flags.style)
        skipped_text = skip_format(skipped, flags.style)

        pass_fail = (
            statusinfo.dim(" [tests: ", style=flags.style)
            + f"{passed_text} {failed_text} {skipped_text}"
            + statusinfo.dim("] ", style=flags.style)
        )

    tasks_status = statusinfo.dim(
        f"  [tasks: {task_status.tasks_running} running, {task_status.tasks_complete}/{task_status.total_tasks()} complete]",
        style=flags.style,
    )

    # Print out the duration lines if they are present.
    if status_lines:
        status_lines = [
            statusinfo.green("Status: ", style=flags.style)
            + statusinfo.dim(f"{run_duration}", style=flags.style)
            + (tasks_status if not pass_fail else pass_fail)
        ] + status_lines

    return status_lines


def _produce_task_status_from_state(state: ConsoleState) -> TaskStatus:
    # Generate a mapping of each duration to its children.
    duration_children: dict[event.Id, list[event.Id]] = defaultdict(list)
    all_durations: dict[event.Id, DurationInfo] = dict()

    for id, duration in chain(
        state.active_durations.items(), state.complete_durations.items()
    ):
        if id != event.GLOBAL_RUN_ID:
            duration_children[duration.parent or event.GLOBAL_RUN_ID].append(id)
        all_durations[id] = duration

    # Calculate counts of how many tasks are in what state.
    tasks_running = len(state.active_durations)
    tasks_complete = len(state.complete_durations)
    tasks_queued_but_not_running = 0
    for id, children in duration_children.items():
        expected = all_durations[id].expected_child_tasks
        if expected and expected >= len(children):
            tasks_queued_but_not_running += expected - len(children)

    # Process the active durations into DurationPrintInfo, which
    # contains information on how to print the duration state.
    # We perform an in-order tree traversal over all durations
    # starting from the root, taking account only of those
    # durations that are active and sorting by descending
    # timestamp.
    duration_print_infos: list[DurationPrintInfo] = []
    assert event.GLOBAL_RUN_ID in all_durations

    # Stack of duration event.Ids to process. Second
    # element of the tuple tracks indent level.
    work_stack: list[tuple[event.Id, int]] = [(event.GLOBAL_RUN_ID, 0)]
    while work_stack:
        id, indent = work_stack.pop()
        info: DurationInfo | None = None

        if id == event.GLOBAL_RUN_ID:
            pass
        elif id not in state.active_durations:
            continue
        else:
            progress = None
            info = state.active_durations[id]
            if info.expected_child_tasks:
                progress = min(
                    1.0,
                    sum(
                        [
                            1 if child_id in state.complete_durations else 0
                            for child_id in duration_children.get(id, [])
                        ]
                    )
                    / info.expected_child_tasks,
                )
            duration_print_infos.append(
                DurationPrintInfo(info, indent, progress)
            )

        if info is not None and info.hide_children:
            # Skip processing children of this duration for display.
            continue

        for child_id in duration_children.get(id, []):
            children = []
            if child_id in state.active_durations:
                children.append(child_id)
            # Put children in the work stack in ascending
            # order, so that they will be popped in descending
            # order.
            children.sort(key=lambda x: all_durations[x].start_monotonic)
            work_stack.extend([(child_id, indent + 1) for child_id in children])

    return TaskStatus(
        tasks_running=tasks_running,
        tasks_complete=tasks_complete,
        tasks_queued_but_not_running=tasks_queued_but_not_running,
        duration_infos=duration_print_infos,
    )


def _format_duration_lines(flags: args.Flags, status: TaskStatus) -> list[str]:
    """Given the processed status for all tasks, format output based
    on the flags.

    Args:
        flags (args.Flags): Flags to control output format.
        status (TaskStatus): Processed task status.

    Returns:
        list[str]: A list of lines to present to the user.
    """
    monotonic = time.monotonic()
    duration_lines: list[str] = []
    for print_info in status.duration_infos:
        prefix = " " * (print_info.indent * 2)
        if print_info.progress is not None:
            duration_lines.append(
                statusinfo.status_progress(
                    prefix + print_info.info.name,
                    print_info.progress,
                    style=flags.style,
                )
            )
        else:
            duration_lines.append(
                statusinfo.duration_progress(
                    prefix + print_info.info.name,
                    datetime.timedelta(
                        seconds=monotonic - print_info.info.start_monotonic
                    ),
                    style=flags.style,
                )
            )
    return duration_lines


class TestExecutionInfo:
    """Track and record a single test suite's execution."""

    def __init__(self, name: str):
        """Initialize execution info for a named suite.

        Args:
            name (str): The test suite's name.
        """
        self.name: str = name
        self.buffered_lines: list[str] = []
        self.buffered_output_task: asyncio.Task[None] | None = None

    def spawn_buffered_output_printer(
        self, timeout: float, queue: asyncio.Queue[list[str]]
    ) -> None:
        """Spawn an async task that will print buffered lines after a timeout.

        Args:
            timeout (float): Seconds to wait before printing buffered data.
            queue (asyncio.Queue[list[str]]): Queue to print the
                buffered data to upon timeout.
        """

        async def output_printer_task() -> None:
            """Task that handles sleeping and then sending queued lines.

            May be asynchronously canceled.
            """
            await asyncio.sleep(timeout)
            await queue.put(self.buffered_lines)
            self.buffered_lines = []  # drop to release memory

        self.buffered_output_task = asyncio.create_task(output_printer_task())

    def print_verbatim(self) -> bool:
        """Determine if an output printer should skip buffering and just print.

        Returns:
            bool: True if output should be printed verbatim, False otherwise.
        """
        return (task := self.buffered_output_task) is not None and task.done()

    def should_buffer_output(self) -> bool:
        """Determine if output should be buffered.

        Returns:
            bool: True if an output printer should buffer lines in
                this object, False otherwise.
        """
        return (
            task := self.buffered_output_task
        ) is not None and not task.done()

    def cleanup(self) -> None:
        """Clean up any buffers, cancel, print tasks, and drop any buffered output."""
        if (
            self.buffered_output_task is not None
            and not self.buffered_output_task.done()
        ):
            self.buffered_output_task.cancel()
            self.buffered_lines = []


async def _console_event_loop(
    recorder: event.EventRecorder,
    flags: args.Flags,
    state: ConsoleState,
    print_queue: asyncio.Queue[list[str]],
) -> None:
    """Internal event processor.

    This task processes the events generated by the given EventRecorder and
    updates the given ConsoleState based on their contents. It may also
    request that some lines be printed for the user to see.

    Args:
        recorder (event.EventRecorder): Source of events to process.
        flags (args.Flags): Command line flags for this invocation.
        state (ConsoleState): Shared state object to update over time.
        print_queue (asyncio.Queue): Queue for lines to print to the user.
    """

    # Keep track of ids corresponding to test suites for display purposes:
    # 1. We need the name to report success or failure.
    # 2. We flatten the status display so that commands run as
    #    part of a test execution are not shown.
    # 3. We can buffer output from those programs for later display using
    #    the --slow flag.
    test_suite_execution_info: dict[event.Id, TestExecutionInfo] = dict()

    # Keep track of task IDs that are nested under a test suite.
    # This is needed to map buffered program output to the correct test suite.
    event_id_to_test_suite: dict[event.Id, event.Id] = dict()
    next_event: event.Event
    async for next_event in recorder.iter():
        lines_to_print: list[str] = []

        # If set, and we do verbose printing, append this suffix to the output.
        verbose_suffix: str = ""

        old_duration: DurationInfo | None = None
        if (
            next_event.ending
            and next_event.id is not None
            and next_event.id in state.active_durations
        ):
            old_duration = state.active_durations.pop(next_event.id)
            state.complete_durations[next_event.id] = old_duration
            elapsed_time = next_event.timestamp - old_duration.start_monotonic
            verbose_suffix = (
                f" [duration={datetime.timedelta(seconds=elapsed_time)}]"
            )
            if next_event.id == event.GLOBAL_RUN_ID:
                state.end_duration = elapsed_time

        if flags.verbose:
            # In verbose mode, refuse to print too many output characters
            # to avoid scrolling info out of view.
            lines_to_print.append(
                statusinfo.ellipsize(recorder.event_string(next_event), 400)
                + verbose_suffix
            )

        if next_event.payload:
            if (
                next_event.id is not None
                and next_event.starting
                and next_event.parent not in test_suite_execution_info
            ):
                # Provide nice formatting for event types that need to be tracked for a duration.

                if next_event.id == event.GLOBAL_RUN_ID:
                    state.active_durations[next_event.id] = DurationInfo(
                        "fx test",
                        next_event.timestamp,
                        parent=next_event.parent,
                    )
                elif next_event.payload.parsing_file is not None:
                    styled_name = statusinfo.highlight(
                        "parsing", style=flags.style
                    )
                    state.active_durations[next_event.id] = DurationInfo(
                        f"{styled_name} {next_event.payload.parsing_file.name}",
                        next_event.timestamp,
                        parent=next_event.parent,
                    )
                elif next_event.payload.program_execution is not None:
                    styled_name = statusinfo.highlight(
                        "running", style=flags.style
                    )
                    state.active_durations[next_event.id] = DurationInfo(
                        f"{styled_name} {next_event.payload.program_execution.to_formatted_command_line()}",
                        next_event.timestamp,
                        parent=next_event.parent,
                    )
                elif (
                    next_event.payload.event_group is not None
                    or next_event.payload.test_group is not None
                ):
                    group: event.EventGroupPayload = next_event.payload.event_group or next_event.payload.test_group  # type: ignore
                    styled_name = statusinfo.highlight(
                        group.name, style=flags.style
                    )
                    state.active_durations[next_event.id] = DurationInfo(
                        styled_name,
                        next_event.timestamp,
                        parent=next_event.parent,
                        expected_child_tasks=group.queued_events or 0,
                        hide_children=group.hide_children,
                    )
                elif next_event.payload.build_targets:
                    styled_name = statusinfo.highlight(
                        f"Refreshing {len(next_event.payload.build_targets)} targets",
                        style=flags.style,
                    )
                    state.active_durations[next_event.id] = DurationInfo(
                        styled_name,
                        next_event.timestamp,
                        parent=next_event.parent,
                    )
                elif next_event.payload.test_suite_started:
                    styled_name = statusinfo.highlight(
                        next_event.payload.test_suite_started.name,
                        style=flags.style,
                    )
                    state.active_durations[next_event.id] = DurationInfo(
                        styled_name,
                        next_event.timestamp,
                        parent=next_event.parent,
                    )
                else:
                    # Fallback. Display an ugly error if this is triggered so that we can fix the bug.
                    styled_name = statusinfo.error_highlight(
                        f"BUG: no title for {next_event.payload.to_dict()}",  # type:ignore
                        style=flags.style,
                    )
                    state.active_durations[next_event.id] = DurationInfo(
                        styled_name,
                        next_event.timestamp,
                        parent=next_event.parent,
                    )

            if (
                next_event.id is not None
                and next_event.parent in test_suite_execution_info
            ):
                # Track direct children of a test suite, so their
                # output can be buffered to the right suite.
                if next_event.starting:
                    event_id_to_test_suite[next_event.id] = next_event.parent
                elif next_event.ending:
                    del event_id_to_test_suite[next_event.id]

            if next_event.payload.process_env is not None:
                # Extract the path from the parsed environment.
                root_path = next_event.payload.process_env["fuchsia_dir"]
            elif next_event.payload.user_message is not None:
                # Style and display user messages.
                msg = next_event.payload.user_message
                if msg.level == event.MessageLevel.INSTRUCTION:
                    text = statusinfo.dim(msg.value, style=flags.style)
                elif msg.level == event.MessageLevel.WARNING:
                    text = statusinfo.warning(msg.value, style=flags.style)
                elif msg.level == event.MessageLevel.INFO:
                    text = msg.value
                elif msg.level == event.MessageLevel.VERBATIM:
                    text = msg.value
                else:
                    text = msg.value
                lines_to_print.append(text)
            elif next_event.payload.program_output is not None:
                output = next_event.payload.program_output

                if output.data.endswith("\n"):
                    data = output.data[:-1]
                else:
                    data = output.data

                if output.print_verbatim:
                    # If a program execution requests verbatim output,
                    # print to console.
                    lines_to_print.append(data)
                elif (
                    next_event.id is not None
                    and (suite_id := event_id_to_test_suite.get(next_event.id))
                    is not None
                ):
                    # This output corresponds to a running suite.
                    suite_info = test_suite_execution_info[suite_id]
                    if suite_info.print_verbatim():
                        # Already timed out, just print this output verbatim.
                        lines_to_print.append(data)
                    elif suite_info.should_buffer_output():
                        # Awaiting timeout, buffer this output.
                        suite_info.buffered_lines.append(data)
            elif next_event.payload.test_file_loaded is not None:
                # Print a result to the user when the tests file is parsed.
                test_info = next_event.payload.test_file_loaded
                path = (
                    "//" + os.path.relpath(test_info.file_path, root_path)
                    if root_path
                    else test_info.file_path
                )
                lines_to_print.append(
                    f"\nFound {len(test_info.test_entries)} total tests in {statusinfo.green(path, style=flags.style)}"
                )
            elif next_event.payload.test_selections:
                # Print a result to the user when tests are selected.
                count = len(next_event.payload.test_selections.selected)
                label = statusinfo.highlight(
                    f"{count} test{'s' if count != 1 else ''}",
                    style=flags.style,
                )
                suffix = statusinfo.highlight(
                    f" {flags.count} times" if flags.count > 1 else "",
                    style=flags.style,
                )
                lines_to_print.append(f"\nPlan to run {label}{suffix}")
            elif next_event.payload.build_targets is not None:
                # Print the number of targets we are refreshing.
                label = statusinfo.highlight(
                    f"{len(next_event.payload.build_targets)} targets",
                    style=flags.style,
                )
                lines_to_print.append(
                    f"\n{statusinfo.green('Refreshing', style=flags.style)} {label}"
                )
                # Also output the command line used for fx build up to a limit,
                # to avoid scrolling multiple pages.
                lines_to_print.append(
                    statusinfo.ellipsize(
                        statusinfo.green_highlight(
                            f"> fx build {' '.join(next_event.payload.build_targets)}",
                            style=flags.style,
                        ),
                        width=80 * 5,  # Approximately 5 lines
                    ),
                )
            elif next_event.payload.test_group is not None:
                # Let the user know we intend to run a number of tests.
                val = statusinfo.highlight(
                    f"{next_event.payload.test_group.queued_events} tests",
                    style=flags.style,
                )
                label = statusinfo.green("Running", style=flags.style)
                lines_to_print.append(f"\n{label} {val}")
            elif next_event.payload.test_suite_started is not None:
                # Let the user know a test suite is starting.
                assert next_event.id
                suite_info = TestExecutionInfo(
                    next_event.payload.test_suite_started.name
                )
                test_suite_execution_info[next_event.id] = suite_info
                if flags.slow > 0:
                    suite_info.buffered_lines.extend(
                        [
                            statusinfo.dim(
                                f"Runtime has exceeded {flags.slow} seconds",
                                style=flags.style,
                            ),
                            f"Showing output for {test_suite_execution_info[next_event.id].name}",
                        ]
                    )
                    suite_info.spawn_buffered_output_printer(
                        flags.slow, print_queue
                    )

                label = "Starting:"
                val = statusinfo.green_highlight(
                    next_event.payload.test_suite_started.name,
                    style=flags.style,
                )
                # Explicitly mark if the suite is hermetic or not.
                hermeticity = (
                    ""
                    if next_event.payload.test_suite_started.hermetic
                    else statusinfo.warning("(NOT HERMETIC)", style=flags.style)
                )
                lines_to_print.append(f"\n{label} {val} {hermeticity}")
            elif next_event.payload.test_suite_ended is not None:
                # Let the user know a test suite has ended, and
                # what its status is.
                assert next_event.id
                payload = next_event.payload.test_suite_ended
                if payload.status == event.TestSuiteStatus.PASSED:
                    label = statusinfo.green_highlight(
                        "PASSED", style=flags.style
                    )
                elif payload.status == event.TestSuiteStatus.FAILED:
                    label = statusinfo.error_highlight(
                        "FAILED", style=flags.style
                    )
                elif payload.status == event.TestSuiteStatus.SKIPPED:
                    label = statusinfo.highlight("SKIPPED", style=flags.style)
                elif payload.status == event.TestSuiteStatus.ABORTED:
                    label = statusinfo.highlight("ABORTED", style=flags.style)
                elif payload.status == event.TestSuiteStatus.TIMEOUT:
                    label = statusinfo.error_highlight(
                        "TIMEOUT", style=flags.style
                    )
                else:
                    label = statusinfo.error_highlight(
                        "BUG: UNKNOWN", style=flags.style
                    )

                # Record status of the test, and stop tracking the test task.
                finished_test = test_suite_execution_info[next_event.id]
                state.test_results[payload.status].add(finished_test.name)
                finished_test.cleanup()
                del test_suite_execution_info[next_event.id]

                suffix = ""
                if payload.message:
                    suffix = "\n" + statusinfo.dim(payload.message) + "\n"

                lines_to_print.append(
                    f"\n{label}: {finished_test.name}{suffix}"
                )

            elif next_event.payload.enumerate_test_cases is not None:
                cases_payload = next_event.payload.enumerate_test_cases
                styled_name = statusinfo.green_highlight(
                    cases_payload.test_name, style=flags.style
                )
                lines_to_print.append(f"\nTest cases in {styled_name}:")
                for line in cases_payload.test_case_names:
                    lines_to_print.append(
                        f" {statusinfo.highlight(line, style=flags.style)}"
                    )
                    command = f'fx test {cases_payload.test_name} --test-filter "{line}"'
                    lines_to_print.append(
                        f" {statusinfo.dim(command, style=flags.style)}"
                    )
            elif next_event.payload.load_config is not None:
                load_config = next_event.payload.load_config
                lines_to_print.extend(
                    [
                        statusinfo.highlight(
                            f"Default flags loaded from {load_config.path}:",
                            style=flags.style,
                        ),
                        statusinfo.dim(f"{str(load_config.command_line)}\n"),
                    ]
                )

        if next_event.error:
            # Highlight all errors
            lines_to_print.extend(
                [
                    statusinfo.error_highlight(line, style=flags.style)
                    for line in ("ERROR: " + next_event.error).splitlines()
                ]
            )

        if lines_to_print:
            await print_queue.put(lines_to_print)


def pass_format(count: int, style: bool = True) -> str:
    """Helper to format passing tests.

    Args:
        count (int): The number of passing tests. Don't highlight for 0.
        style (bool, optional): Only style if True. Defaults to True.

    Returns:
        str: Formatted test count.
    """
    label = f"PASS: {count}"
    if count > 0:
        return statusinfo.green_highlight(label, style=style)
    else:
        return statusinfo.dim(label, style=style)


def fail_format(count: int, style: bool = True) -> str:
    """Helper to format failing tests.

    Args:
        count (int): The number of failing tests. Don't highlight for 0.
        style (bool, optional): Only style if True. Defaults to True.

    Returns:
        str: Formatted test count.
    """
    label = f"FAIL: {count}"
    if count > 0:
        return statusinfo.error_highlight(label, style=style)
    else:
        return statusinfo.dim(label, style=style)


def skip_format(count: int, style: bool = True) -> str:
    """Helper to format skipped tests.

    Args:
        count (int): The number of skipped tests. Don't highlight for 0.
        style (bool, optional): Only style if True. Defaults to True.

    Returns:
        str: Formatted test count.
    """
    label = f"SKIP: {count}"
    if count > 0:
        return label
    else:
        return statusinfo.dim(label, style=style)
