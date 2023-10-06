#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for GnLabel."""

from pathlib import Path

from gn_label import GnLabel
import unittest


class GnLabelTest(unittest.TestCase):
    def test_from_str(self):
        label = GnLabel.from_str("//path/to/foo")
        self.assertEqual(label.gn_str, "//path/to/foo")
        self.assertEqual(label.path, Path("path/to/foo"))
        self.assertEqual(label.name, "foo")
        self.assertIsNone(label.toolchain)

    def test_from_str_with_toolchain(self):
        toolchain = GnLabel.from_str("//some/toolchain")
        label = GnLabel.from_str("//path/to/foo(//some/toolchain)")
        self.assertEqual(label.gn_str, "//path/to/foo(//some/toolchain)")
        self.assertEqual(label.path, Path("path/to/foo"))
        self.assertEqual(label.name, "foo")
        self.assertEqual(label.toolchain, toolchain)

    def test_from_str_with_local_name(self):
        label = GnLabel.from_str("//path/to/foo:bar")
        self.assertEqual(label.gn_str, "//path/to/foo:bar")
        self.assertEqual(label.path, Path("path/to/foo"))
        self.assertEqual(label.name, "bar")
        self.assertIsNone(label.toolchain)

    def test_from_str_with_local_name_and_toolchain(self):
        toolchain = GnLabel.from_str("//some/toolchain")
        label = GnLabel.from_str("//path/to/foo:bar(//some/toolchain)")
        self.assertEqual(label.gn_str, "//path/to/foo:bar(//some/toolchain)")
        self.assertEqual(label.path, Path("path/to/foo"))
        self.assertEqual(label.name, "bar")
        self.assertEqual(label.toolchain, toolchain)

    def test_from_str_root_path(self):
        label = GnLabel.from_str("//")
        self.assertEqual(label.gn_str, "//")
        self.assertEqual(label.path, Path("."))
        self.assertEqual(label.name, "")
        self.assertIsNone(label.toolchain)

    def test_from_root_path(self):
        label = GnLabel.from_path(Path(""))
        self.assertEqual(label.gn_str, "//")
        self.assertEqual(label.path, Path("."))
        self.assertEqual(label.name, "")
        self.assertIsNone(label.toolchain)

    def test_from_path(self):
        label = GnLabel.from_path(Path("path/to/foo"))
        self.assertEqual(label.gn_str, "//path/to/foo")
        self.assertEqual(label.path, Path("path/to/foo"))
        self.assertEqual(label.name, "foo")
        self.assertIsNone(label.toolchain)

    def test_parent_label(self):
        label = GnLabel.from_path(Path("path/to/foo"))
        self.assertTrue(label.has_parent_label())
        self.assertEqual(label.parent_label(), GnLabel.from_str("//path/to"))

    def test_parent_label_of_root_label(self):
        label = GnLabel.from_str("//")
        self.assertEqual(label.gn_str, "//")
        self.assertFalse(label.has_parent_label())

        with self.assertRaises(AssertionError) as context:
            label.parent_label()
        self.assertEqual("// has no parent label", str(context.exception))

    def test_parent_label_of_label_created_from_path(self):
        label = GnLabel.from_path(Path("path/to/foo"))
        self.assertTrue(label.has_parent_label())
        self.assertEqual(label.parent_label(), GnLabel.from_str("//path/to"))

    def test_without_toolchain(self):
        label = GnLabel.from_str(
            "//path/to/foo(//some:toolchain)"
        ).without_toolchain()
        self.assertEqual(label, GnLabel.from_str("//path/to/foo"))

    def test_ensure_toolchain(self):
        toolchain = GnLabel.from_str("//some/toolchain")
        label = GnLabel.from_str("//path/to/foo").ensure_toolchain(toolchain)
        self.assertEqual(
            label, GnLabel.from_str("//path/to/foo(//some/toolchain)")
        )

    def test_ensure_toolchain_does_not_replace(self):
        toolchain = GnLabel.from_str("//some/toolchain")
        label = GnLabel.from_str(
            "//path/to/foo(//some/other/toolchain)"
        ).ensure_toolchain(toolchain)
        self.assertEqual(
            label, GnLabel.from_str("//path/to/foo(//some/other/toolchain)")
        )

    def test_rebase_package_path(self):
        path = GnLabel.from_str("//path/to/foo").rebased_path(
            Path("rebase/dir")
        )
        self.assertEqual(path, Path("rebase/dir/path/to/foo"))

    def test_code_search_url(self):
        url = GnLabel.from_str("//path/to/foo:bar").code_search_url()
        self.assertEqual(
            url,
            "https://cs.opensource.google/fuchsia/fuchsia/+/main:path/to/foo",
        )

    def test_is_host_target(self):
        def is_host_target(s):
            return GnLabel.from_str(s).is_host_target()

        self.assertFalse(is_host_target("//host_:foo"))
        self.assertFalse(is_host_target("//foo:host_"))
        self.assertTrue(is_host_target("//foo:bar(//host_baz)"))
        self.assertFalse(is_host_target("//foo:bar(//not_host)"))

    def test_is_third_party(self):
        def is_3p(s):
            return GnLabel.from_str(s).is_3rd_party()

        self.assertFalse(is_3p("//foo:bar"))
        self.assertTrue(is_3p("//third_party/foo"))
        self.assertTrue(is_3p("//foo:third_party"))
        self.assertTrue(is_3p("//foo/third_party/bar"))

    def test_is_3p_rust_crate(self):
        self.assertFalse(GnLabel.from_str("//foo:bar").is_3p_rust_crate())
        self.assertTrue(
            GnLabel.from_str("//third_party/rust_crates:foo").is_3p_rust_crate()
        )

    def test_is_3p_golib(self):
        self.assertFalse(GnLabel.from_str("//foo:bar").is_3p_golib())
        self.assertTrue(
            GnLabel.from_str("//third_party/golibs:foo").is_3p_golib()
        )

    def test_create_child_from_str(self):
        parent = GnLabel.from_str("//path1/to/foo:bar(//toolchain)")
        child = parent.create_child_from_str("path2/to/child:baz")
        self.assertEqual(
            child, GnLabel.from_str("//path1/to/foo/path2/to/child:baz")
        )

    def test_create_child_from_absolute_str(self):
        parent = GnLabel.from_str("//path1/to/foo:bar(//toolchain)")
        child = parent.create_child_from_str("//path2/to/child:baz")
        self.assertEqual(child, GnLabel.from_str("//path2/to/child:baz"))

    def test_create_child_from_str_with_dot_dot(self):
        parent = GnLabel.from_str("//path/to/foo")
        self.assertEqual(parent.parent_label(), GnLabel.from_str("//path/to"))

        self.assertEqual(
            parent.create_child_from_str("../baz:qux"),
            GnLabel.from_str("//path/to/baz:qux"),
        )

        self.assertEqual(
            parent.create_child_from_str("../../baz:qux"),
            GnLabel.from_str("//path/baz:qux"),
        )

        self.assertEqual(
            parent.create_child_from_str("../../../baz:qux"),
            GnLabel.from_str("//baz:qux"),
        )

        # .. in the middle of the path is not supported
        with self.assertRaises(AssertionError) as context:
            parent.create_child_from_str("foo/../baz:qux")
        self.assertEqual(
            ".. is not supported but got //path/to/foo/foo/../baz:qux",
            str(context.exception),
        )

        # Going back beyond the root path is not supported
        with self.assertRaises(AssertionError) as context:
            parent.create_child_from_str("../../../../baz:qux")
        self.assertEqual("Can't apply ../baz:qux to //", str(context.exception))

    def test_create_child_from_str_with_dot_dot_with_local_name(self):
        parent = GnLabel.from_str("//path/to/foo:bar")
        self.assertEqual(
            parent.parent_label(), GnLabel.from_str("//path/to/foo")
        )

        self.assertEqual(
            parent.create_child_from_str("../baz:qux"),
            GnLabel.from_str("//path/to/baz:qux"),
        )

        self.assertEqual(
            parent.create_child_from_str("../../baz:qux"),
            GnLabel.from_str("//path/baz:qux"),
        )

        self.assertEqual(
            parent.create_child_from_str("../../../baz:qux"),
            GnLabel.from_str("//baz:qux"),
        )

        # .. in the middle of the path is not supported
        with self.assertRaises(AssertionError) as context:
            parent.create_child_from_str("foo/../baz:qux")
        self.assertEqual(
            ".. is not supported but got //path/to/foo/foo/../baz:qux",
            str(context.exception),
        )

        # Going back beyond the root path is not supported
        with self.assertRaises(AssertionError) as context:
            parent.create_child_from_str("../../../../baz:qux")
        self.assertEqual("Can't apply ../baz:qux to //", str(context.exception))

    def test_gt(self):
        # Testing greater_than indirectly by sorting
        sorted_labels = sorted(
            [
                GnLabel.from_str("//path2"),
                GnLabel.from_str("//path3:foo2"),
                GnLabel.from_str("//path3:foo1"),
                GnLabel.from_str("//path1"),
            ]
        )
        self.assertListEqual(
            sorted_labels,
            [
                GnLabel.from_str("//path1"),
                GnLabel.from_str("//path2"),
                GnLabel.from_str("//path3:foo1"),
                GnLabel.from_str("//path3:foo2"),
            ],
        )


if __name__ == "__main__":
    unittest.main()
