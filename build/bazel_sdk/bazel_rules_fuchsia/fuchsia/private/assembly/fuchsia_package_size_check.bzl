# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for running size checker on blobfs package."""

load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_assembly_inputs")
load(":providers.bzl", "FuchsiaProductImageInfo", "FuchsiaSizeCheckerInfo")

def _fuchsia_package_size_check_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    manifests = ",".join([pkg[FuchsiaPackageInfo].package_manifest.path for pkg in ctx.attr.packages])

    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    size_report = ctx.actions.declare_file(ctx.label.name + ".json")
    verbose_output = ctx.actions.declare_file(ctx.label.name + "_verbose_output.json")
    budgets_file = ctx.actions.declare_file(ctx.label.name + "_size_budgets.json")

    # Construct the size budgets file from the manifests.
    ctx.actions.run(
        outputs = [budgets_file],
        executable = ctx.executable._construct_budgets_file,
        arguments = [
            "--name",
            ctx.attr.size_report_name,
            "--budget",
            str(ctx.attr.budget),
            "--creep-budget",
            str(ctx.attr.creep_budget),
            "--packages",
            manifests,
            "--output",
            budgets_file.path,
        ],
    )

    # Size checker execution
    inputs = get_ffx_assembly_inputs(fuchsia_toolchain) + [budgets_file] + ctx.files.packages
    outputs = [size_report, verbose_output, ffx_isolate_dir]

    # Gather all the arguments to pass to ffx.
    ffx_invocation = [
        fuchsia_toolchain.ffx.path,
        "--config \"assembly_enabled=true\"",
        "--isolate-dir",
        ffx_isolate_dir.path,
        "assembly",
        "size-check",
        "package",
        "--budgets",
        budgets_file.path,
        "--blobfs-layout",
        "deprecated_padded",
        "--gerrit-output",
        size_report.path,
        "--verbose-json-output",
        verbose_output.path,
    ]

    script_lines = [
        "set -e",
        "mkdir -p " + ffx_isolate_dir.path,
        " ".join(ffx_invocation),
    ]
    script = "\n".join(script_lines)

    ctx.actions.run_shell(
        inputs = inputs,
        outputs = outputs,
        command = script,
        progress_message = "Size checking for %s" % ctx.label.name,
    )

    return [
        DefaultInfo(files = depset(direct = outputs + [budgets_file])),
        FuchsiaSizeCheckerInfo(
            size_budgets = budgets_file,
            size_report = size_report,
            verbose_output = verbose_output,
        ),
    ]

fuchsia_package_size_check = rule(
    doc = """Create a size report for a set of fuchsia packages.""",
    implementation = _fuchsia_package_size_check_impl,
    provides = [FuchsiaSizeCheckerInfo],
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "size_report_name": attr.string(
            doc = "The name to add to the size report for viewing in Gerrit.",
            mandatory = True,
        ),
        "packages": attr.label_list(
            doc = "fuchsia_package targets to cover in the report.",
            providers = [FuchsiaPackageInfo],
            allow_empty = False,
        ),
        "budget": attr.int(
            doc = "Maximum number of bytes the packages can consume.",
            mandatory = True,
        ),
        "creep_budget": attr.int(
            doc = "Maximum number of bytes the packages can grow without a warning.",
            mandatory = True,
        ),
        "_construct_budgets_file": attr.label(
            default = "//fuchsia/tools:construct_budgets_file",
            executable = True,
            cfg = "exec",
        ),
    },
)
