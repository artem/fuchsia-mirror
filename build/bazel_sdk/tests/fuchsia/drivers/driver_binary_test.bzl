# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

""" A test which verifies that driver binaries are built correctly. """

load("//test_utils:py_test_utils.bzl", "PY_TOOLCHAIN_DEPS", "populate_py_test_sh_script")
load("@fuchsia_sdk//fuchsia/private:fuchsia_transition.bzl", "fuchsia_transition")
load("@fuchsia_sdk//fuchsia/private:fuchsia_debug_symbols.bzl", "strip_resources")

def _driver_binary_test_impl(ctx):
    # We normally strip the binary when we do our packaging but we don't need to
    # create an entire fuchsia package to run this test so we just strip it here.
    resource = struct(
        src = ctx.files.driver[0],
        dest = "__not_needed__",
    )
    stripped_resources, _debug_info = strip_resources(ctx, [resource])
    driver = stripped_resources[0].src

    script = ctx.actions.declare_file(ctx.label.name + ".sh")
    args = [
        "--driver_binary={}".format(driver.short_path),
        "--readelf={}".format(ctx.executable._readelf.short_path),
    ]

    runfiles = [
        ctx.executable._readelf,
        driver,
    ]

    populate_py_test_sh_script(ctx, script, ctx.executable._binary_checker, args)

    return [
        DefaultInfo(
            executable = script,
            runfiles = ctx.runfiles(
                files = runfiles,
            ).merge(ctx.attr._binary_checker[DefaultInfo].default_runfiles),
        ),
    ]

driver_binary_test = rule(
    doc = """Validate the driver binary.""",
    test = True,
    implementation = _driver_binary_test_impl,
    cfg = fuchsia_transition,
    attrs = {
        "driver": attr.label(
            doc = "The driver binary.",
            mandatory = True,
        ),
        "_binary_checker": attr.label(
            default = ":verify_driver_binary",
            executable = True,
            cfg = "exec",
        ),
        "fuchsia_api_level": attr.string(
            doc = "The fuchsia api level to target for this test.",
        ),
        "_readelf": attr.label(
            default = "@fuchsia_clang//:bin/llvm-readelf",
            executable = True,
            cfg = "exec",
            allow_single_file = True,
        ),
        # The CC tools are needed to strip the binary
        "_elf_strip_tool": attr.label(
            default = "@fuchsia_sdk//fuchsia/tools:elf_strip",
            executable = True,
            cfg = "exec",
        ),
        "_generate_symbols_dir_tool": attr.label(
            default = "@fuchsia_sdk//fuchsia/tools:generate_symbols_dir",
            executable = True,
            cfg = "exec",
        ),
        "_cc_toolchain": attr.label(
            default = Label("@bazel_tools//tools/cpp:current_cc_toolchain"),
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    } | PY_TOOLCHAIN_DEPS,
)
