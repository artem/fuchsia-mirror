# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import subprocess
import tempfile
import unittest

import args
import environment
import event
import execution
import test_list_file
import tests_json_file


class TestExecution(unittest.IsolatedAsyncioTestCase):
    async def test_run_command(self):
        """Test that run_command works with and without events"""
        with tempfile.TemporaryDirectory() as tmp:
            open(os.path.join(tmp, "temp-file.txt"), "w").close()

            output = await execution.run_command(
                "ls", "temp-file.txt", env={"CWD": tmp}
            )
            assert output is not None
            self.assertEqual(output.return_code, 0)

            recorder = event.EventRecorder()
            recorder.emit_init()

            output = await execution.run_command(
                "ls", "temp-file.txt", env={"CWD": tmp}, recorder=recorder
            )
            assert output is not None
            self.assertEqual(output.return_code, 0)

            recorder.emit_end()

            events = [e async for e in recorder.iter()]
            # Ensure we got init, end, and at least one sub-event start/stop
            self.assertGreater(len(events), 4)

    async def test_run_command_failure(self):
        """Test that running an invalid command emits an error event"""
        recorder = event.EventRecorder()
        recorder.emit_init()
        output = await execution.run_command(
            "___invalid_command_name___", recorder=recorder
        )
        recorder.emit_end()
        events = [e async for e in recorder.iter()]

        # Ensure we get no output and that at least one event is an error.
        self.assertIsNone(output)
        self.assertTrue(any([e.error is not None for e in events]))

    async def test_test_execution_component(self):
        """Test the usage of the TestExecution wrapper on a component test"""

        exec_env = environment.ExecutionEnvironment(
            "/fuchsia", "/out/fuchsia", None, "", ""
        )

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(tests_json_file.TestSection("foo", "//foo")),
                test_list_file.TestListEntry(
                    "foo",
                    [],
                    test_list_file.TestListExecutionEntry(
                        "fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm",
                        realm="foo_tests",
                        max_severity_logs="INFO",
                        min_severity_logs="TRACE",
                    ),
                ),
            ),
            exec_env,
        )

        self.assertListEqual(
            test.command_line(),
            [
                "fx",
                "ffx",
                "test",
                "run",
                "--realm",
                "foo_tests",
                "--max-severity-logs",
                "INFO",
                "--min-severity-logs",
                "TRACE",
                "fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm",
            ],
        )

        self.assertFalse(test.is_hermetic())
        self.assertIsNone(test.environment())
        self.assertTrue(test.should_symbolize())

    async def test_test_execution_host(self):
        """Test the usage of the TestExecution wrapper on a host test, and actually run it"""

        with tempfile.TemporaryDirectory() as tmp:
            # We will run ls, but it needs to be relative to the output directory.
            # Find the actual path to the ls binary and symlink it into the
            # output directory.
            ls_path = subprocess.check_output(["which", "ls"]).decode().strip()
            self.assertTrue(os.path.isfile, f"{ls_path} is not a file")
            os.symlink(ls_path, os.path.join(tmp, "ls"))

            exec_env = environment.ExecutionEnvironment("/fuchsia", tmp, None, "", "")

            test = execution.TestExecution(
                test_list_file.Test(
                    tests_json_file.TestEntry(
                        tests_json_file.TestSection("foo", "//foo", path="ls")
                    ),
                    test_list_file.TestListEntry("foo", [], execution=None),
                ),
                exec_env,
            )

            self.assertFalse(test.is_hermetic())
            env = test.environment()
            assert env is not None
            self.assertDictEqual(env, {"CWD": tmp})
            self.assertFalse(test.should_symbolize())

            recorder = event.EventRecorder()
            recorder.emit_init()

            flags = args.parse_args([])

            output = await test.run(recorder, flags, event.GLOBAL_RUN_ID)
            recorder.emit_end()

            assert output is not None

            self.assertFalse(any([e.error is not None async for e in recorder.iter()]))
