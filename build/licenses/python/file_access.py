#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
from pathlib import Path
from typing import Any, Callable, Set
from gn_label import GnLabel
import dataclasses
import os


@dataclasses.dataclass(frozen=False)
class FileAccess:
    """Manages access to the real file system, while keeping track of depfiles."""

    fuchsia_source_path_str: str | Path
    visited_files: Set[str] = dataclasses.field(default_factory=set)

    def read_text(self, label: GnLabel) -> str:
        """Reads the file into a text string"""
        GnLabel.check_type(label)
        path = os.path.join(self.fuchsia_source_path_str, label.path_str)
        self.visited_files.add(path)
        with open(path) as f:
            return f.read()

    def read_json(self, label: GnLabel) -> Any:
        GnLabel.check_type(label)
        path = os.path.join(self.fuchsia_source_path_str, label.path_str)
        self.visited_files.add(path)
        with open(path) as f:
            return json.load(f)

    def file_exists(self, label: GnLabel) -> bool:
        """Whether the file exists and is not a directory"""
        GnLabel.check_type(label)
        path = os.path.join(self.fuchsia_source_path_str, label.path_str)
        if os.path.isfile(path):
            self.visited_files.add(path)
            return True
        return False

    def directory_exists(self, label: GnLabel) -> bool:
        """Whether the directory exists and is indeed a directory"""
        GnLabel.check_type(label)
        path = os.path.join(self.fuchsia_source_path_str, label.path_str)
        if os.path.isdir(path):
            self.visited_files.add(path)
            return True
        return False

    def search_directory(
        self, label: GnLabel, path_predicate: Callable[[str], bool]
    ) -> list[GnLabel]:
        """Lists the files in a directory corresponding with `label` (including files in subdirs) matching `path_predicate`"""
        GnLabel.check_type(label)
        path = os.path.join(self.fuchsia_source_path_str, label.path_str)
        self.visited_files.add(path)

        output = []

        for root, _, files in os.walk(path):
            for file in files:
                file_path = os.path.join(root, file)
                if path_predicate(file_path):
                    relative_to_label = os.path.relpath(
                        os.path.relpath(
                            file_path, self.fuchsia_source_path_str
                        ),
                        label.path_str,
                    )
                    output.append(
                        label.create_child_from_str(relative_to_label)
                    )

        return output

    def write_depfile(self, dep_file_path: Path | str, main_entry: str) -> None:
        os.makedirs(os.path.dirname(dep_file_path), exist_ok=True)
        with open(dep_file_path, "w") as dep_file:
            dep_file.write(f"{main_entry}:\\\n")
            dep_file.write(
                "\\\n".join(sorted([f"    {p}" for p in self.visited_files]))
            )
