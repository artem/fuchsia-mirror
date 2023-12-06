# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""ffx invokation as a workflow task."""

load(":fuchsia_shell_task.bzl", "shell_task_rule")

def ffx_task_rule(*, implementation, toolchains = [], attrs = {}, **kwargs):
    """Starlark higher-order rule for creating ffx-based tasks."""

    def _fuchsia_task_ffx_impl(ctx, make_shell_task):
        sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]

        def _make_ffx_task(prepend_args = [], *runfiles):
            return make_shell_task(
                [ctx.attr._run_ffx, "--ffx", sdk.ffx] + prepend_args,
                default_argument_scope = "global",
                *runfiles
            )

        return implementation(ctx, _make_ffx_task)

    return shell_task_rule(
        implementation = _fuchsia_task_ffx_impl,
        toolchains = ["@fuchsia_sdk//fuchsia:toolchain"] + toolchains,
        attrs = {
            "_run_ffx": attr.label(
                doc = "The task runner used to run ffx tasks.",
                default = "//fuchsia/tools:run_ffx",
                executable = True,
                cfg = "exec",
            ),
        } | attrs,
        **kwargs
    )

def _fuchsia_task_ffx_impl(_, _make_ffx_task):
    return _make_ffx_task()

_fuchsia_task_ffx, _fuchsia_task_ffx_for_test, fuchsia_task_ffx = ffx_task_rule(
    implementation = _fuchsia_task_ffx_impl,
    doc = """Defines a task which invokes ffx.""",
)
