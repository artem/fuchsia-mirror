# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import tempfile
import typing
import unittest
import unittest.mock as mock

from parameterized import parameterized

import args

# We need a place to create a temporary file that is used to ensure
# output directory checking works correctly. Place it here and then
# cleanup after the tests are done executing.
GLOBAL_TEMP_DIRECTORY = tempfile.TemporaryDirectory()
GLOBAL_FILE_NAME = os.path.join(GLOBAL_TEMP_DIRECTORY.name, "tempfile")
open(GLOBAL_FILE_NAME, "w").close()


class TestArgs(unittest.TestCase):
    @classmethod
    def tearDownClass(cls) -> None:
        GLOBAL_TEMP_DIRECTORY.cleanup()
        return super().tearDownClass()

    def test_empty(self):
        flags = args.parse_args([])
        flags.validate()

    @parameterized.expand(
        [
            (
                "cannot show status with --simple",
                ["--simple", "--status"],
            ),
            (
                "cannot show style with --simple",
                ["--simple", "--style"],
            ),
            (
                "cannot show status when terminal is not a TTY",
                ["--status"],
            ),
            (
                "cannot run only host and only device tests",
                ["--device", "--host"],
            ),
            (
                "cannot exceed maximum status frequency",
                ["--status-delay", ".00001"],
            ),
            (
                "cannot have a negative suggestion count",
                ["--suggestion-count", "-1"],
            ),
            (
                "cannot run tests 0 times",
                ["--count", "0"],
            ),
            (
                "cannot have a negative timeout",
                ["--timeout", "-3"],
            ),
            (
                "cannot output to a file",
                ["--ffx-output-directory", GLOBAL_FILE_NAME],
            ),
        ]
    )
    @mock.patch("args.termout.is_valid", return_value=False)
    def test_validation_errors(
        self, _unused_name, arg_list: typing.List[str], _mock: mock.Mock
    ):
        flags = args.parse_args(arg_list)
        try:
            with self.assertRaises(args.FlagError):
                flags.validate()
        except AssertionError:
            raise AssertionError("Expected FlagError from " + str(arg_list))

    def test_simple(self):
        flags = args.parse_args(["--simple"])
        flags.validate()
        self.assertEqual(flags.style, False)
        self.assertEqual(flags.status, False)

    def test_e2e(self):
        flags = args.parse_args(["--only-e2e"])
        flags.validate()
        self.assertEqual(flags.e2e, True)
        self.assertEqual(flags.only_e2e, True)
