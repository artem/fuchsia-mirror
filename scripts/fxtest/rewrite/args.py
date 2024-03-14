# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
from dataclasses import dataclass
import pathlib
import sys
import typing

import termout
import util.arg_option as arg_option


@dataclass
class Flags:
    """Command line flags for fx test.

    See `parse_args` for documentation.
    """

    dry: bool
    list: bool
    print_logs: bool

    build: bool
    updateifinbase: bool

    host: bool
    device: bool
    exact: bool
    e2e: bool
    only_e2e: bool
    selection: typing.List[str]
    fuzzy: int
    show_suggestions: bool
    suggestion_count: int

    parallel: int
    parallel_cases: int
    random: bool
    count: int
    limit: int | None
    offset: int
    min_severity_logs: typing.List[str]
    timeout: float | None
    test_filter: typing.List[str]
    fail: bool
    use_package_hash: bool
    restrict_logs: bool
    also_run_disabled_tests: bool
    show_full_moniker_in_logs: bool
    break_on_failure: bool
    breakpoints: typing.List[str]
    extra_args: typing.List[str]

    output: bool
    simple: bool
    style: bool
    log: bool
    logpath: str | None
    status: bool | None
    verbose: bool
    status_lines: int
    status_delay: float
    ffx_output_directory: str | None
    slow: float

    def validate(self) -> None:
        """Validate incoming flags, raising an exception on failure.

        Raises:
            FlagError: If the flags are invalid.
        """
        if self.simple and self.status:
            raise FlagError("--simple is incompatible with --status")
        if self.simple and self.style:
            raise FlagError("--simple is incompatible with --style")
        if self.device and self.host:
            raise FlagError("--device is incompatible with --host")
        if self.status_delay < 0.005:
            raise FlagError("--status-delay must be at least 0.005 (5ms)")
        if self.timeout and self.timeout <= 0:
            raise FlagError("--timeout must be greater than 0")
        if self.count < 1:
            raise FlagError("--count must be a positive number")
        if self.suggestion_count < 0:
            raise FlagError("--suggestion-count must be non-negative")
        if (
            self.ffx_output_directory is not None
            and pathlib.Path(self.ffx_output_directory).is_file()
        ):
            raise FlagError("--ffx-output-directory cannot be a file")
        if self.parallel < 0:
            raise FlagError("--parallel must be non-negative")
        if self.parallel_cases < 0:
            raise FlagError("--parallel-cases must be non-negative")

        if not termout.is_valid() and self.status:
            raise FlagError(
                "Refusing to output interactive status to a non-TTY."
            )

        if self.only_e2e:
            self.e2e = True

        if self.simple:
            self.style = False
            self.status = False
        else:
            if self.style is None:
                self.style = termout.is_valid()
            if self.status is None:
                self.status = termout.is_valid()

    def has_debugger(self) -> bool:
        return bool(self.break_on_failure or self.breakpoints)


class FlagError(Exception):
    """Raised if there was a problem parsing command line flags."""


