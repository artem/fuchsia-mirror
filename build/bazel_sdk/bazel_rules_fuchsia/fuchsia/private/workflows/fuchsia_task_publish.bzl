# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Publishes packages as a workflow task."""

load("@rules_fuchsia//fuchsia/private:providers.bzl", "FuchsiaPackageGroupInfo", "FuchsiaPackageInfo")
load(":fuchsia_task.bzl", "fuchsia_task_rule")

def _fuchsia_task_publish_impl(ctx, make_fuchsia_task):
    sdk = ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"]
    far_files = [
        pkg.far_file
        for dep in ctx.attr.packages
        for pkg in (dep[FuchsiaPackageGroupInfo].packages if FuchsiaPackageGroupInfo in dep else [dep[FuchsiaPackageInfo]])
    ]

    repo_name_args = [
        "--repo_name",
        ctx.attr.package_repository_name,
    ] if ctx.attr.package_repository_name else []
    return make_fuchsia_task(
        task_runner = ctx.attr._publish_packages_tool,
        prepend_args = [
            "--ffx",
            sdk.ffx,
            "--pm",
            sdk.pm,
            "--package",
        ] + far_files + repo_name_args,
        runfiles = [sdk.runfiles, far_files],
    )

(
    _fuchsia_task_publish,
    _fuchsia_task_publish_for_test,
    fuchsia_task_publish,
) = fuchsia_task_rule(
    implementation = _fuchsia_task_publish_impl,
    doc = """A workflow task that publishes multiple fuchsia packages.""",
    toolchains = ["@rules_fuchsia//fuchsia:toolchain"],
    attrs = {
        "packages": attr.label_list(
            doc = "The packages to publish.",
            providers = [[FuchsiaPackageInfo], [FuchsiaPackageGroupInfo]],
        ),
        "package_repository_name": attr.string(
            doc = "Optionally specify the repository name to publish these packages to.",
        ),
        "_publish_packages_tool": attr.label(
            doc = "The publish_packages tool.",
            default = "//fuchsia/tools:publish_packages",
            executable = True,
            cfg = "target",
        ),
    },
)
