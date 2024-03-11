# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule that creates a product bundle for flashing, emulating, or updating a Fuchsia product to a target device."""

load("//fuchsia/private:ffx_tool.bzl", "get_ffx_product_bundle_inputs")
load("//fuchsia/private/workflows:fuchsia_product_bundle_tasks.bzl", "fuchsia_product_bundle_tasks")
load(
    ":providers.bzl",
    "FuchsiaAssemblyConfigInfo",
    "FuchsiaProductBundleInfo",
    "FuchsiaProductImageInfo",
    "FuchsiaRepositoryKeysInfo",
    "FuchsiaScrutinyConfigInfo",
    "FuchsiaVirtualDeviceInfo",
)

DELIVERY_BLOB_TYPE = struct(
    UNCOMPRESSED = "0",
    COMPRESSED = "1",
)

def fuchsia_product_bundle(
        *,
        name,
        board_name = None,
        # Deprecated. Use product_bundle_name.
        product_name = None,
        product_bundle_name = None,
        partitions_config = None,
        # Deprecated. Use main.
        product_image = None,
        main = None,
        **kwargs):
    """Build a fuchsia product bundle.

    This rule produces a fuchsia product bundle which can be used to flash or OTA a device.

    This macro will expand out into several fuchsia tasks that can be run by a
    bazel invocation. Given a product bundle definition, the following targets will be
    created.

    ```
    fuchsia_product_bundle(
        name = "product_bundle",
        board_name = "<your_board>",
        product_name = "<your_product_name>",
        partitions_config = ":your_partitions_config",
        product = ":your_image",
    )
    ```
    - product_bundle.flash: Calling run on this target will flash the device with the created product_bundle.
    - product_bundle.ota: Calling run on this target will ota the device with the created product_bundle.
    - product_bundle.zip: Calling build on this target will create a zipped version of the product_bundle.
    - product_bundle.emu: <TBA>.
    """

    _build_fuchsia_product_bundle(
        name = name,
        board_name = board_name,
        partitions_config = partitions_config,
        product_image = main or product_image,
        product_name = product_bundle_name or product_name,
        **kwargs
    )

    _build_zipped_product_bundle(
        name = name + ".zip",
        product_bundle = name,
    )

    fuchsia_product_bundle_tasks(
        name = "%s_tasks" % name,
        product_bundle = name,
    )

def _build_zipped_product_bundle_impl(ctx):
    pb_zip = ctx.actions.declare_file(ctx.label.name)
    product_bundle = ctx.attr.product_bundle[FuchsiaProductBundleInfo].product_bundle

    ctx.actions.run_shell(
        inputs = ctx.files.product_bundle,
        outputs = [pb_zip],
        command = """(cd $PRODUCT_BUNDLE_PATH && zip -r pb.zip . 1> /dev/null)
            mv $PRODUCT_BUNDLE_PATH/pb.zip $PB_ZIP_PATH
        """,
        env = {
            "PRODUCT_BUNDLE_PATH": product_bundle.path,
            "PB_ZIP_PATH": pb_zip.path,
        },
        progress_message = "Creating zipped product bundle for %s" % ctx.label.name,
    )

    return [DefaultInfo(files = depset(direct = [pb_zip]))]

_build_zipped_product_bundle = rule(
    doc = """Creates zipped product bundle.""",
    implementation = _build_zipped_product_bundle_impl,
    attrs = {
        "product_bundle": attr.label(
            doc = "The Product Bundle to be zipped",
            providers = [FuchsiaProductBundleInfo],
            mandatory = True,
        ),
    },
)

