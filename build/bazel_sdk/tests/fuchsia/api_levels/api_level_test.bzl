# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia API level support."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")
load("@fuchsia_sdk//fuchsia/private:fuchsia_api_level.bzl", "FuchsiaAPILevelInfo", "fuchsia_api_level")

def _level_setting_test_impl(ctx):
    env = analysistest.begin(ctx)

    target_under_test = analysistest.target_under_test(env)
    api_level_info = target_under_test[FuchsiaAPILevelInfo]

    asserts.equals(
        env,
        ctx.attr.expected_level,
        api_level_info.level,
    )

    return analysistest.end(env)

level_setting_test = analysistest.make(
    _level_setting_test_impl,
    attrs = {
        "expected_level": attr.string(),
    },
)

def _make_test_fuchsia_api_level(name, level):
    fuchsia_api_level(
        name = name,
        build_setting_default = level,
        # Use levels that we know will never be in the SDK
        valid_api_levels_for_test = ["1", "2", "3"],
    )

def _test_level_setting():
    _make_test_fuchsia_api_level(
        name = "supported",
        level = "2",
    )

    level_setting_test(
        name = "test_setting_supported",
        target_under_test = ":supported",
        expected_level = "2",
        tags = ["manual"],
    )

    _make_test_fuchsia_api_level(
        name = "unset",
        level = "",
    )

    level_setting_test(
        name = "test_unset",
        target_under_test = ":unset",
        expected_level = "",
        tags = ["manual"],
    )

# Failure tests
def _level_setting_failure_test_impl(ctx):
    env = analysistest.begin(ctx)
    asserts.expect_failure(env, ctx.attr.expected_failure_message)
    return analysistest.end(env)

level_setting_failure_test = analysistest.make(
    _level_setting_failure_test_impl,
    expect_failure = True,
    attrs = {
        "expected_failure_message": attr.string(),
    },
)

def _test_level_setting_failures():
    _make_test_fuchsia_api_level(
        name = "head",
        level = "HEAD",
    )

    level_setting_failure_test(
        name = "test_setting_head",
        target_under_test = ":head",
        expected_failure_message = "HEAD is not a valid API level",
        tags = ["manual"],
    )

    _make_test_fuchsia_api_level(
        name = "unsupported_level",
        level = "900",
    )

    level_setting_failure_test(
        name = "test_unsupported",
        target_under_test = ":unsupported_level",
        expected_failure_message = "900 is not a valid API level",
        tags = ["manual"],
    )

def fuchsia_api_level_test_suite(name, **kwargs):
    _test_level_setting()
    _test_level_setting_failures()

    native.test_suite(
        name = name,
        tests = [
            # _test_level_setting tests
            ":test_setting_supported",
            ":test_unset",

            # _test_level_setting_failures tests
            ":test_setting_head",
            ":test_unsupported",
        ],
        **kwargs
    )
