# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia assembly input bundle names."""

load("@fuchsia_icu_config//:constants.bzl", "icu_flavors")

# These are the user-buildtype-safe platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_USER_PLATFORM_AIB_NAMES = [
    "zircon",
    "bootstrap",
    "driver_framework_v1",
    "driver_framework_v2",
    "empty_live_usb",
    "emulator_support",
    "fshost_common",
    "fshost_fxfs",
    "fshost_fvm_blobfs",
    "fshost_fvm_f2fs",
    "fshost_fvm_fxfs",
    "fshost_fvm_minfs",
    "fshost_fvm_minfs_migration",
    "fshost_storage",
    "paravirtualization_support",
    "kernel_anonymous_memory_compression",
    "kernel_anonymous_memory_compression_eager_lru",
    "kernel_args_user",
    "live_usb",
    "resources",
    "virtcon",
]

# These are the userdebug platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_USERDEBUG_PLATFORM_AIB_NAMES = [
    "bootstrap_userdebug",
    "clock_development_tools",
    "kernel_args_userdebug",
]

# These are the eng-buildtype-safe platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_ENG_PLATFORM_AIB_NAMES = [
    "bootstrap_eng",
    "kernel_args_eng",
    "kernel_pmm_checker_enabled",
]

# This is the combined set of valid AIBs for "bringup" builds (which are the
# ones that need to use the bootstrap feature-set-level)
BRINGUP_PLATFORM_AIB_NAMES = BOOTSTRAP_USER_PLATFORM_AIB_NAMES + BOOTSTRAP_USERDEBUG_PLATFORM_AIB_NAMES + BOOTSTRAP_ENG_PLATFORM_AIB_NAMES

# The names of all of the platform's 'testonly=false' Assembly Input Bundles
# Please keep sorted, it makes merge conflicts less likely vs adding to the
# end of the list.
USER_PLATFORM_AIB_NAMES_RISCV64 = BOOTSTRAP_USER_PLATFORM_AIB_NAMES + [
    "audio_core",
    "audio_core_routing",
    "audio_core_use_adc_device",
    "audio_device_registry",
    "battery_manager",
    "bluetooth_core",
    "bluetooth_snoop_eager",
    "bluetooth_snoop_lazy",
    "brightness_manager",
    "camera",
    "common_minimal",
    "core_realm",
    "core_realm_networking",
    "core_realm_user_and_userdebug",
    "diagnostics_triage_detect_mali",
    "element_manager",
    "factory_data",
    "factory_reset_trigger",
    "fan",
    "feedback_large_disk",
    "feedback_low_memory_product_config",
    "feedback_remote_device_id_provider",
    "feedback_user_config",
    "feedback_userdebug_config",
    "fonts",
    "fonts_hermetic",
    "input_group_one",
    "input_group_two",
    "intl_services.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "intl_services.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "intl_services_small.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services_small.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "intl_services_small.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "intl_services_small_with_timezone.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services_small_with_timezone.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "intl_services_small_with_timezone.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "minimal_user",
    "netstack3",
    "netstack3_packages",
    "netstack3_packages_gub",
    "network_realm",
    "network_realm_packages",
    "network_realm_packages_gub",
    "network_tun",
    "networking_basic",
    "networking_basic_packages",
    "networking_basic_packages_gub",
    "networking_with_virtualization",
    "no_update_checker",
    "omaha_client",
    "radar_proxy_without_injector",
    "sensors_framework",
    "session_manager",
    "setui",
    "thread_lowpan",
    "setui.icu_default_{}".format(icu_flavors.default_git_commit),
    "setui.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "setui.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "setui_with_camera",
    "setui_with_camera.icu_default_{}".format(icu_flavors.default_git_commit),
    "setui_with_camera.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "setui_with_camera.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "starnix_support",
    "system_update_configurator",
    "ui",
    "ui_package_eng_userdebug_with_synthetic_device_support",
    "ui_package_user_and_userdebug",
    "ui_user_and_userdebug",
    "ui_user_and_userdebug.icu_default_{}".format(icu_flavors.default_git_commit),
    "ui_user_and_userdebug.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "ui_user_and_userdebug.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "wlan_base",
    "wlan_contemporary_privacy_only_support",
    "wlan_fullmac_support",
    "wlan_legacy_privacy_support",
    "wlan_softmac_support",
    "wlan_wlanix",
]
USER_PLATFORM_AIB_NAMES_EXCLUDED_FROM_RISCV64 = [
    # The following are not currently supported on RISC-V. They must be added
    # separately to each list that (indirectly) builds on
    # USER_PLATFORM_AIB_NAMES_RISCV64.

    # The Go toolchain used for netstack2 does not support RISC-V.
    # netstack-migration contains netstack2, so equally no support on RISC-V.
    "netstack2",
    "netstack_migration",
    "netstack_migration_packages",
    "netstack_migration_packages_gub",

    # TODO(fxbug.dev/128551): Move to the list above once build errors are
    # resolved and/or define a formal mechanism for AIBs to vary across
    # architectures.
    "virtualization_support",
]
USER_PLATFORM_AIB_NAMES = USER_PLATFORM_AIB_NAMES_RISCV64 + USER_PLATFORM_AIB_NAMES_EXCLUDED_FROM_RISCV64

USERDEBUG_PLATFORM_AIB_NAMES_RISCV64 = BOOTSTRAP_USERDEBUG_PLATFORM_AIB_NAMES + USER_PLATFORM_AIB_NAMES_RISCV64 + [
    "core_realm_development_access",
    "core_realm_development_access_rcs_no_usb",
    "core_realm_development_access_rcs_usb",
    "omaha_client_empty_eager_config",
    "minimal_userdebug",
    "minimal_userdebug_and_eng",
    "mdns_fuchsia_device_wired_service",
    "radar_proxy_with_injector",
    "sl4f",
    "wlan_development",
]
USERDEBUG_PLATFORM_AIB_NAMES = USERDEBUG_PLATFORM_AIB_NAMES_RISCV64 + USER_PLATFORM_AIB_NAMES_EXCLUDED_FROM_RISCV64

# The names of all of the platform's Assembly Input Bundles.
ENG_PLATFORM_AIB_NAMES_RISCV64 = BOOTSTRAP_ENG_PLATFORM_AIB_NAMES + USERDEBUG_PLATFORM_AIB_NAMES_RISCV64 + [
    "audio_development_support",
    "core_realm_eng",
    "example_assembly_bundle",
    "minimal_eng",
    "networking_test_collection",
    "system_update_checker",
    "pkgfs_disable_executability_restrictions",
    "testing_support",
    "ui_eng",
    "ui_eng.icu_default_{}".format(icu_flavors.default_git_commit),
    "ui_eng.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "ui_eng.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "ui_package_eng",
    "video_development_support",
]
ENG_PLATFORM_AIB_NAMES = ENG_PLATFORM_AIB_NAMES_RISCV64 + USER_PLATFORM_AIB_NAMES_EXCLUDED_FROM_RISCV64
