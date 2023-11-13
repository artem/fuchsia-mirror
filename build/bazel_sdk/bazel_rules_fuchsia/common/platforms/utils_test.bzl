# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Unit-tests for utils.bzl."""

load("@bazel_skylib//lib:unittest.bzl", "asserts", "unittest")
load(
    "//platforms:utils.bzl",
    "config_setting_label_for_target_os_cpu",
    "config_setting_label_for_target_tag",
    "target_tag_dict_to_select_keys",
    "to_bazel_cpu_name",
    "to_bazel_os_name",
    "to_fuchsia_cpu_name",
    "to_fuchsia_os_name",
    "to_platform_cpu_constraint",
    "to_platform_os_constraint",
    "to_target_os_cpu_pair",
)

def _test_to_fuchsia_os_name(env):
    cases = [
        ("fuchsia", "fuchsia"),
        ("linux", "linux"),
        ("mac", "mac"),
        ("macos", "mac"),
        ("osx", "mac"),
        ("windows", "win"),
        ("win", "win"),
    ]
    for input, expected in cases:
        asserts.equals(env, expected, to_fuchsia_os_name(input))

def _test_to_fuchsia_cpu_name(env):
    cases = [
        ("x64", "x64"),
        ("k8", "x64"),
        ("x86_64", "x64"),
        ("arm64", "arm64"),
        ("aarch64", "arm64"),
        ("riscv64", "riscv64"),
    ]
    for input, expected in cases:
        asserts.equals(env, expected, to_fuchsia_cpu_name(input))

def _test_to_bazel_os_name(env):
    cases = [
        ("fuchsia", "fuchsia"),
        ("linux", "linux"),
        ("mac", "macos"),
        ("macos", "macos"),
        ("osx", "macos"),
        ("windows", "windows"),
        ("win", "windows"),
    ]
    for input, expected in cases:
        asserts.equals(env, expected, to_bazel_os_name(input))

def _test_to_bazel_cpu_name(env):
    cases = [
        ("x64", "x86_64"),
        ("k8", "x86_64"),
        ("x86_64", "x86_64"),
        ("arm64", "aarch64"),
        ("aarch64", "aarch64"),
        ("riscv64", "riscv64"),
    ]
    for input, expected in cases:
        asserts.equals(env, expected, to_bazel_cpu_name(input))

def _test_to_platform_os_constraint(env):
    cases = [
        ("fuchsia", "@platforms//os:fuchsia"),
        ("linux", "@platforms//os:linux"),
        ("mac", "@platforms//os:macos"),
        ("macos", "@platforms//os:macos"),
        ("osx", "@platforms//os:macos"),
        ("windows", "@platforms//os:windows"),
        ("win", "@platforms//os:windows"),
    ]
    for input, expected in cases:
        asserts.equals(env, expected, to_platform_os_constraint(input))

def _test_to_platform_cpu_constraint(env):
    cases = [
        ("x64", "@platforms//cpu:x86_64"),
        ("k8", "@platforms//cpu:x86_64"),
        ("x86_64", "@platforms//cpu:x86_64"),
        ("arm64", "@platforms//cpu:aarch64"),
        ("aarch64", "@platforms//cpu:aarch64"),
        ("riscv64", "@platforms//cpu:riscv64"),
    ]
    for input, expected in cases:
        asserts.equals(env, expected, to_platform_cpu_constraint(input))

