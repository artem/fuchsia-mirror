# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""file-making utilities."""

load(
    "@fuchsia_sdk//fuchsia/private:providers.bzl",
    "FuchsiaPackageResourcesInfo",
)
load("@fuchsia_sdk//fuchsia/private:utils.bzl", "make_resource_struct")

def _make_file_impl(ctx):
    f = ctx.actions.declare_file(ctx.attr.filename)
    ctx.actions.write(f, ctx.attr.content)
    return DefaultInfo(files = depset([f]))

make_file = rule(
    implementation = _make_file_impl,
    doc = """A simple rule for making a file.

    This could be achieved with a genrule that cats to a file but this provides
    a simpler interface.""",
    attrs = {
        "filename": attr.string(),
        "content": attr.string(),
    },
)

def _make_resource_file_impl(ctx):
    f = ctx.actions.declare_file(ctx.label.name + "_" + ctx.attr.dest.replace("/", "_"))
    ctx.actions.write(f, ctx.attr.content)
    return [
        DefaultInfo(files = depset([f])),
        FuchsiaPackageResourcesInfo(resources = [make_resource_struct(src = f, dest = ctx.attr.dest)]),
    ]

make_resource_file = rule(
    implementation = _make_resource_file_impl,
    doc = "Creates a file which will provide a FuchsiaPackageResourcesInfo.",
    attrs = {
        "dest": attr.string(doc = "Where the file will be installed in a fuchsia package"),
        "content": attr.string(),
    },
)