def _scrutiny_validation(
        ctx,
        ffx_tool,
        pb_out_dir,
        scrutiny_config,
        platform_scrutiny_config,
        is_recovery = False):
    deps = []
    ffx_invocation = [
        ffx_tool.path,
        "--isolate-dir $FFX_ISOLATE_DIR",
        "scrutiny",
        "verify",
    ]
    if is_recovery:
        ffx_invocation.append("--recovery")
    label_name = ctx.label.name + ("_recovery" if is_recovery else "")
    deps.append(_verify_bootfs_filelist(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        scrutiny_config.bootfs_files + platform_scrutiny_config.bootfs_files,
        scrutiny_config.bootfs_packages + platform_scrutiny_config.bootfs_packages,
    ))
    deps.append(_verify_kernel_cmdline(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        scrutiny_config.kernel_cmdline + platform_scrutiny_config.kernel_cmdline,
    ))
    deps += _verify_base_packages(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        scrutiny_config.static_packages + platform_scrutiny_config.static_packages,
    )
    deps.append(_verify_pre_signing(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        platform_scrutiny_config.pre_signing_policy,
    ))
    if not is_recovery:
        deps += _verify_route_sources(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            platform_scrutiny_config.routes_config_golden,
        )
        deps += _verify_component_resolver_allowlist(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            platform_scrutiny_config.component_resolver_allowlist,
        )
        deps += _verify_routes(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            platform_scrutiny_config.component_route_exceptions,
            scrutiny_config.component_tree_config,
        )
        deps += _verify_structured_config(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            platform_scrutiny_config.structured_config_policy,
        )
        deps += _extract_structured_config(
            ctx,
            ffx_tool,
            pb_out_dir,
            is_recovery,
        )
    return deps

def _verify_bootfs_filelist(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        zbi_bootfs_filelist_goldens,
        zbi_bootfs_packages_goldens):
    stamp_file = ctx.actions.declare_file(label_name + "_bootfs.stamp")
    ffx_isolate_dir = ctx.actions.declare_directory(label_name + "_bootfs.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "bootfs",
        "--product-bundle",
        pb_out_dir.path,
    ]
    for golden_file in zbi_bootfs_filelist_goldens:
        _ffx_invocation += [
            "--golden",
            golden_file.path,
        ]
    for golden_package in zbi_bootfs_packages_goldens:
        _ffx_invocation += [
            "--golden-packages",
            golden_package.path,
        ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir] + zbi_bootfs_filelist_goldens + zbi_bootfs_packages_goldens
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Bootfs file list for %s" % label_name,
    )
    return stamp_file

def _verify_kernel_cmdline(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        zbi_kernel_cmdline_goldens):
    stamp_file = ctx.actions.declare_file(label_name + "_kernel.stamp")
    ffx_isolate_dir = ctx.actions.declare_directory(label_name + "_kernel.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "kernel-cmdline",
        "--product-bundle",
        pb_out_dir.path,
    ]
    for golden_file in zbi_kernel_cmdline_goldens:
        _ffx_invocation += [
            "--golden",
            golden_file.path,
        ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir] + zbi_kernel_cmdline_goldens
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Kernel Cmdline for %s" % label_name,
    )
    return stamp_file

def _verify_route_sources(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        routes_config_golden):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_route.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_route.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_route.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "route-sources",
        "--product-bundle",
        pb_out_dir.path,
        "--config",
        routes_config_golden.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, routes_config_golden]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Route sources for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _verify_component_resolver_allowlist(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        component_resolver_allowlist):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_component_resolver.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_component_resolver.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_component_resolver.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "component-resolvers",
        "--product-bundle",
        pb_out_dir.path,
        "--allowlist",
        component_resolver_allowlist.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, component_resolver_allowlist]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Component Resolver for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _verify_routes(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        allow_lists,
        component_tree_config):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_routes.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_routes.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_routes.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "routes",
        "--product-bundle",
        pb_out_dir.path,
    ]
    if component_tree_config:
        _ffx_invocation += [
            "--component-tree-config",
            component_tree_config.path,
        ]
    for allow_list in allow_lists:
        _ffx_invocation += [
            "--allowlist",
            allow_list.path,
        ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, component_tree_config] + allow_lists
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Routes for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _verify_base_packages(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        base_packages):
    stamp_file = ctx.actions.declare_file(label_name + "_static_pkgs.stamp")
    tmp_dir = ctx.actions.declare_directory(label_name + "_static_pkgs.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(label_name + "_static_pkgs.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "static-pkgs",
        "--product-bundle",
        pb_out_dir.path,
    ]
    for base_package_list in base_packages:
        _ffx_invocation += [
            "--golden",
            base_package_list.path,
        ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir] + base_packages
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Static pkgs for %s" % label_name,
    )
    return [stamp_file, tmp_dir]

