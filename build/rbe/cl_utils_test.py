#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import contextlib
import filecmp
import io
import multiprocessing
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import cl_utils


class ImmediateExit(Exception):
    """Mocked calls that are not expected to return can raise this.

    Examples: os.exec*(), sys.exit()
    """

    pass


class AutoEnvPrefixCommandTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(cl_utils.auto_env_prefix_command([]), [])

    def test_no_prefix(self):
        self.assertEqual(cl_utils.auto_env_prefix_command(["echo"]), ["echo"])

    def test_env_looking_arg(self):
        self.assertEqual(
            cl_utils.auto_env_prefix_command(["echo", "BAR=FOO"]),
            ["echo", "BAR=FOO"],
        )

    def test_need_prefix(self):
        self.assertEqual(
            cl_utils.auto_env_prefix_command(["FOO=BAR", "echo"]),
            [cl_utils._ENV, "FOO=BAR", "echo"],
        )


class TimerCMTests(unittest.TestCase):
    @mock.patch("cl_utils._ENABLE_TIMERS", True)
    def test_basic(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            with cl_utils.timer_cm("descriptive text"):
                pass
        lines = output.getvalue().splitlines()
        self.assertIn("start: descriptive text", lines[0])
        self.assertIn("end  : descriptive text", lines[1])


class BoolGolangFlagTests(unittest.TestCase):
    def test_true(self):
        for v in ("1", "t", "T", "true", "True", "TRUE"):
            self.assertTrue(cl_utils.bool_golang_flag(v))

    def test_false(self):
        for v in ("0", "f", "F", "false", "False", "FALSE"):
            self.assertFalse(cl_utils.bool_golang_flag(v))

    def test_invalid(self):
        for v in ("", "maybe", "true-ish", "false-y"):
            with self.assertRaises(KeyError):
                cl_utils.bool_golang_flag(v)


class CopyPreserveSubpathTests(unittest.TestCase):
    def test_subpath(self):
        with tempfile.TemporaryDirectory() as td1:
            tdp1 = Path(td1)
            dest_dir = tdp1 / "backups"
            with cl_utils.chdir_cm(tdp1):  # working directory
                srcdir = Path("aa/bb")
                srcdir.mkdir(parents=True, exist_ok=True)
                src_file = srcdir / "c.txt"
                src_file.write_text("hello\n")
                cl_utils.copy_preserve_subpath(src_file, dest_dir)
                dest_file = dest_dir / src_file
                self.assertTrue(filecmp.cmp(src_file, dest_file, shallow=False))

    def test_do_not_recopy_if_identical(self):
        with tempfile.TemporaryDirectory() as td1:
            tdp1 = Path(td1)
            dest_dir = tdp1 / "backups"
            with cl_utils.chdir_cm(tdp1):  # working directory
                srcdir = Path("aa/bb")
                srcdir.mkdir(parents=True, exist_ok=True)
                src_file = srcdir / "c.txt"
                src_file.write_text("hello\n")
                cl_utils.copy_preserve_subpath(src_file, dest_dir)
                dest_file = dest_dir / src_file
                self.assertTrue(filecmp.cmp(src_file, dest_file, shallow=False))

                # Attempting to copy over identical file should be suppressed.
                with mock.patch.object(shutil, "copy2") as mock_copy:
                    cl_utils.copy_preserve_subpath(src_file, dest_dir)
                mock_copy.assert_not_called()

    def test_do_not_copy_overwrite_if_different(self):
        with tempfile.TemporaryDirectory() as td1:
            tdp1 = Path(td1)
            dest_dir = tdp1 / "backups"
            with cl_utils.chdir_cm(tdp1):  # working directory
                srcdir = Path("aa/bb")
                srcdir.mkdir(parents=True, exist_ok=True)
                src_file = srcdir / "c.txt"
                src_file.write_text("hello\n")
                cl_utils.copy_preserve_subpath(src_file, dest_dir)
                dest_file = dest_dir / src_file
                self.assertTrue(filecmp.cmp(src_file, dest_file, shallow=False))

                # Attempting to copy over different file is suppressed
                src_file.write_text("not hello\n")
                with mock.patch.object(shutil, "copy2") as mock_copy:
                    cl_utils.copy_preserve_subpath(src_file, dest_dir)
                mock_copy.assert_not_called()


class PartitionSequenceTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(
            cl_utils.partition_sequence([], 28),
            ([], None, []),
        )

    def test_int_not_found(self):
        seq = [5, 234, 1, 9]
        self.assertEqual(
            cl_utils.partition_sequence(seq, 28),
            (seq, None, []),
        )

    def test_sep_found_at_beginning(self):
        left = []
        sep = "z"
        right = ["x", "y"]
        seq = left + [sep] + right
        self.assertEqual(
            cl_utils.partition_sequence(seq, sep),
            (left, sep, right),
        )

    def test_sep_found_in_middle(self):
        left = ["12", "34"]
        sep = "zz"
        right = ["23", "asdf"]
        seq = left + [sep] + right
        self.assertEqual(
            cl_utils.partition_sequence(seq, sep),
            (left, sep, right),
        )

    def test_sep_found_at_end(self):
        left = ["12", "34", "qw", "er"]
        sep = "yy"
        right = []
        seq = left + [sep] + right
        self.assertEqual(
            cl_utils.partition_sequence(seq, sep),
            (left, sep, right),
        )


class SplitIntoSubequencesTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(
            list(cl_utils.split_into_subsequences([], None)),
            [[]],
        )

    def test_only_separators(self):
        sep = ":"
        self.assertEqual(
            list(cl_utils.split_into_subsequences([sep] * 4, sep)),
            [[]] * 5,
        )

    def test_no_match_separators(self):
        seq = ["a", "b", "c", "d", "e"]
        sep = "%"
        self.assertEqual(
            list(cl_utils.split_into_subsequences(seq, sep)),
            [seq],
        )

    def test_different_size_slices(self):
        seq = ["a", "b", "%", "c", "%", "d", "e", "f"]
        sep = "%"
        self.assertEqual(
            list(cl_utils.split_into_subsequences(seq, sep)),
            [
                ["a", "b"],
                ["c"],
                ["d", "e", "f"],
            ],
        )


class MatchPrefixTransformSuffixTests(unittest.TestCase):
    def test_no_match(self):
        result = cl_utils.match_prefix_transform_suffix(
            "abc", "xyz", lambda x: x
        )
        self.assertIsNone(result)

    def test_match(self):
        result = cl_utils.match_prefix_transform_suffix(
            "abcdef", "abc", lambda x: x.upper()
        )
        self.assertEqual(result, "abcDEF")


class FlattenCommaListTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list([])),
            [],
        )

    def test_singleton(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(["qwe"])),
            ["qwe"],
        )

    def test_one_comma(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(["qw,er"])),
            ["qw", "er"],
        )

    def test_two_items(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(["as", "df"])),
            ["as", "df"],
        )

    def test_multiple_items_with_commas(self):
        self.assertEqual(
            list(cl_utils.flatten_comma_list(["as,12", "df", "zx,cv,bn"])),
            ["as", "12", "df", "zx", "cv", "bn"],
        )


