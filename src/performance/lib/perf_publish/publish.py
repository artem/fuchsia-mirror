# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import logging
import os
import re
import subprocess
import time
from typing import Any, Iterable, Self, Union

import perf_publish.metrics_allowlist as metrics_allowlist
import perf_publish.summarize as summarize

_LOGGER: logging.Logger = logging.getLogger(__name__)

# The 'test_suite' field should be all lower case.  It should start with 'fuchsia.', to distinguish
# Fuchsia test results from results from other projects that upload to Catapult (Chromeperf),
# because the namespace is shared between projects and Catapult does not enforce any separation
# between projects.
_TEST_SUITE_REGEX: re.Pattern = re.compile(
    r"^fuchsia\.([a-z0-9_-]+\.)*[a-z0-9_-]+$"
)

# The regexp for the 'label' field is fairly permissive. This reflects what is currently generated
# by tests.
_LABEL_REGEX: re.Pattern = re.compile(r"^[A-Za-z0-9_/.:=+<>\\ -]+$")

_FUCHSIA_PERF_EXT: str = ".fuchsiaperf.json"
_FUCHSIA_PERF_FULL_EXT: str = ".fuchsiaperf_full.json"
_CATAPULT_UPLOAD_ENABLED_EXT: str = ".catapult_json"
_CATAPULT_UPLOAD_DISABLED_EXT: str = ".catapult_json_disabled"
_SUMMARIZED_RESULTS_FILE: str = f"results{_FUCHSIA_PERF_EXT}"

ENV_CATAPULT_DASHBOARD_MASTER: str = "CATAPULT_DASHBOARD_MASTER"
ENV_CATAPULT_DASHBOARD_BOT: str = "CATAPULT_DASHBOARD_BOT"
ENV_BUILDBUCKET_ID: str = "BUILDBUCKET_ID"
ENV_BUILD_CREATE_TIME: str = "BUILD_CREATE_TIME"
ENV_RELEASE_VERSION: str = "RELEASE_VERSION"
ENV_FUCHSIA_EXPECTED_METRIC_NAMES_DEST_DIR: str = (
    "FUCHSIA_EXPECTED_METRIC_NAMES_DEST_DIR"
)


def publish_fuchsiaperf(
    fuchsia_perf_file_paths: Iterable[Union[str, os.PathLike]],
    expected_metric_names_filename: str,
    env: Union[dict[str, str], type(os.environ)] = os.environ,
    converter_path: Union[str, os.PathLike] | None = None,
) -> None:
    """Publishes the given metrics.

    Args:
        fuchsia_perf_file_paths: paths to the fuchsiaperf.json files containing the metrics. These
            will be summarized into a single fuchsiaperf.json file.
        env: map holding the environment variables.
        converter_path: path to the catapult_converter binary.
        expected_metric_names_filename: allows to optionally validate the metrics in the perf file
            against a set of expected metrics.
    """
    converter = CatapultConverter.from_env(fuchsia_perf_file_paths, env)
    converter.run(expected_metric_names_filename, converter_path=converter_path)