def parse_args(
    cli_args: typing.List[str] | None = None, defaults: Flags | None = None
) -> Flags:
    """Parse command line flags.

    Args:
        cli_args (List[str], optional): Arguments to parse. If
            unset, read arguments from actual command line.
        defaults (Flags, optional): Default set of flags. If set,
            overrides the defaults from the command line.

    Returns:
        Flags: Typed representation of the command line for this program.
    """

    extra_args: typing.List[str] = []

    if cli_args is not None and "--" in cli_args:
        extra_index = cli_args.index("--")
        (cli_args, extra_args) = (
            cli_args[:extra_index],
            cli_args[extra_index + 1 :],
        )
    elif cli_args is None and "--" in sys.argv:
        extra_index = sys.argv.index("--")
        (sys.argv, extra_args) = (
            sys.argv[:extra_index],
            sys.argv[extra_index + 1 :],
        )

    parser = argparse.ArgumentParser(
        "fx test",
        description="Test Executor for Humans",
        exit_on_error=False,
    )
    utility = parser.add_argument_group("Utility Options")
    utility.add_argument(
        "--dry",
        action="store_true",
        help="Do not actually run tests. Instead print out the tests that would have been run.",
    )
    utility.add_argument(
        "--list",
        action="store_true",
        help="Do not actually run tests. Instead print out the list of test cases each test contains.",
    )
    utility.add_argument(
        "--print-logs",
        action="store_true",
        default=False,
        help="If set, pretty print the contents of the log file. If no --logpath is set, the default log output location is checked and the most recent log is used.",
    )

    build = parser.add_argument_group("Build Options")
    build.add_argument(
        "--build",
        action=argparse.BooleanOptionalAction,
        help="Invoke `fx build` before running the test suite (defaults to on)",
        default=True,
    )
    build.add_argument(
        "--updateifinbase",
        action=argparse.BooleanOptionalAction,
        help="Invoke `fx update-if-in-base` before running device tests (defaults to on)",
        default=True,
    )

    selection = parser.add_argument_group("Test Selection Options")
    selection.add_argument(
        "--host",
        action="store_true",
        default=False,
        help="Only run host tests. The opposite of `--device`",
    )
    selection.add_argument(
        "-d",
        "--device",
        action="store_true",
        default=False,
        help="Only run device tests. The opposite of `--host`",
    )
    selection.add_argument(
        "--exact",
        action="store_true",
        default=False,
        help="""Only match tests whose name exactly matches the selection.
        Cannot be specified along with --host or --device.""",
    )
    selection.add_argument(
        "--e2e",
        action=argparse.BooleanOptionalAction,
        help="Run selected end to end tests. Default is to not run e2e tests.",
        default=False,
    )
    selection.add_argument(
        "--only-e2e",
        action="store_true",
        default=False,
        help="Only run end to end tests. Implies --e2e.",
    )
    selection.add_argument(
        "-p",
        "--package",
        action=arg_option.SelectionAction,
        dest="selection",
        nargs="*",
        help="Match tests against their Fuchsia package name",
    )
    selection.add_argument(
        "-c",
        "--component",
        action=arg_option.SelectionAction,
        dest="selection",
        nargs="*",
        help="Match tests against their Fuchsia component name",
    )
    selection.add_argument(
        "-a",
        "--and",
        action=arg_option.SelectionAction,
        dest="selection",
        nargs="*",
        help="Add requirements to the preceding filter",
    )
    selection.add_argument(
        "selection", action=arg_option.SelectionAction, nargs="*"
    )
    selection.add_argument(
        "--fuzzy",
        type=int,
        default=3,
        help="The Damerau–Levenshtein distance threshold for fuzzy matching tests",
    )
    selection.add_argument(
        "--show-suggestions",
        action=argparse.BooleanOptionalAction,
        type=bool,
        help="If True and no tests match, suggest matching tests from the build directory. Default is True.",
        default=True,
    )
    selection.add_argument(
        "--suggestion-count",
        type=int,
        help="Show this number of suggestions if no tests match. Default is 6.",
        default=6,
    )

    execution = parser.add_argument_group("Execution Options")
    execution.add_argument(
        "--use-package-hash",
        action=argparse.BooleanOptionalAction,
        type=bool,
        help="Use the package Merkle root hash from the build artifacts to ensure you are running the most recently built device test code.",
        default=True,
    )
    execution.add_argument(
        "--parallel",
        type=int,
        help="Maximum number of test suites to run in parallel. Does not affect per-suite parallelism.",
        default=4,
    )
    execution.add_argument(
        "--parallel-cases",
        type=int,
        help="Instruct on-device test runners to prefer running this number of cases in parallel.",
        default=0,
    )
    execution.add_argument(
        "-r",
        "--random",
        action="store_true",
        help="Randomize test execution order",
        default=False,
    )
    execution.add_argument(
        "--timeout",
        type=float,
        help="Terminate tests that take longer than this number of seconds to complete. Default is no timeout.",
    )
    execution.add_argument(
        "--test-filter",
        type=str,
        default=[],
        nargs="*",
        help="Run specific test cases in a test suite. Can be specified multiple times to pass in multiple patterns.",
    )
    execution.add_argument(
        "--count",
        type=int,
        help="Execute each test this many times. If any iteration of a test times out, no further iterations will be executed",
        default=1,
    )
    execution.add_argument(
        "--limit",
        type=int,
        help="Stop execution after this many tests",
        default=None,
    )
    execution.add_argument(
        "--offset",
        type=int,
        help="Skip this many tests at the beginning of the test list. Combine with --limit to deterministically select a subrange of tests.",
        default=0,
    )
    execution.add_argument(
        "-f",
        "--fail",
        action="store_true",
        help="Stop running tests after the first failed test suite. This will abort all tests in progress and end with a failure code.",
        default=False,
    )
    execution.add_argument(
        "--restrict-logs",
        action=argparse.BooleanOptionalAction,
        help="If False, do not limit maximum log severity regardless of the test's configuration. Default is True.",
        default=True,
    )
    execution.add_argument(
        "--min-severity-logs",
        nargs="*",
        help="""Modifies the minimum log severity level emitted by components during the test execution.
        Specify using the format <component-selector>#<log-level>, or just <log-level> (in which
        case the severity will apply to all components under the test, including the test component
        itself) with level as one of FATAL|ERROR|WARN|INFO|DEBUG|TRACE.""",
        default=[],
    )
    execution.add_argument(
        "--also-run-disabled-tests",
        action="store_true",
        help="If True, also run tests that are disabled by the test author. This only affects test components. Default is False.",
        default=False,
    )
    execution.add_argument(
        "--show-full-moniker-in-logs",
        action=argparse.BooleanOptionalAction,
        help="""If set, show the full moniker in log output for on-device tests.
        Otherwise only the last segment of the moniker is displayed.
        Default is False.""",
        default=False,
    )
    execution.add_argument(
        "--break-on-failure",
        action="store_true",
        help="""If set, any test case failures will stop test execution and launch zxdb attached
        to the failed test case, if the test runner supports this feature. Implies --no-status.
        Note: this flag is experimental""",
        default=False,
    )
    execution.add_argument(
        "--breakpoint",
        nargs="*",
        dest="breakpoints",
        help="""Run the test with zxdb attached and set the given breakpoint. For example,
        --breakpoint=my_source_file.cc:37 will insert a breakpoint at line 37 of any file
        named my_source_file.cc. Implies --no-status. Note: this flag is experimental""",
        default=[],
    )

    output = parser.add_argument_group("Output Options")
    output.add_argument(
        "-o",
        "--output",
        action=argparse.BooleanOptionalAction,
        help="Display the output from passing tests. Some test arguments may be needed.",
    )
    output.add_argument(
        "--simple",
        action="store_true",
        help="Remove any color or decoration from output. Disable pretty status printing. Implies --no-style",
    )
    output.add_argument(
        "--style",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Remove color and decoration from output. Does not disable pretty status printing. Default is to only style for TTY output.",
    )
    output.add_argument(
        "--log",
        action=argparse.BooleanOptionalAction,
        help="Emit command events to a file. Turned on when running real tests unless `--no-log` is passed.",
        default=True,
    )
    output.add_argument(
        "--logpath",
        help="If passed and --log is enabled, customizes the destination of the target log.",
        default=None,
    )
    output.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        default=False,
        help="Print verbose logs to the console",
    )
    output.add_argument(
        "--status",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Toggle interactive status printing to console. Default is to vary behavior depending on if output is to a TTY. Setting to True on a non-TTY is an error.",
    )
    output.add_argument(
        "--status-lines",
        default=8,
        type=int,
        help="Number of lines used to display status output.",
    )
    output.add_argument(
        "--status-delay",
        default=0.033,
        type=float,
        help="Control how frequently the status output is updated. Default is every 0.033s, but you can increase the number for calmer output on slower connections.",
    )
    output.add_argument(
        "--ffx-output-directory",
        default=None,
        help="If set, write ffx test output to this directory for post processing.",
    )
    output.add_argument(
        "-s",
        "--slow",
        type=float,
        default=0,
        help="If non-zero, automatically show output for tests taking longer than this many seconds.",
    )

    if defaults is not None:
        actions = parser._actions.copy()
        groups_to_process: typing.List[
            argparse._ArgumentGroup
        ] = parser._action_groups.copy()

        # Recursively find all actions.
        while groups_to_process:
            group = groups_to_process.pop()
            actions.extend(group._actions)
            groups_to_process.extend(group._action_groups)

        # Apply defaults for all identified actions from the given defaults.
        for action in actions:
            if hasattr(defaults, action.dest):
                action.default = getattr(defaults, action.dest)

    flags: Flags = Flags(
        **vars(parser.parse_args(cli_args)), extra_args=extra_args
    )
    return flags