def _verify_pre_signing(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        pre_signing_policy_file):
    stamp_file = ctx.actions.declare_file(label_name + "_pre_signing.stamp")
    ffx_isolate_dir = ctx.actions.declare_directory(label_name + "_pre_signing.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "pre-signing",
        "--product-bundle",
        pb_out_dir.path,
        "--policy",
        pre_signing_policy_file.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, pre_signing_policy_file]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify pre-signing checks for %s" % label_name,
    )
    return stamp_file

def _verify_structured_config(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        structured_config_policy):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_structured_config.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_structured_config.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_structured_config.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "structured-config",
        "--product-bundle",
        pb_out_dir.path,
        "--policy",
        structured_config_policy.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, structured_config_policy]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify structured config for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _extract_structured_config(ctx, ffx_tool, pb_out_dir, is_recovery):
    structured_config = ctx.actions.declare_file(ctx.label.name + "_structured_config")
    depfile = ctx.actions.declare_file(ctx.label.name + "_depfile")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_extract_structured_config.ffx")
    ffx_invocation = [
        ffx_tool.path,
        "--isolate-dir " + ffx_isolate_dir.path,
        "scrutiny",
        "extract",
        "structured-config",
        "--product-bundle",
        pb_out_dir.path,
        "--output",
        structured_config.path,
        # These args are currently required, but the outputs are ignored.
        "--build-path",
        pb_out_dir.path,
        "--depfile",
        depfile.path,
    ]
    if is_recovery:
        ffx_invocation.append("--recovery")
    script_lines = [
        "set -e",
        " ".join(ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [structured_config, depfile, ffx_isolate_dir],
        command = "\n".join(script_lines),
        progress_message = "Extract structured config for %s" % ctx.label.name,
    )
    return [structured_config, depfile]

def _build_fuchsia_product_bundle_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    partitions_configuration = ctx.attr.partitions_config[FuchsiaAssemblyConfigInfo].config
    if ctx.attr.main:
        system_a_out = ctx.attr.main[FuchsiaProductImageInfo].images_out
    else:
        system_a_out = ctx.attr.product_image[FuchsiaProductImageInfo].images_out
    ffx_tool = fuchsia_toolchain.ffx
    pb_out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    product_name = "{}.{}".format(ctx.attr.product_bundle_name or ctx.attr.product_name, ctx.attr.board_name)
    delivery_blob_type = ctx.attr.delivery_blob_type

    # In the future, the product bundles should be versioned independently of
    # the sdk version. So far they have been the same value.
    product_version = ctx.attr.product_bundle_version or ctx.attr.product_version or fuchsia_toolchain.sdk_id
    if not product_version:
        fail("product_version string must not be empty.")

    # Gather all the arguments to pass to ffx.
    ffx_invocation = [
        "$FFX",
        "--config \"product.experimental=true,sdk.root=$SDK_ROOT\"",
        "--isolate-dir $FFX_ISOLATE_DIR",
        "product",
        "create",
        "--product-name",
        product_name,
        "--product-version",
        product_version,
        "--partitions $PARTITIONS_PATH",
        "--system-a $SYSTEM_A_MANIFEST",
        "--out-dir $OUTDIR",
    ]

    if delivery_blob_type:
        ffx_invocation.append("--delivery-blob-type " + delivery_blob_type)

    # Gather the environment variables needed in the script.
    env = {
        "FFX": ffx_tool.path,
        "OUTDIR": pb_out_dir.path,
        "PARTITIONS_PATH": partitions_configuration.path,
        "SYSTEM_A_MANIFEST": system_a_out.path + "/images.json",
        "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
        "SDK_ROOT": ctx.attr._sdk_manifest.label.workspace_root,
    }

    # Gather all the inputs.
    inputs = ctx.files.partitions_config + ctx.files.product_image + ctx.files.main + get_ffx_product_bundle_inputs(fuchsia_toolchain)

    # Add virtual devices.
    for virtual_device in ctx.attr.virtual_devices:
        ffx_invocation.append("--virtual-device")
        ffx_invocation.append(virtual_device[FuchsiaVirtualDeviceInfo].config.path)
        inputs.append(virtual_device[FuchsiaVirtualDeviceInfo].config)
    if ctx.attr.default_virtual_device != None:
        ffx_invocation.append("--recommended-device")
        ffx_invocation.append(ctx.attr.default_virtual_device[FuchsiaVirtualDeviceInfo].device_name)

    # If recovery is supplied, add it to the product bundle.
    if ctx.attr.recovery != None:
        system_r_out = ctx.attr.recovery[FuchsiaProductImageInfo].images_out
        ffx_invocation.append("--system-r $SYSTEM_R_MANIFEST")
        env["SYSTEM_R_MANIFEST"] = system_r_out.path + "/images.json"
        inputs.extend(ctx.files.recovery)

    # If update info is supplied, add it to the product bundle.
    if ctx.file.update_version_file != None:
        if ctx.attr.repository_keys == None:
            fail("Repository keys must be supplied in order to build an update package")
        ffx_invocation.extend([
            "--update-package-version-file $UPDATE_VERSION_FILE",
            "--update-package-epoch $UPDATE_EPOCH",
        ])
        env["UPDATE_VERSION_FILE"] = ctx.file.update_version_file.path
        env["UPDATE_EPOCH"] = ctx.attr.update_epoch
        inputs.append(ctx.file.update_version_file)

    # If a repository is supplied, add it to the product bundle.
    if ctx.attr.repository_keys != None:
        ffx_invocation.append("--tuf-keys $REPOKEYS")
        env["REPOKEYS"] = ctx.attr.repository_keys[FuchsiaRepositoryKeysInfo].dir
        inputs += ctx.files.repository_keys
    script_lines = [
        "set -e",
        "mkdir -p $FFX_ISOLATE_DIR",
        " ".join(ffx_invocation),
    ]

    script = "\n".join(script_lines)
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [pb_out_dir, ffx_isolate_dir],
        command = script,
        env = env,
        progress_message = "Creating product bundle for %s" % ctx.label.name,
    )
    deps = [pb_out_dir] + ctx.files.partitions_config + ctx.files.product_image + ctx.files.main

    # Scrutiny Validation
    main_config = ctx.attr.main_scrutiny_config or ctx.attr.product_image_scrutiny_config
    if main_config:
        if ctx.attr.main:
            build_type = ctx.attr.main[FuchsiaProductImageInfo].build_type
        else:
            build_type = ctx.attr.product_image[FuchsiaProductImageInfo].build_type
        if build_type == "user":
            platform_scrutiny_config = ctx.attr._platform_user_scrutiny_config[FuchsiaScrutinyConfigInfo]
        elif build_type == "userdebug":
            platform_scrutiny_config = ctx.attr._platform_userdebug_scrutiny_config[FuchsiaScrutinyConfigInfo]
        else:
            fail("scrutiny cannot run on 'product' because it is an eng build type")

        main_scrutiny_config = main_config[FuchsiaScrutinyConfigInfo]
        deps += _scrutiny_validation(ctx, ffx_tool, pb_out_dir, main_scrutiny_config, platform_scrutiny_config)
    if ctx.attr.recovery_scrutiny_config:
        build_type = ctx.attr.recovery[FuchsiaProductImageInfo].build_type
        if build_type == "user":
            platform_scrutiny_config = ctx.attr._platform_recovery_user_scrutiny_config[FuchsiaScrutinyConfigInfo]
        elif build_type == "userdebug":
            platform_scrutiny_config = ctx.attr._platform_recovery_userdebug_scrutiny_config[FuchsiaScrutinyConfigInfo]
        else:
            fail("scrutiny cannot run on 'recovery' because it is an eng build type")

        recovery_scrutiny_config = ctx.attr.recovery_scrutiny_config[FuchsiaScrutinyConfigInfo]
        deps += _scrutiny_validation(ctx, ffx_tool, pb_out_dir, recovery_scrutiny_config, platform_scrutiny_config, True)

    return [DefaultInfo(files = depset(direct = deps)), FuchsiaProductBundleInfo(
        product_bundle = pb_out_dir,
        is_remote = False,
    )]

