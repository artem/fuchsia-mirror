# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import re
import tempfile
import typing

import args
import environment
import event
import package_repository
import statusinfo
import test_list_file
import util.command as command


class TestExecutionError(Exception):
    """Base error type for test failures."""


class TestCouldNotRun(TestExecutionError):
    """The test could not be run at all."""


class TestSkipped(TestExecutionError):
    """The test was skipped for some non-error reason"""


class TestFailed(TestExecutionError):
    """The test ran, but returned a failure error code."""


class TestTimeout(TestExecutionError):
    """The test timed out."""


# Unique number suffix for output subdirectories.
UNIQUE_OUTPUT_SUFFIX = 0


class TestExecution:
    """Represents a single execution for a specific test."""

    def __init__(
        self,
        test: test_list_file.Test,
        exec_env: environment.ExecutionEnvironment,
        flags: args.Flags,
        run_suffix: int | None = None,
        device_env: environment.DeviceEnvironment | None = None,
    ):
        """Initialize the test execution wrapper.

        Args:
            test (test_list_file.Test): Test to run.
            exec_env (environment.ExecutionEnvironment): Execution environment.
            flags (args.Flags): Command flags.
            run_suffix (int, optional): If set, this is the unique
                index of a single run of the referenced test.
            device_env (environment.DeviceEnvironment, optional):
                If set, this contains information on how to connect to
                a device. Otherwise if this is a host test it may not
                connect to a device.
        """
        global UNIQUE_OUTPUT_SUFFIX
        self._test = test
        self._exec_env = exec_env
        self._flags = flags
        self._run_suffix = run_suffix
        self._device_env = device_env
        if self._flags.ffx_output_directory is not None:
            self._outdir: str | None = os.path.join(
                self._flags.ffx_output_directory, str(UNIQUE_OUTPUT_SUFFIX)
            )
            UNIQUE_OUTPUT_SUFFIX += 1
        else:
            self._outdir = None

    def name(self) -> str:
        """Get the name of the test.

        Returns:
            str: Name of the test.
        """
        return self._test.info.name + (
            f" (Run {self._run_suffix})" if self._run_suffix is not None else ""
        )

    def is_hermetic(self) -> bool:
        """Determine if a test is hermetic.

        Returns:
            bool: True if the wrapped test is hermetic, False otherwise.
        """
        return self._test.info.is_hermetic()

    def command_line(self) -> typing.List[str]:
        """Format the command line required to execute this test.

        Raises:
            TestCouldNotRun: If we do not know how to run this type of test.

        Returns:
            typing.List[str]: The command line for the test.
        """
        if self._test.info.execution is not None:
            execution = self._test.info.execution

            component_url = self._get_component_url()
            assert component_url is not None

            min_severity_logs: typing.List[str] = []
            if self._flags.min_severity_logs:
                min_severity_logs = self._flags.min_severity_logs
            elif execution.min_severity_logs is not None:
                min_severity_logs = [execution.min_severity_logs]

            extra_args = []
            if execution.realm:
                extra_args += ["--realm", execution.realm]
            if execution.max_severity_logs and self._flags.restrict_logs:
                extra_args += [
                    "--max-severity-logs",
                    execution.max_severity_logs,
                ]
            if min_severity_logs:
                for min_severity_log in min_severity_logs:
                    extra_args += ["--min-severity-logs", min_severity_log]
            if self._test.build.test.parallel is not None:
                extra_args += [
                    "--parallel",
                    str(self._test.build.test.parallel),
                ]
            for test_filter in self._flags.test_filter:
                extra_args += ["--test-filter", test_filter]
            if self._flags.also_run_disabled_tests:
                extra_args += ["--run-disabled"]
            if self._flags.show_full_moniker_in_logs:
                extra_args += ["--show-full-moniker-in-logs"]
            if self._flags.break_on_failure:
                extra_args += ["--break-on-failure"]
            if self._outdir is not None:
                extra_args += ["--output-directory", self._outdir]

            suffix_args = (
                ["--"] + self._flags.extra_args
                if self._flags.extra_args
                else []
            )

            return (
                ["fx", "ffx", "test", "run"]
                + extra_args
                + [component_url]
                + suffix_args
            )
        elif self._test.build.test.path:
            return [
                os.path.join(self._exec_env.out_dir, self._test.build.test.path)
            ] + self._flags.extra_args
        else:
            raise TestCouldNotRun(
                f"We do not know how to run this test: {str(self._test)}"
            )

    def enumerate_cases_command_line(self) -> typing.List[str] | None:
        """Get the command line to enumerate all test cases in this test.

        If this type of test does not support test case enumeration,
        return None.

        Returns:
            typing.List[str] | None: Command line to enumerate cases
                if possible, None otherwise.
        """

        execution = self._test.info.execution
        try:
            component_url = self._get_component_url()
        except TestCouldNotRun:
            return None
        if component_url is None or execution is None:
            return None

        extra_args = []
        if execution.realm:
            extra_args += ["--realm", execution.realm]

        return (
            ["fx", "ffx", "test", "list-cases"] + extra_args + [component_url]
        )

    def environment(self) -> typing.Dict[str, str] | None:
        """Format environment variables needed to run the test.

        Returns:
            typing.Optional[typing.Dict[str, str]]: Environment for
                the test, or None if no environment is needed.
        """
        env = {}
        if self._test.build.test.path or self._test.is_e2e_test():
            env.update(
                {
                    "CWD": self._exec_env.out_dir,
                }
            )

        if self._test.is_e2e_test() and self._device_env is not None:
            env.update(
                {
                    "FUCHSIA_DEVICE_ADDR": self._device_env.address,
                    "FUCHSIA_SSH_PORT": self._device_env.port,
                    "FUCHSIA_SSH_KEY": self._device_env.private_key_path,
                    "FUCHSIA_NODENAME": self._device_env.name,
                }
            )
        return None if not env else env

    def should_symbolize(self) -> bool:
        """Determine if we should symbolize the output of this test.

        Returns:
            bool: True if we should run the output through a symbolizer, False otherwise.
        """
        return self._test.info.execution is not None

    async def run(
        self,
        recorder: event.EventRecorder,
        flags: args.Flags,
        parent: event.Id,
        timeout: float | None = None,
    ) -> command.CommandOutput:
        """Asynchronously execute this test.

        Args:
            recorder (event.EventRecorder): Recorder for events.
            flags (args.Flags): Command flags to control output.
            parent (event.Id): Parent event to nest the execution under.
            timeout (float, optional): If set, timeout after this number of seconds.

        Raises:
            TestFailed: If the test reported failure.
            TestTimeout: If the test timed out.
            TestSkipped: If the test should not run.

        Returns:
            command.CommandOutput: The output of executing this command.
        """
        if self._test.is_e2e_test() and not flags.e2e:
            raise TestSkipped(
                "Skipping optional end to end test. Pass --e2e to execute this test."
            )

        symbolize = self.should_symbolize()
        command = self.command_line()
        env = self.environment() or {}

        outdir = self._outdir
        maybe_temp_dir: tempfile.TemporaryDirectory | None = None
        if not outdir:
            maybe_temp_dir = tempfile.TemporaryDirectory()
            outdir = maybe_temp_dir.name

        env.update(
            {
                "FUCHSIA_TEST_OUTDIR": outdir,
            }
        )
        output = await run_command(
            *command,
            recorder=recorder,
            parent=parent,
            print_verbatim=flags.output,
            symbolize=symbolize,
            env=env,
            timeout=timeout,
        )

        if maybe_temp_dir is not None:
            maybe_temp_dir.cleanup()

        if not output:
            raise TestFailed("Failed to run the test command")
        elif output.return_code != 0 or output.was_timeout:
            if not flags.output:
                # Test failed, print output now.
                recorder.emit_info_message(
                    f"\n{statusinfo.error_highlight(self._test.info.name, style=flags.style)}:\n"
                )
                if output.stdout:
                    recorder.emit_verbatim_message(output.stdout)
                if output.stderr:
                    recorder.emit_verbatim_message(output.stderr)
                if not output.stderr and not output.stdout:
                    recorder.emit_verbatim_message("<No command output>")
            if output.was_timeout:
                raise TestTimeout(f"Test exceeded runtime of {timeout} seconds")
            else:
                raise TestFailed("Test reported failure")
        return output

    def _get_component_url(self) -> str | None:
        """Get the final component URL to execute for this test.

        If this test is not a component test, return None.

        Raises:
            TestCouldNotRun: If we cannot determine the merkle hash for this test.

        Returns:
            str | None: The final URL to execute (optionally with
                hash), or None if this is not a component test.
        """
        if self._test.info.execution is None:
            return None

        execution = self._test.info.execution

        component_url = execution.component_url
        if self._flags.use_package_hash:
            try:
                package_repo = package_repository.PackageRepository.from_env(
                    self._exec_env
                )

                name = extract_package_name_from_url(component_url)
                if name is None:
                    raise TestCouldNotRun(
                        "Failed to parse package name for Merkle root matching.\nTry running with --no-use-package-hash."
                    )

                if name not in package_repo.name_to_merkle:
                    raise TestCouldNotRun(
                        f"Could not find a Merkle hash for this test: {component_url}\nTry running with --no-use-package-hash or rebuild your package repository."
                    )

                suffix = f"?hash={package_repo.name_to_merkle[name]}"
                component_url = component_url.replace("#", f"{suffix}#", 1)

            except package_repository.PackageRepositoryError as e:
                raise TestCouldNotRun(
                    f"Could not load a Merkle hash for this test ({str(e)})\nTry running with --no-use-package-hash or rebuild your package repository."
                )
        return component_url


