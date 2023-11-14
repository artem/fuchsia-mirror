# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Tests for clang_utilities.bzl

The BUILD.bazel file of a clang repository should include this file
and call the `include_clang_utils_test_suite()` function to define a target
that can be launched with `bazel test` to run the test suite, as in:

  bazel test @clang_repo//:test_suite

If @clang_repo//BUILD.bazel contains something like:

  load("@fuchsia_sdk_common//:toolchains/clang/clang_utils_test.bzl",
       "include_clang_utils_test_suite")

  include_clang_utils_test_suite(
    name = "test_suite",
  )
"""

load("@bazel_skylib//lib:unittest.bzl", "asserts", "unittest")
load("//platforms:utils_test.bzl", "platforms_utils_test")
load(
    "//:toolchains/clang/clang_utils.bzl",
    "clang_all_target_tags",
    "format_labels_list_to_target_tag_dict",
    "format_target_tag_labels_dict",
    "process_clang_builtins_output",
    "to_clang_target_tuple",
)

def _process_clang_builtins_output_test(ctx):
    response = """Fuchsia clang version 16.0.0 (https://llvm.googlesource.com/llvm-project 039b969b32b64b64123dce30dd28ec4e343d893f)
Target: x86_64-unknown-linux-gnu
Thread model: posix
InstalledDir: /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin
Found candidate GCC installation: /usr/lib/gcc/x86_64-linux-gnu/11
Found candidate GCC installation: /usr/lib/gcc/x86_64-linux-gnu/12
Selected GCC installation: /usr/lib/gcc/x86_64-linux-gnu/12
Candidate multilib: .;@m64
Candidate multilib: 32;@m32
Candidate multilib: x32;@mx32
Selected multilib: .;@m64
 (in-process)
 "/usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin/clang-16" -cc1 -triple x86_64-unknown-linux-gnu -E -disable-free -clear-ast-before-backend -disable-llvm-verifier -discard-value-names -main-file-name empty -mrelocation-model pic -pic-level 2 -pic-is-pie -mframe-pointer=all -fmath-errno -ffp-contract=on -fno-rounding-math -mconstructor-aliases -funwind-tables=2 -target-cpu x86-64 -tune-cpu generic -debugger-tuning=gdb -v -fcoverage-compilation-dir=/usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang -resource-dir /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/lib/clang/16.0.0 -internal-isystem /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin/../include/x86_64-unknown-linux-gnu/c++/v1 -internal-isystem /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin/../include/c++/v1 -internal-isystem /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/lib/clang/16.0.0/include -internal-isystem /usr/local/include -internal-isystem /usr/lib/gcc/x86_64-linux-gnu/12/../../../../x86_64-linux-gnu/include -internal-externc-isystem /usr/include/x86_64-linux-gnu -internal-externc-isystem /include -internal-externc-isystem /usr/include -fdeprecated-macro -fdebug-compilation-dir=/usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang -ferror-limit 19 -fgnuc-version=4.2.1 -fcxx-exceptions -fexceptions -faddrsig -D__GCC_HAVE_DWARF2_CFI_ASM=1 -o - -x c++ ./empty
clang -cc1 version 16.0.0 based upon LLVM 16.0.0git default target x86_64-unknown-linux-gnu
ignoring nonexistent directory "/usr/lib/gcc/x86_64-linux-gnu/12/../../../../x86_64-linux-gnu/include"
ignoring nonexistent directory "/include"
#include "..." search starts here:
#include <...> search starts here:
 /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin/../include/x86_64-unknown-linux-gnu/c++/v1
 /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin/../include/c++/v1
 /usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/lib/clang/16.0.0/include
 /usr/local/include
 /usr/include/x86_64-linux-gnu
 /usr/include
End of search list.
"""
    expect = (
        "16",
        "16.0.0",
        [
            "/usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin/../include/x86_64-unknown-linux-gnu/c++/v1",
            "/usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/bin/../include/c++/v1",
            "/usr/local/home/user/fuchsia/out/default/gen/build/bazel/output_base/external/prebuilt_clang/lib/clang/16.0.0/include",
        ],
    )
    env = unittest.begin(ctx)
    asserts.equals(env, expect, process_clang_builtins_output(response))
    return unittest.end(env)

process_clang_builtins_output_test = unittest.make(_process_clang_builtins_output_test)

def _to_clang_target_tuple_test(ctx):
    # A list of (target_os, target_arch, expected_clang_tuple) test cases.
    cases = [
        ("fuchsia", "x64", "x86_64-unknown-fuchsia"),
        ("fuchsia", "arm64", "aarch64-unknown-fuchsia"),
        ("fuchsia", "riscv64", "riscv64-unknown-fuchsia"),
        ("fuchsia", "k8", "x86_64-unknown-fuchsia"),
        ("fuchsia", "x86_64", "x86_64-unknown-fuchsia"),
        ("fuchsia", "aarch64", "aarch64-unknown-fuchsia"),
        ("linux", "x64", "x86_64-unknown-linux-gnu"),
        ("linux", "arm64", "aarch64-unknown-linux-gnu"),
        ("linux", "riscv64", "riscv64-unknown-linux-gnu"),
        ("linux", "k8", "x86_64-unknown-linux-gnu"),
        ("linux", "x86_64", "x86_64-unknown-linux-gnu"),
        ("linux", "aarch64", "aarch64-unknown-linux-gnu"),
        ("mac", "x64", "x86_64-apple-darwin"),
        ("mac", "arm64", "aarch64-apple-darwin"),
        ("macos", "k8", "x86_64-apple-darwin"),
        ("macos", "x86_64", "x86_64-apple-darwin"),
        ("macos", "aarch64", "aarch64-apple-darwin"),
        ("macos", "arm64", "aarch64-apple-darwin"),
        ("osx", "k8", "x86_64-apple-darwin"),
        ("osx", "x86_64", "x86_64-apple-darwin"),
        ("osx", "aarch64", "aarch64-apple-darwin"),
        ("osx", "arm64", "aarch64-apple-darwin"),
    ]
    env = unittest.begin(ctx)
    for target_os, target_arch, expected_tuple in cases:
        asserts.equals(env, expected_tuple, to_clang_target_tuple(target_os, target_arch))
    return unittest.end(env)

to_clang_target_tuple_test = unittest.make(_to_clang_target_tuple_test)

def _format_target_tag_labels_dict_test(ctx):
    cases = [
        (
            {
                "linux-x64": ["//{pkg}:{clang_target_tuple}/name_{os}_{cpu}"],
                "macos-aarch64": ["//{pkg}:{clang_target_tuple}/name_{os}_{cpu}"],
                "mac-arm64": ["//{pkg}:{clang_target_tuple}/name_{bazel_os}_{bazel_cpu}"],
                "fuchsia-riscv64": ["//{pkg}:{clang_target_tuple}/name_{os}_{cpu}"],
            },
            {"pkg": "package"},
            {
                "linux-x64": ["//package:x86_64-unknown-linux-gnu/name_linux_x64"],
                "macos-aarch64": ["//package:aarch64-apple-darwin/name_mac_arm64"],
                "mac-arm64": ["//package:aarch64-apple-darwin/name_macos_aarch64"],
                "fuchsia-riscv64": ["//package:riscv64-unknown-fuchsia/name_fuchsia_riscv64"],
            },
        ),
    ]
    env = unittest.begin(ctx)
    for input_dict, extra_dict, expected_dict in cases:
        asserts.equals(env, expected_dict, format_target_tag_labels_dict(input_dict, extra_dict))
    return unittest.end(env)

format_target_tag_labels_dict_test = unittest.make(_format_target_tag_labels_dict_test)

def _format_labels_list_to_target_tag_dict_test(ctx):
    cases = [
        (
            [
                "{pkg}:{clang_target_tuple}/foo",
                "{os}_{cpu}_{bazel_os}_{bazel_cpu}.txt",
            ],
            clang_all_target_tags,
            {"pkg": "//package"},
            {
                "linux-x64": [
                    "//package:x86_64-unknown-linux-gnu/foo",
                    "linux_x64_linux_x86_64.txt",
                ],
                "linux-arm64": [
                    "//package:aarch64-unknown-linux-gnu/foo",
                    "linux_arm64_linux_aarch64.txt",
                ],
                "mac-x64": [
                    "//package:x86_64-apple-darwin/foo",
                    "mac_x64_macos_x86_64.txt",
                ],
                "mac-arm64": [
                    "//package:aarch64-apple-darwin/foo",
                    "mac_arm64_macos_aarch64.txt",
                ],
                "fuchsia-x64": [
                    "//package:x86_64-unknown-fuchsia/foo",
                    "fuchsia_x64_fuchsia_x86_64.txt",
                ],
                "fuchsia-arm64": [
                    "//package:aarch64-unknown-fuchsia/foo",
                    "fuchsia_arm64_fuchsia_aarch64.txt",
                ],
                "fuchsia-riscv64": [
                    "//package:riscv64-unknown-fuchsia/foo",
                    "fuchsia_riscv64_fuchsia_riscv64.txt",
                ],
            },
        ),
    ]
    env = unittest.begin(ctx)
    for labels, target_tags, extra_dict, expected in cases:
        asserts.equals(env, expected, format_labels_list_to_target_tag_dict(labels, target_tags, extra_dict))
    return unittest.end(env)

format_labels_list_to_target_tag_dict_test = unittest.make(_format_labels_list_to_target_tag_dict_test)

def _clang_all_target_tags_test(ctx):
    expected_dict = {
        "fuchsia-x64": None,
        "fuchsia-arm64": None,
        "fuchsia-riscv64": None,
        "linux-x64": None,
        "linux-arm64": None,
        "mac-x64": None,
        "mac-arm64": None,
    }
    input_dict = {
        target_tag: None
        for target_tag in clang_all_target_tags
    }
    env = unittest.begin(ctx)
    asserts.equals(env, expected_dict, input_dict)
    return unittest.end(env)

clang_all_target_tags_test = unittest.make(_clang_all_target_tags_test)

def include_clang_utils_test_suite(name):
    unittest.suite(
        name,
        platforms_utils_test,
        process_clang_builtins_output_test,
        to_clang_target_tuple_test,
        format_target_tag_labels_dict_test,
        clang_all_target_tags_test,
    )