_build_fuchsia_product_bundle = rule(
    doc = """Create a product bundle (PB) for flashing, emulating, or updating a Fuchsia product to a target device.""",
    implementation = _build_fuchsia_product_bundle_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "board_name": attr.string(
            doc = "Name of the board this PB runs on. E.g. qemu-x64.",
            mandatory = True,
        ),
        # Deprecated. Use product_bundle_name.
        "product_name": attr.string(
            doc = "Name of the Fuchsia product. E.g. workstation_eng.",
        ),
        "product_bundle_name": attr.string(
            doc = "Name of the Fuchsia product. E.g. workstation_eng.",
        ),
        # Deprecated. Use product_bundle_version.
        "product_version": attr.string(
            doc = "Version of the Fuchsia product. E.g. 35.20221231.0.1.",
        ),
        "product_bundle_version": attr.string(
            doc = "Version of the Fuchsia product. E.g. 35.20221231.0.1.",
        ),
        "delivery_blob_type": attr.string(
            doc = "Delivery blob type of the product bundle.",
            values = [DELIVERY_BLOB_TYPE.UNCOMPRESSED, DELIVERY_BLOB_TYPE.COMPRESSED],
        ),
        "partitions_config": attr.label(
            doc = "Partitions config to use.",
            mandatory = True,
        ),
        # Deprecated. Use main.
        "product_image": attr.label(
            doc = "fuchsia_product target to put in slot A.",
            providers = [FuchsiaProductImageInfo],
        ),
        "main": attr.label(
            doc = "fuchsia_product target to put in slot A.",
            providers = [FuchsiaProductImageInfo],
        ),
        "recovery": attr.label(
            doc = "fuchsia_product_image target to put in slot R.",
            providers = [FuchsiaProductImageInfo],
        ),
        "repository_keys": attr.label(
            doc = "A fuchsia_repository_keys target, which must be specified when update_version_file is specified.",
            providers = [FuchsiaRepositoryKeysInfo],
            default = None,
        ),
        "update_version_file": attr.label(
            doc = "Version file needed to create update package.",
            allow_single_file = True,
            default = None,
        ),
        "update_epoch": attr.string(
            doc = "Epoch needed to create update package.",
            default = "1",
        ),
        # Deprecated. Use main_scrutiny_config.
        "product_image_scrutiny_config": attr.label(
            doc = "Scrutiny config for slot A.",
            providers = [FuchsiaScrutinyConfigInfo],
        ),
        "main_scrutiny_config": attr.label(
            doc = "Scrutiny config for slot A.",
            providers = [FuchsiaScrutinyConfigInfo],
        ),
        "recovery_scrutiny_config": attr.label(
            doc = "Scrutiny config for recovery.",
            providers = [FuchsiaScrutinyConfigInfo],
        ),
        "virtual_devices": attr.label_list(
            doc = "Virtual devices to make available.",
            providers = [FuchsiaVirtualDeviceInfo],
            default = [],
        ),
        "default_virtual_device": attr.label(
            doc = "Default virtual device to run when none is specified.",
            providers = [FuchsiaVirtualDeviceInfo],
            default = None,
        ),
        "_sdk_manifest": attr.label(
            allow_single_file = True,
            default = "@fuchsia_sdk//:meta/manifest.json",
        ),
        "_rebase_flash_manifest": attr.label(
            default = "//fuchsia/tools:rebase_flash_manifest",
            executable = True,
            cfg = "exec",
        ),
        "_platform_user_scrutiny_config": attr.label(
            doc = "Scrutiny config listing platform content of user products",
            providers = [FuchsiaScrutinyConfigInfo],
            default = "//fuchsia/private/assembly/goldens:user",
        ),
        "_platform_userdebug_scrutiny_config": attr.label(
            doc = "Scrutiny config listing platform content of userdebug products",
            providers = [FuchsiaScrutinyConfigInfo],
            default = "//fuchsia/private/assembly/goldens:userdebug",
        ),
        "_platform_recovery_user_scrutiny_config": attr.label(
            doc = "Scrutiny config listing platform content of recovery user products",
            providers = [FuchsiaScrutinyConfigInfo],
            default = "//fuchsia/private/assembly/goldens:user_recovery",
        ),
        "_platform_recovery_userdebug_scrutiny_config": attr.label(
            doc = "Scrutiny config listing platform content of recovery userdebug products",
            providers = [FuchsiaScrutinyConfigInfo],
            default = "//fuchsia/private/assembly/goldens:userdebug_recovery",
        ),
    },
)