class RemoveHashCommentsTests(unittest.TestCase):
    def test_empty_line(self):
        self.assertEqual(list(cl_utils.remove_hash_comments([""])), [""])

    def test_newline(self):
        self.assertEqual(list(cl_utils.remove_hash_comments(["\n"])), ["\n"])

    def test_comments(self):
        self.assertEqual(list(cl_utils.remove_hash_comments(["#"])), [])
        self.assertEqual(list(cl_utils.remove_hash_comments(["##"])), [])
        self.assertEqual(list(cl_utils.remove_hash_comments(["# comment"])), [])

    def test_mixed(self):
        self.assertEqual(
            list(
                cl_utils.remove_hash_comments(
                    ["#!/she/bang", "--foo", "", "# BAR section", "--bar=baz"]
                )
            ),
            ["--foo", "", "--bar=baz"],
        )


class StringSetArgparseActionTests(unittest.TestCase):
    def _parser(self) -> argparse.ArgumentParser:
        parser = argparse.ArgumentParser(
            description="For testing", add_help=False
        )
        parser.add_argument(
            "--add",
            type=str,
            dest="strset",
            default=set(),
            action=cl_utils.StringSetAdd,
            help="add to set",
        )
        parser.add_argument(
            "--remove",
            type=str,
            dest="strset",
            action=cl_utils.StringSetRemove,
            help="remove from set",
        )
        return parser

    def test_set_default(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args([])
        self.assertEqual(attrs.strset, set())

    def test_set_add(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args(["--add", "AAA"])
        self.assertEqual(attrs.strset, {"AAA"})

    def test_set_add_dupe(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args(
            ["--add", "AAA", "--add", "AAA"]
        )
        self.assertEqual(attrs.strset, {"AAA"})

    def test_set_add_different(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args(
            ["--add", "AAA", "--add", "YYY"]
        )
        self.assertEqual(attrs.strset, {"AAA", "YYY"})

    def test_set_remove_nonexisting(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args(["--remove", "BBB"])
        self.assertEqual(attrs.strset, set())

    def test_set_add_then_remove(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args(
            ["--add", "DDD", "--remove", "DDD"]
        )
        self.assertEqual(attrs.strset, set())

    def test_set_remove_then_add(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args(
            ["--remove", "EEE", "--add", "EEE"]
        )
        self.assertEqual(attrs.strset, {"EEE"})

    def test_set_remove_all(self):
        parser = self._parser()
        (attrs, others) = parser.parse_known_args(
            ["--add", "AAA", "--add", "YYY", "--remove=all"]
        )
        self.assertEqual(attrs.strset, set())


class ExpandResponseFilesTests(unittest.TestCase):
    def test_no_rspfiles(self):
        command = ["sed", "-e", "s|foo|bar|"]
        rspfiles = []
        self.assertEqual(
            list(cl_utils.expand_response_files(command, rspfiles)), command
        )
        self.assertEqual(rspfiles, [])

    def test_space_only_rspfile(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp = tdp / "args.rsp"
            rsp.write_text(" \n")
            command = ["tool.sh", f"@{rsp}", "-o", "space.out"]
            rspfiles = []
            self.assertEqual(
                list(cl_utils.expand_response_files(command, rspfiles)),
                ["tool.sh", "-o", "space.out"],
            )
            self.assertEqual(rspfiles, [rsp])

    def test_blank_line_rspfile(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp = tdp / "args.rsp"
            rsp.write_text("\n")
            command = ["tool.sh", f"@{rsp}", "-o", "blank.out"]
            rspfiles = []
            self.assertEqual(
                list(cl_utils.expand_response_files(command, rspfiles)),
                ["tool.sh", "-o", "blank.out"],
            )
            self.assertEqual(rspfiles, [rsp])

    def test_one_rspfile(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp = tdp / "args.rsp"
            rsp.write_text("12\n\n34\n56\n")
            command = ["tool.sh", f"@{rsp}", "-o", "cmd.out"]
            rspfiles = []
            self.assertEqual(
                list(cl_utils.expand_response_files(command, rspfiles)),
                ["tool.sh", "12", "34", "56", "-o", "cmd.out"],
            )
            self.assertEqual(rspfiles, [rsp])

    def test_rspfile_rustc_alternative_syntax(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp = tdp / "args.rsp"
            rsp.write_text("56\n78\n")
            command = ["tool.sh", f"@shell:{rsp}", "-o", "cmd4.out"]
            rspfiles = []
            self.assertEqual(
                list(cl_utils.expand_response_files(command, rspfiles)),
                ["tool.sh", "56", "78", "-o", "cmd4.out"],
            )
            self.assertEqual(rspfiles, [rsp])

    def test_nested_repeated_rspfiles(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp1 = tdp / "args1.rsp"
            rsp2 = tdp / "args2.rsp"
            rsp1.write_text(f"@{rsp2}\nand\n@{rsp2}")
            rsp2.write_text("fee\n#comment\nfigh\n")
            command = ["tool.sh", f"@{rsp1}"]
            rspfiles = []
            self.assertEqual(
                list(cl_utils.expand_response_files(command, rspfiles)),
                ["tool.sh", "fee", "figh", "and", "fee", "figh"],
            )
            self.assertEqual(set(rspfiles), {rsp1, rsp2})

    def test_multitoken_lines(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp = tdp / "args.rsp"
            rsp.write_text(" a  b \nc    d  \n")
            command = ["tool.sh", f"@{rsp}", "-o", "space.out"]
            rspfiles = []
            self.assertEqual(
                list(cl_utils.expand_response_files(command, rspfiles)),
                ["tool.sh", "a", "b", "c", "d", "-o", "space.out"],
            )
            self.assertEqual(rspfiles, [rsp])


class ExpandFusedFlagsTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags([], ["-Z"])),
            [],
        )

    def test_no_expand(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags(["-Yfoo"], ["-Z"])),
            ["-Yfoo"],
        )

    def test_expand_one(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags(["-Yfoo"], ["-Y"])),
            ["-Y", "foo"],
        )

    def test_expand_multiple(self):
        self.assertEqual(
            list(
                cl_utils.expand_fused_flags(
                    ["-Xxx", "-Yfog", "-Dbar"], {"-Y", "-X"}
                )
            ),
            ["-X", "xx", "-Y", "fog", "-Dbar"],
        )

    def test_already_expanded(self):
        self.assertEqual(
            list(cl_utils.expand_fused_flags(["-Y", "foo"], ["-Y"])),
            ["-Y", "foo"],
        )

    def test_expand_repeated(self):
        self.assertEqual(
            list(
                cl_utils.expand_fused_flags(
                    ["-Yfoo=f", "other", "-Ybar=g"], ["-Y"]
                )
            ),
            ["-Y", "foo=f", "other", "-Y", "bar=g"],
        )


class FuseExpandedFlagsTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags([], {"-Z"})),
            [],
        )

    def test_no_fuse(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags(["-Y", "foo"], {"-Z"})),
            ["-Y", "foo"],
        )

    def test_fuse_one(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags(["-Y", "foo"], {"-Y"})),
            ["-Yfoo"],
        )

    def test_already_fused(self):
        self.assertEqual(
            list(cl_utils.fuse_expanded_flags(["-Wfoo"], {"-W"})),
            ["-Wfoo"],
        )

    def test_fuse_repeated(self):
        self.assertEqual(
            list(
                cl_utils.fuse_expanded_flags(
                    ["-W", "zoo", "blah", "-W", "woof"], {"-W"}
                )
            ),
            ["-Wzoo", "blah", "-Wwoof"],
        )


class KeyedFlagsToValuesDictTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict([]),
            dict(),
        )

    def test_key_no_value(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(["a", "z"]),
            {
                "a": [],
                "z": [],
            },
        )

    def test_blank_string_values(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(["b=", "b=", "e="]),
            {
                "b": ["", ""],
                "e": [""],
            },
        )

    def test_no_repeat_keys(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(["a=b", "c=d"]),
            {
                "a": ["b"],
                "c": ["d"],
            },
        )

    def test_repeat_keys(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(["a=b", "c=d", "a=b", "c=e"]),
            {
                "a": ["b", "b"],
                "c": ["d", "e"],
            },
        )

    def test_convert_values_to_int(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(
                ["a=7", "c=8"], convert_type=int
            ),
            {
                "a": [7],
                "c": [8],
            },
        )

    def test_convert_values_to_path(self):
        self.assertEqual(
            cl_utils.keyed_flags_to_values_dict(
                ["a=/foo/bar", "c=bar/foo.quux"], convert_type=Path
            ),
            {
                "a": [Path("/foo/bar")],
                "c": [Path("bar/foo.quux")],
            },
        )


class LastValueOrDefaultTests(unittest.TestCase):
    def test_default(self):
        self.assertEqual(
            cl_utils.last_value_or_default([], "3"),
            "3",
        )

    def test_last_value(self):
        self.assertEqual(
            cl_utils.last_value_or_default(["1", "2", "5", "6"], "4"),
            "6",
        )


class LastValueOfDictFlagTests(unittest.TestCase):
    def test_default_no_key(self):
        self.assertEqual(
            cl_utils.last_value_of_dict_flag(
                {"f": ["g", "h"], "p": []}, "z", "default"
            ),
            "default",
        )

    def test_default_empty_values(self):
        self.assertEqual(
            cl_utils.last_value_of_dict_flag(
                {"f": ["g", "h"], "p": []}, "p", "boring"
            ),
            "boring",
        )

    def test_last_value(self):
        self.assertEqual(
            cl_utils.last_value_of_dict_flag(
                {"f": ["g", "h"], "p": []}, "f", "boring"
            ),
            "h",
        )


class ExpandPathsFromFilesTests(unittest.TestCase):
    def test_basic(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            paths1 = ["foo/bar1.txt", "bar/foo1.txt"]
            paths2 = ["foo/bar2.txt", "bar/foo2.txt"]
            list1 = tdp / "list1.rsp"
            list2 = tdp / "list2.rsp"
            list1.write_text("\n".join(paths1) + "\n")
            list2.write_text("\n".join(paths2) + "\n")
            all_paths = list(cl_utils.expand_paths_from_files([list1, list2]))
            self.assertEqual(all_paths, [Path(p) for p in paths1 + paths2])

    def test_escaped_spaces(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            paths1 = ["foo\\ spacey1.txt", "bar\\ spacer.txt"]
            paths2 = ["foo\\ spacey2.txt", "bar\\ spaced.txt"]
            list1 = tdp / "list1.rsp"
            list2 = tdp / "list2.rsp"
            list1.write_text("\n".join(paths1) + "\n")
            list2.write_text("\n".join(paths2) + "\n")
            all_paths = list(cl_utils.expand_paths_from_files([list1, list2]))
            self.assertEqual(all_paths, [Path(p) for p in paths1 + paths2])

    def test_multiple_paths_per_line(self):
        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            paths1 = ["foo/bar1.txt", "bar/foo1.txt"]
            paths2 = ["foo/bar2.txt", "bar/foo2.txt"]
            list1 = tdp / "list1.rsp"
            list2 = tdp / "list2.rsp"
            list1.write_text(" ".join(paths1) + "\n")
            list2.write_text(" ".join(paths2) + "\n")
            all_paths = list(cl_utils.expand_paths_from_files([list1, list2]))
            self.assertEqual(all_paths, [Path(p) for p in paths1 + paths2])


class FilterOutOptionWithArgTests(unittest.TestCase):
    def test_no_change(self):
        actual = list(
            cl_utils.filter_out_option_with_arg(
                ["keep", "--all"], "--delete-me"
            )
        )
        self.assertEqual(actual, ["keep", "--all"])

    def test_remove_fused_optarg(self):
        actual = list(
            cl_utils.filter_out_option_with_arg(
                ["sleep", "--delete-me=--all", "foo"], "--delete-me"
            )
        )
        self.assertEqual(actual, ["sleep", "foo"])

    def test_remove_separate_optarg(self):
        actual = list(
            cl_utils.filter_out_option_with_arg(
                ["creep", "--erase-me", "--foo=baz", "--bar"], "--erase-me"
            )
        )
        self.assertEqual(actual, ["creep", "--bar"])


class StripOptionPrefixTests(unittest.TestCase):
    def test_no_change(self):
        actual = list(
            cl_utils.strip_option_prefix(["keep", "--all"], "--keep-me")
        )
        self.assertEqual(actual, ["keep", "--all"])

    def test_remove_fused_optarg(self):
        actual = list(
            cl_utils.strip_option_prefix(
                ["sleep", "--keep-me=--all", "foo"], "--keep-me"
            )
        )
        self.assertEqual(actual, ["sleep", "--all", "foo"])

    def test_remove_separate_optarg(self):
        actual = list(
            cl_utils.strip_option_prefix(
                ["creep", "--keep-me", "--foo=baz", "--bar"], "--keep-me"
            )
        )
        self.assertEqual(actual, ["creep", "--foo=baz", "--bar"])


class FlagForwarderTests(unittest.TestCase):
    def test_no_transform(self):
        f = cl_utils.FlagForwarder([])
        command = ["a", "b", "-c", "d", "--e", "f", "--g=h"]
        forwarded, filtered = f.sift(command)
        self.assertEqual(forwarded, [])
        self.assertEqual(filtered, command)

    def test_renamed_no_optarg(self):
        f = cl_utils.FlagForwarder(
            [
                cl_utils.ForwardedFlag(
                    name="--old", has_optarg=False, mapped_name="--new"
                )
            ]
        )
        command = ["a", "b", "--old", "d", "--old", "f", "--g=h"]
        forwarded, filtered = f.sift(command)
        self.assertEqual(forwarded, ["--new", "--new"])
        self.assertEqual(filtered, ["a", "b", "d", "f", "--g=h"])

    def test_renamed_with_optarg(self):
        f = cl_utils.FlagForwarder(
            [
                cl_utils.ForwardedFlag(
                    name="--old", has_optarg=True, mapped_name="--new"
                )
            ]
        )
        command = ["a", "b", "--old", "d", "--old=f", "--g=h"]
        forwarded, filtered = f.sift(command)
        self.assertEqual(forwarded, ["--new", "d", "--new=f"])
        self.assertEqual(filtered, ["a", "b", "--g=h"])

    def test_deleted_no_optarg(self):
        f = cl_utils.FlagForwarder(
            [
                cl_utils.ForwardedFlag(
                    name="--old", has_optarg=False, mapped_name=""
                )
            ]
        )
        command = ["a", "b", "--old", "d", "--old", "f", "--g=h"]
        forwarded, filtered = f.sift(command)
        self.assertEqual(forwarded, [])
        self.assertEqual(filtered, ["a", "b", "d", "f", "--g=h"])

    def test_deleted_with_optarg(self):
        f = cl_utils.FlagForwarder(
            [
                cl_utils.ForwardedFlag(
                    name="--old", has_optarg=True, mapped_name=""
                )
            ]
        )
        command = ["a", "b", "--old", "--eek", "--old=-f=z", "--g=h"]
        forwarded, filtered = f.sift(command)
        self.assertEqual(forwarded, ["--eek", "-f=z"])
        self.assertEqual(filtered, ["a", "b", "--g=h"])

    def test_multiple_transforms(self):
        f = cl_utils.FlagForwarder(
            [
                cl_utils.ForwardedFlag(
                    name="--bad", has_optarg=True, mapped_name="--ugly"
                ),
                cl_utils.ForwardedFlag(
                    name="--old", has_optarg=True, mapped_name=""
                ),
            ]
        )
        command = ["a", "b", "--old", "d", "--bad=f", "--g=h"]
        forwarded, filtered = f.sift(command)
        self.assertEqual(forwarded, ["d", "--ugly=f"])
        self.assertEqual(filtered, ["a", "b", "--g=h"])


class RelpathTests(unittest.TestCase):
    def test_identity(self):
        self.assertEqual(cl_utils.relpath(Path("a"), Path("a")), Path("."))

    def test_sibling(self):
        self.assertEqual(cl_utils.relpath(Path("a"), Path("b")), Path("../a"))

    def test_ancestor(self):
        self.assertEqual(cl_utils.relpath(Path("a"), Path("a/c")), Path(".."))

    def test_subdir(self):
        self.assertEqual(cl_utils.relpath(Path("a/d"), Path("a")), Path("d"))

    def test_distant(self):
        self.assertEqual(
            cl_utils.relpath(Path("a/b/c"), Path("x/y/z")),
            Path("../../../a/b/c"),
        )

    def test_common_parent(self):
        self.assertEqual(
            cl_utils.relpath(Path("a/b/c"), Path("a/y/z")), Path("../../b/c")
        )


def _readlink(path: Path) -> str:
    # Path.readlink() is only available in Python 3.9+
    return os.readlink(str(path))


class SymlinkRelativeTests(unittest.TestCase):
    def test_same_dir(self):
        with tempfile.TemporaryDirectory() as td:
            dest = Path(td) / "dest.txt"  # doesn't exist
            src = Path(td) / "src.link"
            cl_utils.symlink_relative(dest, src)
            self.assertTrue(src.is_symlink())
            self.assertEqual(_readlink(src), "dest.txt")  # relative
            # Need dest.resolve() on Mac OS where tempdirs can be symlinks
            self.assertEqual(src.resolve(), dest.resolve())

    def test_dest_in_subdir(self):
        with tempfile.TemporaryDirectory() as td:
            destdir = Path(td) / "must" / "go" / "deeper"
            dest = destdir / "log.txt"  # doesn't exist
            src = Path(td) / "log.link"
            cl_utils.symlink_relative(dest, src)
            self.assertTrue(src.is_symlink())
            self.assertEqual(
                _readlink(src), "must/go/deeper/log.txt"
            )  # relative
            self.assertEqual(src.resolve(), dest.resolve())

    def test_dest_in_parent(self):
        with tempfile.TemporaryDirectory() as td:
            dest = Path(td) / "log.txt"  # doesn't exist
            srcdir = Path(td) / "must" / "go" / "deeper"  # doesn't exist yet
            src = srcdir / "log.link"
            cl_utils.symlink_relative(dest, src)
            self.assertTrue(src.is_symlink())
            self.assertEqual(_readlink(src), "../../../log.txt")  # relative
            self.assertEqual(src.resolve(), dest.resolve())

    def test_common_parent_srcdir_does_not_exist_yet(self):
        with tempfile.TemporaryDirectory() as td:
            # td is the common parent to both src and dest
            destdir = Path(td) / "trash" / "bin"
            dest = destdir / "garbage.txt"  # doesn't exist
            srcdir = Path(td) / "must" / "go" / "deeper"  # doesn't exist yet
            src = srcdir / "log.link"
            cl_utils.symlink_relative(dest, src)
            self.assertTrue(src.is_symlink())
            self.assertEqual(
                _readlink(src), "../../../trash/bin/garbage.txt"
            )  # relative
            self.assertEqual(src.resolve(), dest.resolve())

    def test_common_parent_srcdir_already_exists(self):
        with tempfile.TemporaryDirectory() as td:
            # td is the common parent to both src and dest
            destdir = Path(td) / "trash" / "bin"
            dest = destdir / "garbage.txt"  # doesn't exist
            srcdir = Path(td) / "must" / "go" / "deeper"
            srcdir.mkdir(
                parents=True
            )  # srcdir exists ahead of symlink_relative
            src = srcdir / "log.link"
            cl_utils.symlink_relative(dest, src)
            self.assertTrue(src.is_symlink())
            self.assertEqual(
                _readlink(src), "../../../trash/bin/garbage.txt"
            )  # relative
            self.assertEqual(src.resolve(), dest.resolve())

    def test_link_over_existing_link_dest_does_not_exist(self):
        with tempfile.TemporaryDirectory() as td:
            dest = Path(td) / "dest.txt"  # doesn't exist
            src = Path(td) / "src.link"
            # note: dest does not actually exist
            cl_utils.symlink_relative(dest, src)
            cl_utils.symlink_relative(dest, src)  # yes, link twice
            self.assertTrue(src.is_symlink())
            self.assertEqual(_readlink(src), "dest.txt")  # relative
            # Need dest.resolve() on Mac OS where tempdirs can be symlinks
            self.assertEqual(src.resolve(), dest.resolve())

    def test_link_replaces_file(self):
        with tempfile.TemporaryDirectory() as td:
            dest = Path(td) / "dest.txt"  # doesn't exist
            src = Path(td) / "src.link"
            # note: dest does not actually exist
            with open(src, "w") as f:
                f.write("\t\n")
            cl_utils.symlink_relative(dest, src)  # overwrite file
            self.assertTrue(src.is_symlink())
            self.assertEqual(_readlink(src), "dest.txt")  # relative
            # Need dest.resolve() on Mac OS where tempdirs can be symlinks
            self.assertEqual(src.resolve(), dest.resolve())

    def test_link_replaces_dir(self):
        with tempfile.TemporaryDirectory() as td:
            dest = Path(td) / "dest.txt"  # doesn't exist
            src = Path(td) / "src.link"
            # note: dest does not actually exist
            src.mkdir(parents=True, exist_ok=True)
            cl_utils.symlink_relative(dest, src)  # overwrite empty dir
            self.assertTrue(src.is_symlink())
            self.assertEqual(_readlink(src), "dest.txt")  # relative
            # Need dest.resolve() on Mac OS where tempdirs can be symlinks
            self.assertEqual(src.resolve(), dest.resolve())


class QualifyToolPathTests(unittest.TestCase):
    def test_absolute(self):
        path = Path("/foo/bar.exe")
        self.assertEqual(cl_utils.qualify_tool_path(path), str(path))

    def test_relative_subdir(self):
        path = Path("foo/bar.exe")
        self.assertEqual(cl_utils.qualify_tool_path(path), str(path))

    def test_relative_up_and_down(self):
        path = Path("../../foo/bar.exe")
        self.assertEqual(cl_utils.qualify_tool_path(path), str(path))

    def test_unqualified(self):
        path = Path("bar.exe")
        self.assertEqual(cl_utils.qualify_tool_path(path), "./bar.exe")

    def test_unqualified_redundant(self):
        path = Path("./bar.exe")
        self.assertEqual(cl_utils.qualify_tool_path(path), "./bar.exe")


class ExecRelaunchTests(unittest.TestCase):
    def go_away(self):
        cl_utils.exec_relaunch(["/my/handy/tool"])

    def test_mock_launch(self):
        """Example of how to mock exec_relaunch()."""
        with mock.patch.object(
            cl_utils, "exec_relaunch", side_effect=ImmediateExit
        ) as mock_launch:
            with self.assertRaises(ImmediateExit):
                self.go_away()

    def test_mock_call(self):
        exit_code = 21
        with mock.patch.object(
            subprocess, "call", return_value=exit_code
        ) as mock_call:
            with mock.patch.object(
                sys, "exit", side_effect=ImmediateExit
            ) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    self.go_away()
        mock_call.assert_called_once()
        mock_exit.assert_called_with(exit_code)


def increment_file(file: Path):
    if not file.exists():
        # First writer creates with value 1
        file.write_text("1")
        return 1
    else:
        count = int(file.read_text()) + 1
        file.write_text(f"{count}")
        return count


def locked_increment_file(file: Path):
    lockfile = file.with_suffix(".lock")
    with cl_utils.BlockingFileLock(lockfile) as lock:
        return increment_file(file)


class BlockingFileLockTests(unittest.TestCase):
    def test_exclusion(self):
        N = 50
        with tempfile.TemporaryDirectory() as td:
            count_file = Path(td) / "count"
            try:
                with multiprocessing.Pool() as pool:
                    counts = pool.map(locked_increment_file, [count_file] * N)
            except OSError:  # in case /dev/shm is not writeable (required)
                counts = map(locked_increment_file, [count_file] * N)

            self.assertEqual(sorted(counts), list(range(1, N + 1)))

            # Run a second batch, same lock file
            try:
                with multiprocessing.Pool() as pool:
                    counts = pool.map(locked_increment_file, [count_file] * N)
            except OSError:  # in case /dev/shm is not writeable (required)
                counts = map(locked_increment_file, [count_file] * N)

            self.assertEqual(sorted(counts), list(range(N + 1, 2 * N + 1)))


class SubprocessResultTests(unittest.TestCase):
    def test_defaults(self):
        result = cl_utils.SubprocessResult(3)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout, [])
        self.assertEqual(result.stderr, [])
        self.assertEqual(result.stdout_text, "")
        self.assertEqual(result.stderr_text, "")

    def test_with_output(self):
        result = cl_utils.SubprocessResult(
            1, stdout=["foo", "bar"], stderr=["baz"]
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, ["foo", "bar"])
        self.assertEqual(result.stderr, ["baz"])
        self.assertEqual(result.stdout_text, "foo\nbar")
        self.assertEqual(result.stderr_text, "baz")


class SubprocessCallTests(unittest.TestCase):
    def test_success(self):
        result = cl_utils.subprocess_call(["echo", "hello"])
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, ["hello"])
        self.assertEqual(result.stderr, [])
        self.assertGreater(result.pid, 0)

    def test_success_quiet(self):
        result = cl_utils.subprocess_call(["echo", "hello"], quiet=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, ["hello"])  # still captured
        self.assertEqual(result.stderr, [])
        self.assertGreater(result.pid, 0)

    def test_failure(self):
        result = cl_utils.subprocess_call(["false"])
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, [])
        self.assertEqual(result.stderr, [])
        self.assertGreater(result.pid, 0)

    def test_error(self):
        result = cl_utils.subprocess_call(["ls", "/does/not/exist"])
        # error code is 2 on linux, 1 on darwin
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, [])
        self.assertIn("No such file or directory", result.stderr[0])
        self.assertIn("/does/not/exist", result.stderr[0])
        self.assertGreater(result.pid, 0)


class SubprocessCommunicateTests(unittest.TestCase):
    def test_cat(self):
        input = "echo\n"
        result = cl_utils.subprocess_communicate(["cat"], input)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, ["echo"])
        self.assertEqual(result.stderr, [])

    def test_sed(self):
        input = "aaabbbccc\n"
        result = cl_utils.subprocess_communicate(
            ["sed", "-e", "s|b|B|g"], input
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, ["aaaBBBccc"])
        self.assertEqual(result.stderr, [])

    def test_failure(self):
        result = cl_utils.subprocess_communicate(["false"], "ignored_text")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, [])
        self.assertEqual(result.stderr, [])
        self.assertGreater(result.pid, 0)


if __name__ == "__main__":
    unittest.main()
