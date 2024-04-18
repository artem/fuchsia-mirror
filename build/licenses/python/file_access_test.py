#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for FileAccess."""


from file_access import FileAccess
from gn_label import GnLabel
from pathlib import Path
import os
import tempfile
import unittest


class FileAccessTest(unittest.TestCase):
    temp_dir: tempfile.TemporaryDirectory
    temp_dir_path: Path
    file_access: FileAccess

    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.temp_dir_path = Path(self.temp_dir.name)
        self.file_access = FileAccess(
            fuchsia_source_path_str=self.temp_dir.name
        )

        file_path1 = self.temp_dir_path / "foo"
        file_path1.write_text("FOO")

        file_path2 = self.temp_dir_path / "bar"
        file_path2.write_text("BAR")

        (self.temp_dir_path / "child").mkdir()
        file_path3 = self.temp_dir_path / "child" / "baz"
        file_path3.write_text("BAZ")

        return super().setUp()

    def tearDown(self) -> None:
        self.temp_dir.cleanup()
        return super().tearDown()

    def test_file_exists(self):
        self.assertTrue(self.file_access.file_exists(GnLabel.from_str("//foo")))
        self.assertTrue(self.file_access.file_exists(GnLabel.from_str("//bar")))
        self.assertFalse(
            self.file_access.file_exists(GnLabel.from_str("//baz"))
        )

    def test_directory_exists(self):
        self.assertTrue(
            self.file_access.directory_exists(GnLabel.from_str("//"))
        )
        self.assertFalse(
            self.file_access.file_exists(GnLabel.from_str("//baz"))
        )

    def test_search_directory(self):
        children = self.file_access.search_directory(
            GnLabel.from_str("//"), path_predicate=lambda _: True
        )
        children.sort()
        self.assertEqual(
            children,
            [
                GnLabel.from_str("//bar"),
                GnLabel.from_str("//child/baz"),
                GnLabel.from_str("//foo"),
            ],
        )

    def test_search_directory_with_predicate(self):
        children = self.file_access.search_directory(
            GnLabel.from_str("//"),
            path_predicate=lambda path: os.path.basename(path) == "bar",
        )
        children.sort()
        self.assertEqual(children, [GnLabel.from_str("//bar")])

    def test_read_file(self):
        self.assertEqual(
            self.file_access.read_text(GnLabel.from_str("//foo")), "FOO"
        )
        self.assertEqual(
            self.file_access.read_text(GnLabel.from_str("//bar")), "BAR"
        )

    def _assert_depfile(self, expected_content):
        depfile_path = self.temp_dir_path / "depfile"
        self.file_access.write_depfile(
            dep_file_path=depfile_path, main_entry="main"
        )
        actual_depfile_contents = depfile_path.read_text()
        self.assertEqual(actual_depfile_contents, expected_content)

    def test_write_depfile_after_read_text(self):
        self.file_access.read_text(GnLabel.from_str("//foo"))
        self._assert_depfile(f"""main:\\\n    {self.temp_dir_path}/foo""")

    def test_write_depfile_after_file_exists(self):
        self.file_access.file_exists(GnLabel.from_str("//bar"))
        self._assert_depfile(f"""main:\\\n    {self.temp_dir_path}/bar""")

    def test_write_depfile_after_directory_exists(self):
        self.file_access.directory_exists(GnLabel.from_str("//"))
        self._assert_depfile(f"""main:\\\n    {self.temp_dir_path}/""")

    def test_write_depfile_after_search_directory(self):
        self.file_access.search_directory(
            GnLabel.from_str("//"), path_predicate=lambda _: True
        )
        self._assert_depfile(f"""main:\\\n    {self.temp_dir_path}/""")


if __name__ == "__main__":
    unittest.main()
