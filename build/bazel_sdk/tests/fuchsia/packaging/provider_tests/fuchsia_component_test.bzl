# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")
load("@fuchsia_sdk//fuchsia:defs.bzl", "fuchsia_component", "fuchsia_driver_component")
load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaComponentInfo")
load("//test_utils:make_file.bzl", "make_file")

def _local_name(name):
    """ Returns a name scoped to this file to avoid conflicts when putting into a BUILD file"""
    return "fuchsia_component_test_suite-{name}".format(name = name)

def _local_target(name):
    return ":{}".format(_local_name(name))

## Name Tests
def _provider_test_impl(ctx):
    env = analysistest.begin(ctx)

    target_under_test = analysistest.target_under_test(env)
    component_info = target_under_test[FuchsiaComponentInfo]

    asserts.equals(
        env,
        ctx.attr.component_name,
        component_info.name,
    )

    asserts.equals(
        env,
        ctx.attr.run_tag,
        component_info.run_tag,
    )

    asserts.equals(
        env,
        ctx.attr.is_driver,
        component_info.is_driver,
    )

    asserts.equals(
        env,
        component_info.manifest.basename,
        ctx.attr.manifest_basename,
    )

    return analysistest.end(env)

_provider_test = analysistest.make(
    _provider_test_impl,
    attrs = {
        "component_name": attr.string(),
        "run_tag": attr.string(),
        "is_driver": attr.bool(),
        "manifest_basename": attr.string(),
    },
)

def _test_provider():
    make_file(
        name = _local_name("manifest_component"),
        filename = "component.cm",
        tags = ["manual"],
    )

    make_file(
        name = _local_name("manifest_driver"),
        filename = "driver.cm",
        tags = ["manual"],
    )

    fuchsia_component(
        name = _local_name("component"),
        tags = ["manual"],
        component_name = "component",
        manifest = _local_target("manifest_component"),
    )

    make_file(
        name = _local_name("lib"),
        filename = "driver.so",
        content = "",
    )

    make_file(
        name = _local_name("bind"),
        filename = "bind",
        content = "",
    )

    fuchsia_driver_component(
        name = _local_name("driver"),
        tags = ["manual"],
        component_name = "driver",
        manifest = _local_target("manifest_driver"),
        driver_lib = _local_target("lib"),
        bind_bytecode = _local_target("bind"),
    )

    _provider_test(
        name = "test_component_providers",
        target_under_test = _local_target("component"),
        component_name = "component",
        is_driver = False,
        manifest_basename = "component.cm",
        run_tag = _local_name("component"),
    )

    _provider_test(
        name = "test_driver_component_providers",
        target_under_test = _local_target("driver"),
        component_name = "driver",
        is_driver = True,
        manifest_basename = "driver.cm",
        run_tag = _local_name("driver"),
    )

# Entry point from the BUILD file; macro for running each test case's macro and
# declaring a test suite that wraps them together.
def fuchsia_component_test_suite(name, **kwargs):
    _test_provider()

    native.test_suite(
        name = name,
        tests = [
            ":test_component_providers",
            ":test_driver_component_providers",
        ],
        **kwargs
    )