class CatapultConverter:
    def __init__(
        self,
        fuchsia_perf_file_paths: Iterable[Union[str, os.PathLike]],
        master: str | None = None,
        bot: str | None = None,
        build_bucket_id: str | None = None,
        build_create_time: str | None = None,
        release_version: str | None = None,
        fuchsia_expected_metric_names_dest_dir: str | None = None,
        current_time: int | None = None,
        subprocess_check_call: Any = subprocess.check_call,
    ):
        """Creates a new catapult converter.

        Args:
            fuchsia_perf_file_paths: paths to the fuchsiaperf.json files containing the metrics.
                These will be summarized into a single fuchsiaperf.json file.
            fuchsia_expected_metric_names_dest_dir: directory to which expected metrics are written.
            current_time: the current time, useful for testing. Defaults to time.time.
            subprocess_check_call: allows to execute a process raising an exception on error.
                Useful for testing. Defaults to subprocess.check_call.

        See //src/testing/catapult_converter/README.md for the rest of args.
        """
        self._release_version = release_version
        self._subprocess_check_call = subprocess_check_call
        self._fuchsia_expected_metric_names_dest_dir = (
            fuchsia_expected_metric_names_dest_dir
        )

        # We expect the binary to be in a `runtime_deps` directory hosting the test cases in a
        # parent directory.
        cur_path: str = os.path.dirname(__file__)
        while not os.path.isdir(os.path.join(cur_path, "runtime_deps")):
            cur_path = os.path.dirname(cur_path)
            if cur_path == "/":
                raise ValueError(
                    "Couldn't find required runtime_deps directory"
                )
        self._runtime_deps_dir = os.path.join(cur_path, "runtime_deps")

        self._upload_enabled: bool = True
        if master is None and bot is None:
            _LOGGER.info(
                "CatapultConverter: Infra env vars are not set; treating as a local run."
            )
            self._bot: str = "local-bot"
            self._master: str = "local-master"
            self._log_url: str = "http://ci.example.com/build/300"
            self._timestamp: int = (
                int(current_time if current_time else time.time()) * 1000
            )
            # Disable uploading so that we don't accidentally upload with the placeholder values
            # set here.
            self._upload_enabled = False
        elif (
            master is not None
            and bot is not None
            and build_bucket_id is not None
            and build_create_time is not None
        ):
            self._bot = bot
            self._master = master
            self._log_url = f"https://ci.chromium.org/b/{build_bucket_id}"
            self._timestamp = int(build_create_time)
        else:
            raise ValueError(
                "Catapult-related infra env vars are not set consistently"
            )

        fuchsia_perf_file_paths = self._check_extension_and_relocate(
            fuchsia_perf_file_paths
        )

        results = summarize.summarize_perf_files(fuchsia_perf_file_paths)
        self._results_path = os.path.join(
            os.path.dirname(fuchsia_perf_file_paths[0]),
            _SUMMARIZED_RESULTS_FILE,
        )
        assert not os.path.exists(self._results_path)
        with open(self._results_path, "w") as f:
            summarize.write_fuchsiaperf_json(f, results)

        catapult_extension = (
            _CATAPULT_UPLOAD_ENABLED_EXT
            if self._upload_enabled
            else _CATAPULT_UPLOAD_DISABLED_EXT
        )
        self._output_file: str = (
            self._results_path.removesuffix(_FUCHSIA_PERF_EXT)
            + catapult_extension
        )

    def _check_extension_and_relocate(self, fuchsia_perf_file_paths):
        fuchsia_perf_file_paths = list(map(str, fuchsia_perf_file_paths))
        if len(fuchsia_perf_file_paths) == 0:
            raise ValueError("Expected at least one fuchsiaperf.json file")
        files_with_wrong_ext = []
        files_to_rename = []
        paths = []

        for p in fuchsia_perf_file_paths:
            if p.endswith(_FUCHSIA_PERF_EXT):
                files_to_rename.append(p)
            elif p.endswith(_FUCHSIA_PERF_FULL_EXT):
                paths.append(p)
            else:
                files_with_wrong_ext.append(p)

        if files_with_wrong_ext:
            raise ValueError(
                f"The following files must end with {_FUCHSIA_PERF_FULL_EXT} or {_FUCHSIA_PERF_EXT}:"
                "\n- {}\n".format("\n- ".join(files_with_wrong_ext))
            )

        for file in files_to_rename:
            file_without_suffix = file.removesuffix(_FUCHSIA_PERF_EXT)
            new_file = f"{file_without_suffix}{_FUCHSIA_PERF_FULL_EXT}"
            assert not os.path.exists(new_file)
            os.rename(file, new_file)
            paths.append(new_file)

        return paths

    @classmethod
    def from_env(
        cls,
        fuchsia_perf_file_paths: Iterable[Union[str, os.PathLike]],
        env: Union[dict[str, str], type(os.environ)] = os.environ,
        current_time: int | None = None,
        subprocess_check_call: Any = subprocess.check_call,
    ) -> Self:
        """Creates a new catapult converter using the environment variables.

        Args:
            fuchsia_perf_file_paths: paths to the fuchsiaperf.json files containing the metrics.
            env: map holding the environment variables.
            current_time: the current time, useful for testing. Defaults to time.time.
            subprocess_check_call: allows to execute a process raising an exception on error.
                Useful for testing. Defaults to subprocess.check_call.
        """
        return cls(
            fuchsia_perf_file_paths,
            master=env.get(ENV_CATAPULT_DASHBOARD_MASTER),
            bot=env.get(ENV_CATAPULT_DASHBOARD_BOT),
            build_bucket_id=env.get(ENV_BUILDBUCKET_ID),
            build_create_time=env.get(ENV_BUILD_CREATE_TIME),
            release_version=env.get(ENV_RELEASE_VERSION),
            fuchsia_expected_metric_names_dest_dir=env.get(
                ENV_FUCHSIA_EXPECTED_METRIC_NAMES_DEST_DIR
            ),
            current_time=current_time,
            subprocess_check_call=subprocess_check_call,
        )

    def run(
        self,
        expected_metric_names_filename: str,
        converter_path: Union[str, os.PathLike] | None = None,
        expected_metric_names_dir: Union[str, os.PathLike] | None = None,
    ) -> None:
        """Publishes the given metrics.

        Args:
            expected_metric_names_filename: file required to validate the metrics in the perf file
                against a set of expected metrics.
            converter_path: path to the catapult_converter binary.
            expected_metric_names_dir: optional dir in which to look for metric name expectation files.
                Defaults to the test runtime_deps dir.
        """
        if converter_path is None:
            converter_path = os.path.join(
                self._runtime_deps_dir, "catapult_converter"
            )

        expected_metric_names_dir: str = (
            self._runtime_deps_dir
            if expected_metric_names_dir is None
            else str(expected_metric_names_dir)
        )
        expected_metric_names_file: str = os.path.join(
            expected_metric_names_dir, expected_metric_names_filename
        )

        _LOGGER.info("Converting the results to the catapult format")
        self._check_fuchsia_perf_metrics_naming(expected_metric_names_file)
        args = self._args()
        _LOGGER.info(f'Performance: Running {converter_path} {" ".join(args)}')
        self._subprocess_check_call([str(converter_path)] + args)
        _LOGGER.info(
            f"Conversion to catapult results format completed. Output file: {self._output_file}"
        )

    def _check_fuchsia_perf_metrics_naming(
        self,
        expected_metric_names_file: str,
    ) -> None:
        metrics = self._extract_perf_file_metrics()
        if self._fuchsia_expected_metric_names_dest_dir is None:
            metric_allowlist = metrics_allowlist.MetricsAllowlist(
                expected_metric_names_file
            )
            metric_allowlist.check(metrics)
        else:
            self._write_expectation_file(
                metrics,
                expected_metric_names_file,
                self._fuchsia_expected_metric_names_dest_dir,
            )

    def _extract_perf_file_metrics(self) -> set[str]:
        with open(self._results_path) as f:
            json_data: str = json.load(f)

        if not isinstance(json_data, list):
            raise ValueError("Top level fuchsiaperf node should be a list")

        errors: list[str] = []
        entries: set[str] = set()
        for entry in json_data:
            if not isinstance(entry, dict):
                raise ValueError(
                    "Expected entries in fuchsiaperf list to be objects"
                )
            if "test_suite" not in entry:
                raise ValueError(
                    'Expected key "test_suite" in fuchsiaperf entry'
                )
            if "label" not in entry:
                raise ValueError('Expected key "label" in fuchsiaperf entry')

            test_suite: str = entry["test_suite"]
            if not re.match(_TEST_SUITE_REGEX, test_suite):
                errors.append(
                    f'test_suite field "{test_suite}" does not match the pattern '
                    f'"{_TEST_SUITE_REGEX}"'
                )
                continue

            label: str = entry["label"]
            if not re.match(_LABEL_REGEX, label):
                errors.append(
                    f'test_suite field {label} does not match the pattern "{_LABEL_REGEX}"'
                )
                continue

            entries.add(f"{test_suite}: {label}")

        if errors:
            errors_string = "\n".join(errors)
            raise ValueError(
                "Some performance test metrics don't follow the naming conventions:\n"
                f"{errors_string}"
            )

        return entries

    def _write_expectation_file(
        self,
        metrics: set[str],
        expected_metric_names_filename: str,
        fuchsia_expected_metric_names_dest_dir: str,
    ) -> None:
        dest_file: str = os.path.join(
            fuchsia_expected_metric_names_dest_dir,
            os.path.basename(expected_metric_names_filename),
        )
        with open(dest_file, "w") as f:
            for metric in sorted(metrics):
                f.write(f"{metric}\n")

    def _args(self) -> list[str]:
        args: list[str] = [
            "--input",
            str(self._results_path),
            "--output",
            self._output_file,
            "--execution-timestamp-ms",
            str(self._timestamp),
            "--masters",
            self._master,
            "--log-url",
            self._log_url,
            "--bots",
            self._bot,
        ]
        if self._release_version is not None:
            args += ["--product-versions", self._release_version]
        return args
