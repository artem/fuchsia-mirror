# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Data model and associated methods for tests.json files.

tests.json is generated by the GN build for Fuchsia, and it contains
descriptions of all test targets included in a build.

It is typically stored at the root of the output directory for the
Fuchsia build, and it is the main entrypoint to learn about what
tests are available and how to rebuild them.

"""

from dataclasses import dataclass
import json
import typing

from dataparse import dataparse


@dataparse
@dataclass
class TestSection:
    """Provides details for a specific test in tests.json."""

    # The name of the test. This is unique within the file.
    name: str

    # The build label for the test.
    label: str

    # The os configured for the test.
    os: str

    # The build label for the component being tested in this test.
    component_label: str | None = None

    # The build label for the package containing the test.
    package_label: str | None = None

    # If the test runs on the device, this is the URL of the test to execute.
    package_url: str | None = None

    # If the test runs on the host, this is the path of the binary to execute.
    path: str | None = None

    # Experimental host binary paths for test components
    new_path: str | None = None

    # If the test is a device tests, this is the argument to pass to
    # ffx test run --parallel
    parallel: int | None = None


@dataparse
@dataclass
class DimensionsEntry:
    """A single entry for an environment's test dimensions."""

    # The type of device this test is intended to run on.
    # Missing if the test is not for devices.
    device_type: str | None = None


@dataparse
@dataclass
class EnvironmentEntry:
    """Provides details for a test environment in tests.json."""

    # The dimensions for this environment.
    dimensions: DimensionsEntry


class TestFileError(Exception):
    """There was an error processing the contents of the tests.json file."""


@dataparse
@dataclass
class TestEntry:
    """tests.json consists of a single list of TestEntity."""

    # The "test" field for a specific entry in the file.
    test: TestSection

    # The "environments" field for a specific entry in the file.
    environments: list[EnvironmentEntry] | None = None

    # Optional field that is set for boot tests only.
    product_bundle: str | None = None

    @classmethod
    def from_file(
        cls: typing.Type[typing.Self], file: str
    ) -> list[typing.Self]:
        """Parse the file at the given path into a list of TestEntry.

        This returns a list of entries because the ordering of the
        tests.json file is the default order in which tests are
        executed.

        Args:
            file (os.PathLike): Path to the file to parse.

        Raises:
            IOError: If reading the file fails.
            JSONDecodeError: If the file is not valid JSON.
            TestFileError: If the tests.json file was found to be invalid.

        Returns:
            list[TestEntry]: List of test entries contained in the file.
        """
        with open(file, "r") as f:
            vals = json.load(f)
            if not isinstance(vals, list):
                raise TestFileError(
                    "Expected a list at top-level of tests.json, found "
                    + str(type(vals))
                )
            ret: list[typing.Self] = list(
                map(TestEntry.from_dict, vals)  # type:ignore
            )
            name_path_pairs: typing.Set[typing.Tuple[str, str | None]] = set()
            for v in ret:
                if (v.test.name, v.test.path) in name_path_pairs:
                    raise TestFileError(
                        f"Expected all names/path pairs to be unique in tests.json, but found {v.test.name} twice for the same path."
                    )
                name_path_pairs.add((v.test.name, v.test.path))

            return ret
