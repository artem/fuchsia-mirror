# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia product configuration."""

load("//fuchsia/private:fuchsia_package.bzl", "get_driver_component_manifests")
load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")

# buildifier: disable=module-docstring
load(
    ":providers.bzl",
    "FuchsiaAssembledPackageInfo",
    "FuchsiaOmahaOtaConfigInfo",
    "FuchsiaProductConfigInfo",
)
load(":util.bzl", "extract_labels", "replace_labels_with_files")

# Define build types
BUILD_TYPES = struct(
    ENG = "eng",
    USER = "user",
    USER_DEBUG = "userdebug",
)

# Define input device types option
INPUT_DEVICE_TYPE = struct(
    BUTTON = "button",
    KEYBOARD = "keyboard",
    MOUSE = "mouse",
    TOUCHSCREEN = "touchscreen",
)

def _create_pkg_detail(dep):
    if FuchsiaPackageInfo in dep:
        return {"manifest": dep[FuchsiaPackageInfo].package_manifest.path}

    package = dep[FuchsiaAssembledPackageInfo].package
    configs = dep[FuchsiaAssembledPackageInfo].configs
    config_data = []
    for config in configs:
        config_data.append(
            {
                "destination": config.destination,
                "source": config.source.path,
            },
        )
    return {"manifest": package.package_manifest.path, "config_data": config_data}

def _collect_file_deps(dep):
    if FuchsiaPackageInfo in dep:
        return dep[FuchsiaPackageInfo].files

    return dep[FuchsiaAssembledPackageInfo].files

def _fuchsia_product_configuration_impl(ctx):
    product_config = json.decode(ctx.attr.product_config)
    replace_labels_with_files(product_config, ctx.attr.product_config_labels)
    platform = product_config.get("platform", {})
    build_type = platform.get("build_type")
    product = product_config.get("product", {})
    packages = {}

    output_files = []
    base_pkg_details = []
    for dep in ctx.attr.base_packages:
        base_pkg_details.append(_create_pkg_detail(dep))
        output_files += _collect_file_deps(dep)
    packages["base"] = base_pkg_details

    cache_pkg_details = []
    for dep in ctx.attr.cache_packages:
        cache_pkg_details.append(_create_pkg_detail(dep))
        output_files += _collect_file_deps(dep)
    packages["cache"] = cache_pkg_details
    product["packages"] = packages

    base_driver_details = []
    for dep in ctx.attr.base_driver_packages:
        base_driver_details.append(
            {
                "package": dep[FuchsiaPackageInfo].package_manifest.path,
                "components": get_driver_component_manifests(dep),
            },
        )
        output_files += _collect_file_deps(dep)
    product["base_drivers"] = base_driver_details

    product_config["product"] = product

    if ctx.attr.ota_configuration:
        swd_config = product_config["platform"].setdefault("software_delivery", {})
        update_checker_config = swd_config.setdefault("update_checker", {})
        omaha_config = update_checker_config.setdefault("omaha_client", {})

        ota_config_info = ctx.attr.ota_configuration[FuchsiaOmahaOtaConfigInfo]

        channels_file = ctx.actions.declare_file("channel_config.json")
        ctx.actions.write(channels_file, ota_config_info.channels)
        output_files.append(channels_file)

        omaha_config["channels_path"] = channels_file.path

        tuf_config_paths = []
        for (hostname, repo_config) in ota_config_info.tuf_repositories.items():
            repo_config_file = ctx.actions.declare_file(hostname + ".json")
            ctx.actions.write(repo_config_file, repo_config)
            tuf_config_paths.append(repo_config_file.path)
            output_files.append(repo_config_file)
        swd_config["tuf_config_paths"] = tuf_config_paths

    product_config_file = ctx.actions.declare_file(ctx.label.name + "_product_config.json")
    content = json.encode_indent(product_config, indent = "  ")
    ctx.actions.write(product_config_file, content)
    output_files.append(product_config_file)

    return [
        DefaultInfo(files = depset(direct = output_files + ctx.files.product_config_labels + ctx.files.deps)),
        FuchsiaProductConfigInfo(
            product_config = product_config_file,
            build_type = build_type,
        ),
    ]

_fuchsia_product_configuration = rule(
    doc = """Generates a product configuration file.""",
    implementation = _fuchsia_product_configuration_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "product_config": attr.string(
            doc = "Raw json config. Used as a base template for the config",
            default = "{}",
        ),
        "product_config_labels": attr.label_keyed_string_dict(
            doc = """Map of labels to LABEL(label) strings in the product config.""",
            allow_files = True,
            default = {},
        ),
        "base_packages": attr.label_list(
            doc = "Fuchsia packages to be included in base.",
            providers = [
                [FuchsiaAssembledPackageInfo],
                [FuchsiaPackageInfo],
            ],
            default = [],
        ),
        "cache_packages": attr.label_list(
            doc = "Fuchsia packages to be included in cache.",
            providers = [
                [FuchsiaAssembledPackageInfo],
                [FuchsiaPackageInfo],
            ],
            default = [],
        ),
        "base_driver_packages": attr.label_list(
            doc = "Base-driver packages to include in product.",
            providers = [FuchsiaPackageInfo],
            default = [],
        ),
        "ota_configuration": attr.label(
            doc = "OTA configuration to include in product. only for use with products that use Omaha.",
            providers = [FuchsiaOmahaOtaConfigInfo],
        ),
        "deps": attr.label_list(
            doc = "Additional dependencies that must be built.",
            default = [],
        ),
    },
)

def fuchsia_product_configuration(
        name,
        json_config = None,
        base_packages = None,
        cache_packages = None,
        base_driver_packages = None,
        ota_configuration = None,
        **kwarg):
    """A new implementation of fuchsia_product_configuration that takes raw a json config.

    Args:
        name: Name of the rule.
        TODO(fxb/122898): Point to document instead of Rust definition
        json_config: product assembly json config, as a starlark dictionary.
            Format of this JSON config can be found in this Rust definitions:
               //src/lib/assembly/config_schema/src/assembly_config.rs

            Key values that take file paths should be declared as a string with
            the label path wrapped via "LABEL(" prefix and ")" suffix. For
            example:
            ```
            {
                "platform": {
                    "some_file": "LABEL(//path/to/file)",
                },
            },
            ```

            All assembly json inputs are supported, except for product.packages
            and product.base_drivers, which must be
            specified through the following args.
        base_packages: Fuchsia packages to be included in base.
        cache_packages: Fuchsia packages to be included in cache.
        base_driver_packages: Base driver packages to include in product.
        ota_configuration: OTA configuration to use with the product.
        **kwarg: Common bazel rule args passed through to the implementation rule.
    """

    if not json_config:
        json_config = {}
    if type(json_config) != "dict":
        fail("expecting a dictionary")

    _fuchsia_product_configuration(
        name = name,
        product_config = json.encode_indent(json_config, indent = "    "),
        product_config_labels = extract_labels(json_config),
        base_packages = base_packages,
        cache_packages = cache_packages,
        base_driver_packages = base_driver_packages,
        ota_configuration = ota_configuration,
        **kwarg
    )