def _test_config_setting_label_for_target_os_cpu(env):
    cases = [
        ("fuchsia", "x64", "@fuchsia_sdk_common//platforms:is_fuchsia_x64"),
        ("fuchsia", "k8", "@fuchsia_sdk_common//platforms:is_fuchsia_x64"),
        ("fuchsia", "x86_64", "@fuchsia_sdk_common//platforms:is_fuchsia_x64"),
        ("fuchsia", "arm64", "@fuchsia_sdk_common//platforms:is_fuchsia_arm64"),
        ("fuchsia", "aarch64", "@fuchsia_sdk_common//platforms:is_fuchsia_arm64"),
        ("fuchsia", "riscv64", "@fuchsia_sdk_common//platforms:is_fuchsia_riscv64"),
        ("linux", "x64", "@fuchsia_sdk_common//platforms:is_linux_x64"),
        ("linux", "k8", "@fuchsia_sdk_common//platforms:is_linux_x64"),
        ("linux", "x86_64", "@fuchsia_sdk_common//platforms:is_linux_x64"),
        ("linux", "arm64", "@fuchsia_sdk_common//platforms:is_linux_arm64"),
        ("linux", "aarch64", "@fuchsia_sdk_common//platforms:is_linux_arm64"),
        ("linux", "riscv64", "@fuchsia_sdk_common//platforms:is_linux_riscv64"),
        ("mac", "x64", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("mac", "k8", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("mac", "x86_64", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("mac", "arm64", "@fuchsia_sdk_common//platforms:is_mac_arm64"),
        ("mac", "aarch64", "@fuchsia_sdk_common//platforms:is_mac_arm64"),
        ("macos", "x64", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("macos", "k8", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("macos", "x86_64", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("macos", "arm64", "@fuchsia_sdk_common//platforms:is_mac_arm64"),
        ("macos", "aarch64", "@fuchsia_sdk_common//platforms:is_mac_arm64"),
        ("osx", "x64", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("osx", "k8", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("osx", "x86_64", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("osx", "arm64", "@fuchsia_sdk_common//platforms:is_mac_arm64"),
        ("osx", "aarch64", "@fuchsia_sdk_common//platforms:is_mac_arm64"),
    ]
    for os, cpu, expected in cases:
        asserts.equals(env, expected, config_setting_label_for_target_os_cpu(os, cpu))

def _test_config_setting_label_for_target_tag(env):
    cases = [
        ("fuchsia-x64", "@fuchsia_sdk_common//platforms:is_fuchsia_x64"),
        ("fuchsia-arm64", "@fuchsia_sdk_common//platforms:is_fuchsia_arm64"),
        ("fuchsia-riscv64", "@fuchsia_sdk_common//platforms:is_fuchsia_riscv64"),
        ("linux-x64", "@fuchsia_sdk_common//platforms:is_linux_x64"),
        ("linux-arm64", "@fuchsia_sdk_common//platforms:is_linux_arm64"),
        ("linux-riscv64", "@fuchsia_sdk_common//platforms:is_linux_riscv64"),
        ("mac-x64", "@fuchsia_sdk_common//platforms:is_mac_x64"),
        ("mac-arm64", "@fuchsia_sdk_common//platforms:is_mac_arm64"),
    ]
    for input, expected in cases:
        asserts.equals(env, expected, config_setting_label_for_target_tag(input))

def _test_target_tag_dict_to_select_keys(env):
    cases = [
        (
            {
                "linux-x64": ["oh linux"],
                "mac-arm64": ["damn mac"],
                "fuchsia-riscv64": ["sweet risc"],
            },
            {
                "@fuchsia_sdk_common//platforms:is_linux_x64": ["oh linux"],
                "@fuchsia_sdk_common//platforms:is_mac_arm64": ["damn mac"],
                "@fuchsia_sdk_common//platforms:is_fuchsia_riscv64": ["sweet risc"],
            },
        ),
    ]
    for input_dict, expected_dict in cases:
        asserts.equals(env, expected_dict, target_tag_dict_to_select_keys(input_dict))
        expected_dict_with_default = expected_dict | {
            "//conditions:default": ["default label"],
        }
        asserts.equals(
            env,
            expected_dict_with_default,
            target_tag_dict_to_select_keys(
                input_dict,
                add_default = ["default label"],
            ),
        )

def _platforms_utils_test(ctx):
    env = unittest.begin(ctx)
    _test_to_fuchsia_os_name(env)
    _test_to_fuchsia_cpu_name(env)
    _test_to_bazel_os_name(env)
    _test_to_bazel_cpu_name(env)
    _test_config_setting_label_for_target_os_cpu(env)
    _test_config_setting_label_for_target_tag(env)
    _test_target_tag_dict_to_select_keys(env)
    return unittest.end(env)

platforms_utils_test = unittest.make(_platforms_utils_test)
