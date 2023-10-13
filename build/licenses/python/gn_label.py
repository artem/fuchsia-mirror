#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Utilities for working with GnLabel strings."""

import dataclasses
from pathlib import Path
from typing import List


@dataclasses.dataclass(frozen=True)
class GnLabel:
    """Utility for handling Gn target labels"""

    """The original GN label string, e.g. `//foo/bar:baz(//toolchain)`"""
    gn_str: str
    """The path part of the label, e.g. `foo/bar` in `//foo/bar:baz`"""
    path: Path = dataclasses.field(hash=False, compare=False)
    """The name part of the label, e.g. `baz` in `//foo/bar:baz` or `bar` for `//foo/bar`"""
    name: str = dataclasses.field(hash=False, compare=False)
    """Whether the label has a local name, e.g. True for `//foo/bar:baz` but false for `//foo/bar/baz`"""
    is_local_name: bool = dataclasses.field(hash=False, compare=False)
    """The toolchain part of the label, e.g. `//toolchain` in `//foo/bar(//toolchain)`"""
    toolchain: "GnLabel" = dataclasses.field(hash=False, compare=False)

    def from_str(original_str: str) -> "GnLabel":
        """Constructs a GnLabel instance from a GN target label string"""
        assert original_str.startswith(
            "//"
        ), f"label must start with // but got {original_str}"

        label = original_str
        toolchain = None
        # Extract toolchain part
        if original_str.endswith(")"):
            toolchain_begin = original_str.rfind("(")
            if toolchain_begin != -1:
                toolchain_str = original_str[toolchain_begin + 1 : -1]
                toolchain = GnLabel.from_str(toolchain_str)
                label = original_str[0:toolchain_begin]

        path_and_name = label.split(":", maxsplit=1)

        path_str = path_and_name[0][2:]  # remove '//'
        if ".." in path_str:
            path_str = GnLabel._resolve_dot_dot(path_str)
        path = Path(path_str)

        if len(path_and_name) == 2:
            name = path_and_name[1]
            is_local_name = True
        else:
            assert len(path_and_name) == 1
            name = str(path.name)
            is_local_name = False

        gn_str = ["//", path_str]
        if is_local_name:
            gn_str.extend([":", name])
        if toolchain:
            gn_str.extend(["(", toolchain.gn_str, ")"])
        gn_str = "".join(gn_str)

        return GnLabel(
            gn_str=gn_str,
            name=name,
            is_local_name=is_local_name,
            path=path,
            toolchain=toolchain,
        )

    def from_path(path: Path):
        """Constructs a GnLabel instance from a Path object."""
        assert isinstance(
            path, Path
        ), f"Expected path of type Path but got {type(path)}"
        if path.name == "" and path.parent.name == "":
            return GnLabel.from_str("//")
        return GnLabel.from_str(f"//{path}")

    def check_type(other) -> "GnLabel":
        """Asserts that `other` is of type GnLabel"""
        assert isinstance(
            other, GnLabel
        ), f"{other} type {type(other)} is not {GnLabel}"
        return other

    def check_types_in_list(list: List["GnLabel"]) -> List["GnLabel"]:
        """Asserts that all values in `list` are of type GnLabel"""
        for v in list:
            GnLabel.check_type(v)
        return list

    def parent_label(self) -> "GnLabel":
        """Returns //foo/bar for //foo/bar/baz and //foo/bar:baz"""
        assert self.has_parent_label(), f"{self} has no parent label"
        if not self.is_local_name and self.name == self.path.name:
            # Return //foo for //foo/bar
            return GnLabel.from_path(self.path.parent)
        else:
            # Return //foo/bar for //foo/bar:bar and //foo/bar:baz
            return GnLabel.from_path(self.path)

    def has_parent_label(self) -> bool:
        return self.gn_str != "//"

    def without_toolchain(self) -> "GnLabel":
        """Removes the toolchain part of the label"""
        if self.toolchain:
            return GnLabel.from_str(self.gn_str.split("(", maxsplit=1)[0])
        else:
            return self

    def ensure_toolchain(self, toolchain: "GnLabel") -> "GnLabel":
        """Sets a toolchain if there is none already"""
        if self.toolchain:
            return self
        else:
            return dataclasses.replace(
                self, toolchain=toolchain, gn_str=f"{self.gn_str}({toolchain})"
            )

    def rebased_path(self, base_dir: Path) -> Path:
        """Returns package_path rebased to a given base_dir."""
        return base_dir / self.path

    def code_search_url(self) -> str:
        """Returns package_path rebased to a given base_dir."""
        path = self.path
        return f"https://cs.opensource.google/fuchsia/fuchsia/+/main:{path}"

    def is_host_target(self) -> bool:
        return self.toolchain and self.toolchain.name.startswith("host_")

    def is_3rd_party(self) -> bool:
        return "third_party" in self.gn_str

    def is_3p_rust_crate(self) -> bool:
        return self.gn_str.startswith("//third_party/rust_crates:")

    def is_3p_golib(self) -> bool:
        return self.gn_str.startswith("//third_party/golibs:")

    def create_child_from_str(self, child_path_str: str) -> "GnLabel":
        """Create a GnLabel relative to this label from a child path GN string"""
        if child_path_str.startswith("//"):
            return GnLabel.from_str(child_path_str)
        elif child_path_str.startswith(":"):
            assert (
                not self.is_local_name
            ), f"Can't apply {child_path_str} to {self} because both have :"
            return GnLabel.from_str(f"//{self.path}:{child_path_str[1:]}")
        elif child_path_str.startswith("../"):
            parent_path = (
                self.parent_label().path if self.is_local_name else self.path
            )
            return GnLabel.from_path(parent_path / Path(child_path_str))
        else:
            return GnLabel.from_path(self.path / Path(child_path_str))

    def __str__(self) -> str:
        return self.gn_str

    def __repr__(self) -> str:
        return f"{self.__class__.__name__}({self.gn_str})"

    def __gt__(self, other: "GnLabel") -> bool:
        return self.gn_str > other.gn_str

    def _resolve_dot_dot(path: str) -> str:
        """Resolves .. elements in a path string"""
        assert "//" not in path
        input_parts = path.split("/")
        output_parts = []
        for part in input_parts:
            if part != "..":
                output_parts.append(part)
            else:
                assert (
                    len(output_parts) > 0
                ), f".. goes back beyond the base path: {path}"
                output_parts.pop()
        return "/".join(output_parts)