_PACKAGE_NAME_REGEX = re.compile(r"fuchsia-pkg://fuchsia\.com/([^/#]+)#")


def extract_package_name_from_url(url: str) -> str | None:
    """Given a fuchsia-pkg URL, extract and return the package name.

    Example:
      fuchsia-pkg://fuchsia.com/my-package#meta/my-component.cm -> my-package

    Args:
        url (str): A fuchsia-pkg:// URL.

    Returns:
        str | None: The package name from the URL, or None if parsing failed.
    """
    match = _PACKAGE_NAME_REGEX.match(url)
    if match is None:
        return None
    return match.group(1)


class DeviceConfigError(Exception):
    """There was an error reading the device configuration"""


async def get_device_environment_from_exec_env(
    exec_env: environment.ExecutionEnvironment,
    recorder: event.EventRecorder | None = None,
) -> environment.DeviceEnvironment:
    ssh_output = await run_command(
        "fx", "ffx", "target", "get-ssh-address", recorder=recorder
    )
    if not ssh_output or ssh_output.return_code != 0:
        raise DeviceConfigError("Failed to get the ssh address of the target")

    last_colon_index = ssh_output.stdout.rfind(":")
    if last_colon_index == -1:
        raise DeviceConfigError(f"Could not parse: {ssh_output.stdout}")
    ip = ssh_output.stdout[0:last_colon_index].strip()
    port = ssh_output.stdout[last_colon_index + 1 :].strip()

    target_output = await run_command(
        "fx", "ffx", "target", "default", "get", recorder=recorder
    )
    if not target_output or target_output.return_code != 0:
        raise DeviceConfigError("Failed to get the target name")
    target_name = target_output.stdout.strip()

    with open(os.path.join(exec_env.fuchsia_dir, ".fx-ssh-path")) as f:
        lines = f.readlines()
        ssh_path = lines[0].strip()

    return environment.DeviceEnvironment(
        address=ip, port=port, name=target_name, private_key_path=ssh_path
    )


