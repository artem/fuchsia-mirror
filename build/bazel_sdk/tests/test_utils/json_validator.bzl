# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Convenience function to generate a JSON validating script."""

load("//test_utils:py_test_utils.bzl", "PY_TOOLCHAIN_DEPS", "create_python3_shell_wrapper_provider")

# These attributes must be part of any rule() whose implementation
# function wants to call create_validation_script() below. They
# are used to ensure to ensure the function can access the
# paths and runfiles of json_comparator.py and of the python
# interpreter.
CREATE_VALIDATION_SCRIPT_ATTRS = {
    "_json_comparator": attr.label(
        default = "@fuchsia_sdk//fuchsia/tools:json_comparator",
        executable = True,
        cfg = "exec",
    ),
} | PY_TOOLCHAIN_DEPS

def create_validation_script_provider(ctx, generated, golden_file, runfiles = None):
    """Create a validation script and its related runfiles object.

    Create a validation script that invokes json_comparator.py to
    verify that a given generated file matches a golden file.

    Args:
      ctx: A rule context object. The corresponding rule definition
         *must* include the content of CREATE_VALIDATION_SCRIPT_ATTRS
         in its `attrs` value.

      generated: a File object pointing to the generated
         file/directory to verify.

      golden_file: a File object pointing to the golden file used
         for verification.

      runfiles: an optional runfiles value for extra runtime requirements.

    Returns:
        A DefaultInfo provider for the script and its runtime requirements.
    """
    validator_path = ctx.executable._json_comparator.short_path

    generated_path = generated.short_path
    validator_args = [
        "--generated={}".format(generated_path),
        "--golden={}".format(golden_file.short_path),
    ]

    validator_runfiles = ctx.runfiles(
        files = [
            golden_file,
            generated,
        ],
    ).merge(
        ctx.attr._json_comparator[DefaultInfo].default_runfiles,
    )
    if runfiles != None:
        validator_runfiles = validator_runfiles.merge(runfiles)

    return create_python3_shell_wrapper_provider(
        ctx,
        validator_path,
        args = validator_args,
        runfiles = validator_runfiles,
    )
