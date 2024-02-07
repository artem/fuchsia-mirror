#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import subprocess
import sys
from pathlib import Path

# Because files are directly passed, the mypy --exclude flag is bypassed. As a
# result, we maintain an exclusion list here instead of in
# `fuchsia/pyproject.toml`, and we use it to ignore files from the mypy command.
_EXCLUDE_PATTERNS = [
    "third_party/",
    "out/",
    "prebuilt/",
    "__pycache__",
    ".jiri_root",
]


def run_mypy_checks(files: list[str]) -> int:
    """
    Runs `mypy` type checking on the provided file paths, excluding duplicates

    Args:
        files: List of file paths to run type checking on.

    Returns:
        int: returncode if type checking was successful, else error returncode
    """

    # Remove the duplicate and excluded files.
    files = exclude_files(files)
    if not files:
        return ""

    fuchsia_dir = Path(__file__).parent.parent.parent
    config_path = fuchsia_dir / "pyproject.toml"
    pylibs_dir = fuchsia_dir / "third_party" / "pylibs"
    try:
        return subprocess.run(
            [
                sys.executable,
                "-S",
                "-m",
                "mypy",
                "--config-file",
                str(config_path),
            ]
            + files,
            env={"PYTHONPATH": f"{pylibs_dir}:{pylibs_dir}/mypy/src"},
            capture_output=True,
            text=True,
            check=True,
        ).returncode
    except subprocess.CalledProcessError as e:
        if e.returncode != 0:
            if e.stdout:
                print(
                    f"\nPlease fix the following Mypy errors:\n{e.stdout}\n",
                    file=sys.stderr,
                )
            else:
                print(
                    f"\nError occured during Mypy type checking:\n{e.stderr}\n",
                    file=sys.stderr,
                )
        return e.returncode


def exclude_files(file_list: list[str]) -> set[str]:
    """Exclude files matching the exclude_patterns from the file_list and
        remove duplicate file paths

    Args:
        file_list: List of file paths

    Returns:
        set of file paths after excluding the specified patterns
    """
    return sorted(
        set(
            file
            for file in file_list
            if not any(pattern in file for pattern in _EXCLUDE_PATTERNS)
        )
    )