async def run_command(
    name: str,
    *args: str,
    recorder: event.EventRecorder | None = None,
    parent: event.Id | None = None,
    print_verbatim: bool = False,
    symbolize: bool = False,
    env: typing.Dict[str, str] | None = None,
    timeout: float | None = None,
) -> typing.Optional[command.CommandOutput]:
    """Utility method to run a test command asynchronously.

    Args:
        name (str): Command to run.
        args (typing.List[str]): Arguments to the command.
        recorder (event.EventRecorder | None):
            Recorder for events. Defaults to None.
        parent (event.Id | None): Parent event ID for reporting.
            Defaults to None.
        print_verbatim (bool, optional): If set, record verbatim
            output events for stdout and stderr. Defaults to False.
        symbolize (bool, optional): If true, pipe output through
            symbolizer. Defaults to False.
        env (typing.Dict[str, str], optional):
            Environment to pass to the command. Defaults to None.
        timeout (float, optional): The number of seconds to wait before timing out.

    Returns:
        command.CommandOutput | None: The command output if it could
            be executed, None otherwise.
    """
    id: event.Id
    if recorder is not None:
        id = recorder.emit_program_start(name, list(args), env, parent=parent)
    try:
        symbolizer_args = (
            None if not symbolize else ["fx", "ffx", "debug", "symbolize"]
        )
        started = await command.AsyncCommand.create(
            name,
            *args,
            symbolizer_args=symbolizer_args,
            env=env,
            timeout=timeout,
        )

        def handle_event(current_event: command.CommandEvent):
            if recorder is not None:
                if isinstance(current_event, command.StdoutEvent):
                    recorder.emit_program_output(
                        id,
                        current_event.text.decode(errors="replace"),
                        stream=event.ProgramOutputStream.STDOUT,
                        print_verbatim=print_verbatim,
                    )
                if isinstance(current_event, command.StderrEvent):
                    recorder.emit_program_output(
                        id,
                        current_event.text.decode(errors="replace"),
                        stream=event.ProgramOutputStream.STDERR,
                        print_verbatim=print_verbatim,
                    )
                if isinstance(current_event, command.TerminationEvent):
                    recorder.emit_program_termination(
                        id, current_event.return_code
                    )

        return await started.run_to_completion(callback=handle_event)
    except command.AsyncCommandError as e:
        if recorder is not None:
            recorder.emit_program_termination(id, -1, error=str(e))
        return None
