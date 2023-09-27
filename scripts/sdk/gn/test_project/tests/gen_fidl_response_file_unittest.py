#!/usr/bin/env fuchsia-vendored-python
# Copyright 2020 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import imp
import os
import shutil
import sys
import tempfile
import unittest

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
GEN_FIDL_RESPONSE_FILEPATH = os.path.join(
    SCRIPT_DIR,
    "..",
    "third_party",
    "fuchsia-sdk",
    "build",
    "gen_fidl_response_file.py",
)
if not os.path.exists(GEN_FIDL_RESPONSE_FILEPATH):
    print("CWCW not found", GEN_FIDL_RESPONSE_FILEPATH)
gen_fidl_response_file = imp.load_source(
    "gen_fidl_response_file", GEN_FIDL_RESPONSE_FILEPATH
)

TMP_DIR_NAME = tempfile.mkdtemp(
    prefix="tmp_unittest_%s_" % "GNFidlResponseFileTest"
)

FIDL_FILENAME = "echo.fidl"
OUT_RESPONSE_FILE_FILENAME = "out_response_file"
OUT_LIBRARIES_FILENAME = "out_libraries"

FIDL_FILE_CONTENTS = """library fidl.examples.echo;
// [START protocol]
@discoverable
protocol Echo {
    EchoString(struct {
        value string:optional;
    }) -> (struct {
        response string:optional;
    });
};
// [END protocol]
/// A service with multiple Echo protocol implementations.
service EchoService {
    /// An implementation of `Echo` that prefixes its output with "foo: ".
    foo client_end:Echo;
    /// An implementation of `Echo` that prefixes its output with "bar: ".
    bar client_end:Echo;
};
"""


class GNFidlResponseFileTest(unittest.TestCase):
    def setUp(self):
        # make sure TMP_DIR_NAME is empty
        if os.path.exists(TMP_DIR_NAME):
            shutil.rmtree(TMP_DIR_NAME)
        os.makedirs(TMP_DIR_NAME)
        with open(os.path.join(TMP_DIR_NAME, FIDL_FILENAME), "w") as f:
            f.write(FIDL_FILE_CONTENTS)

    def tearDown(self):
        if os.path.exists(TMP_DIR_NAME):
            shutil.rmtree(TMP_DIR_NAME)

    def testEmptyArchive(self):
        gen_fidl_response_file.main(
            [
                "--out-response-file",
                os.path.join(TMP_DIR_NAME, OUT_RESPONSE_FILE_FILENAME),
                "--out-libraries",
                os.path.join(TMP_DIR_NAME, OUT_LIBRARIES_FILENAME),
                "--sources",
                os.path.join(TMP_DIR_NAME, FIDL_FILENAME),
            ]
        )
        self.verify_contents(TMP_DIR_NAME)

    def verify_contents(self, outdir):
        out_response_file_filepath = os.path.join(
            outdir, OUT_RESPONSE_FILE_FILENAME
        )
        self.assertTrue(os.path.exists(out_response_file_filepath))
        with open(out_response_file_filepath) as f:
            file_contents = f.read()
            self.assertIn("--files", file_contents)

        out_libraries_filepath = os.path.join(outdir, OUT_LIBRARIES_FILENAME)
        self.assertTrue(os.path.exists(out_libraries_filepath))


def TestMain():
    unittest.main()


if __name__ == "__main__":
    TestMain()
