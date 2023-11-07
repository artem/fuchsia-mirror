# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Clang toolchain configuration.
"""

load(
    "@bazel_tools//tools/cpp:cc_toolchain_config_lib.bzl",
    "tool_path",
)
load(
    "@bazel_tools//tools/cpp:toolchain_utils.bzl",
    "find_cpp_toolchain",
    "use_cpp_toolchain",
)
load("@bazel_tools//tools/build_defs/cc:action_names.bzl", "ACTION_NAMES")
load("@fuchsia_clang//:cc_features.bzl", "features", "sanitizer_features")

def _cc_toolchain_config_impl(ctx):
    target_system_name = ctx.attr.cpu + "-unknown-fuchsia"
    tool_paths = [
        tool_path(
            name = "ar",
            path = "bin/llvm-ar",
        ),
        tool_path(
            name = "cpp",
            path = "bin/clang++",
        ),
        tool_path(
            name = "gcc",
            path = "bin/clang",
        ),
        tool_path(
            name = "lld",
            path = "bin/lld",
        ),
        tool_path(
            name = "objdump",
            path = "bin/llvm-objdump",
        ),
        tool_path(
            name = ACTION_NAMES.strip,
            path = "bin/llvm-strip",
        ),
        tool_path(
            name = "nm",
            path = "bin/llvm-nm",
        ),
        tool_path(
            name = "objcopy",
            path = "bin/llvm-objcopy",
        ),
        tool_path(
            name = "dwp",
            path = "/not_available/dwp",
        ),
        tool_path(
            name = "compat-ld",
            path = "/not_available/compat-ld",
        ),
        tool_path(
            name = "gcov",
            path = "/not_available/gcov",
        ),
        tool_path(
            name = "gcov-tool",
            path = "/not_available/gcov-tool",
        ),
        tool_path(
            name = "ld",
            path = "bin/ld.lld",
        ),
    ]

    return cc_common.create_cc_toolchain_config_info(
        ctx = ctx,
        toolchain_identifier = "crosstool-1.x.x-llvm-fuchsia-" + ctx.attr.cpu,
        host_system_name = "x86_64-unknown-linux-gnu",
        target_system_name = target_system_name,
        target_cpu = ctx.attr.cpu,
        target_libc = "fuchsia",
        compiler = "llvm",
        abi_version = "local",
        abi_libc_version = "local",
        tool_paths = tool_paths,
        # Implicit dependencies for Fuchsia system functionality
        cxx_builtin_include_directories = [
            "%sysroot%/include",  # Platform parts of libc.
            "%{CROSSTOOL_ROOT}/include/" + ctx.attr.cpu + "-unknown-fuchsia/c++/v1",  # Platform libc++.
            "%{CROSSTOOL_ROOT}/include/c++/v1",  # Platform libc++.
            "%{CROSSTOOL_ROOT}/lib/clang/%{CLANG_VERSION}/include",  # Platform libc++.
            "%{CROSSTOOL_ROOT}/lib/clang/%{CLANG_VERSION}/share",  # Platform libc++.
        ],
        builtin_sysroot = "%{SYSROOT_PATH_PREFIX}" + ctx.attr.cpu,
        cc_target_os = "fuchsia",
        features = [
            features.default_compile_flags,
            features.default_link_flags,
            features.dbg,
            features.opt,
            features.target_system_name(target_system_name),
            features.dependency_file,
            features.supports_pic,
            features.coverage,
            features.ml_inliner,
            features.static_cpp_standard_library,
        ] + sanitizer_features,
    )

cc_toolchain_config = rule(
    implementation = _cc_toolchain_config_impl,
    attrs = {
        "cpu": attr.string(mandatory = True, values = ["aarch64", "riscv64", "x86_64"]),
    },
    provides = [CcToolchainConfigInfo],
)

def _feature_flag(ctx):
    toolchain = find_cpp_toolchain(ctx)
    feature = ctx.attr.feature_name
    feature_configuration = cc_common.configure_features(
        ctx = ctx,
        cc_toolchain = toolchain,
        requested_features = ctx.features,
        unsupported_features = ctx.disabled_features,
    )
    return [config_common.FeatureFlagInfo(value = str(cc_common.is_enabled(
        feature_configuration = feature_configuration,
        feature_name = feature,
    )))]

# A flag whose value corresponds to whether a toolchain feature is enabled.
# For instance if `feature_name = "asan"` and you passed `--features=asan`
# then the value will be True.
feature_flag = rule(
    implementation = _feature_flag,
    attrs = {
        "feature_name": attr.string(),
        "_cc_toolchain": attr.label(default = Label("@bazel_tools//tools/cpp:current_cc_toolchain")),
    },
    fragments = ["cpp"],
    toolchains = use_cpp_toolchain(),
)
