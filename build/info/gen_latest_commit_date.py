#!/usr/bin/env fuchsia-vendored-python

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script gets an estimate for the time of the most recent commit from the
# integration repo. This is used at runtime as a known-good lower bound on what
# the current time is for cases in which the clock hasn't yet been updated from
# the network.
#
# We do this by taking the CommitDate of the most recent commit from the
# integration repo. Unlike commits in other repositories, the commits in the
# integration repo are always created by server infrastructure, and thus their
# CommitDate fields are more reliable. This is critical since a backstop time
# which is erroneously in the future could break many applications.
#
# The commit date is then truncated to midnight UTC as the extra granularity
# is not needed for the kernel backstop time and because it causes extra work
# on infra builders.

import argparse
from datetime import datetime, timezone
import os
import sys
import subprocess


def main():
    parser = argparse.ArgumentParser(
        description="Tool for extracting the latest commit date and hash from integration.git"
    )
    parser.add_argument("--repo", help="path to the repository", required=True)
    parser.add_argument(
        "--timestamp-file", help="path to write the unix-style timestamp to"
    )
    parser.add_argument(
        "--date-file", help="path to write the human-readable date string to"
    )
    parser.add_argument(
        "--commit-hash-file", help="path to write the commit hash itself to"
    )
    parser.add_argument(
        "--truncate",
        action="store_true",
        help="truncate the date to midnight UTC and use a fake commit hash",
    )

    args = parser.parse_args()

    # Set the following options to make the output as stable as possible:
    # - GIT_CONFIG_NOSYSTEM=1   - Don't check /etc/gitconfig
    # - --no-optional-locks     - Do not update the git index during read-only operations
    #                             (see https://fxbug.dev/42175708).
    # - --date=unix             - Format date as a unix timestamp
    # - --format=%H\n%cd        - Print the hash and CommitDate fields only, on separate lines
    git_cmd = [
        "git",
        "--no-optional-locks",
        f"--git-dir={args.repo}/.git",
        "log",
        "--date=unix",
        "--format=%H\n%cd",
        "-n",
        "1",
    ]
    git_env = {}
    git_env.update(os.environ)
    git_env["GIT_CONFIG_NOSYSTEM"] = "1"
    result = subprocess.run(
        git_cmd, env=git_env, check=True, capture_output=True, text=True
    )

    # Take the hash and timestamp lines, split and parse/convert them.
    output_lines = result.stdout.split()

    latest_commit_hash = output_lines[0]
    latest_commit_date_unix = int(output_lines[1])
    latest_commit_date = datetime.fromtimestamp(
        latest_commit_date_unix, timezone.utc
    )

    # Truncate the date if asked to do so, and replace the hash with one synthesized from the date
    if args.truncate:
        latest_commit_date = latest_commit_date.replace(
            hour=0, minute=0, second=0, microsecond=0
        )
        latest_commit_hash = f"{int(latest_commit_date.timestamp())}fedcba9876543210fedcba98765432"

    write_file_if_changed(
        args.timestamp_file, str(int(latest_commit_date.timestamp()))
    )
    write_file_if_changed(args.date_file, latest_commit_date.isoformat())
    write_file_if_changed(args.commit_hash_file, latest_commit_hash)


def write_file_if_changed(path: str, contents: str):
    if path:
        if contents_changed(path, contents):
            with open(path, "w") as file:
                file.write(contents)


def contents_changed(path: str, contents: str):
    try:
        with open(path, "r") as file:
            existing_contents = file.read()
            return existing_contents != contents
    except Exception as err:
        pass
    return True


if __name__ == "__main__":
    sys.exit(main())
