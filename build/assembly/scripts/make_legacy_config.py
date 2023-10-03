#!/usr/bin/env fuchsia-vendored-python
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Make the legacy configuration set

Create an ImageAssembly configuration based on the GN-generated config, after
removing any other configuration sets from it.
"""

import argparse
from collections import defaultdict
import json
import os
import sys
import logging
from typing import Dict, List, Set, Tuple, Optional

from assembly import (
    AssemblyInputBundle,
    AIBCreator,
    FileEntry,
    FilePath,
    ImageAssemblyConfig,
    PackageManifest,
    KernelInfo,
)
from assembly.assembly_input_bundle import (
    CompiledPackageAdditionalShards,
    DuplicatePackageException,
    PackageManifestParsingException,
    CompiledPackageMainDefinition,
)
from depfile import DepFile
from serialization import json_load

logger = logging.getLogger()

# Some type annotations for clarity
PackageManifestList = List[FilePath]
Merkle = str
BlobList = List[Tuple[Merkle, FilePath]]
FileEntryList = List[FileEntry]
FileEntrySet = Set[FileEntry]
DepSet = Set[FilePath]


def copy_to_assembly_input_bundle(
    base: List[FilePath],
    cache: List[FilePath],
    system: List[FilePath],
    bootfs_packages: List[FilePath],
    kernel: KernelInfo,
    boot_args: List[str],
    config_data_entries: FileEntryList,
    outdir: FilePath,
    base_driver_packages_list: List[str],
    base_driver_components_files_list: List[dict],
    boot_driver_packages_list: List[str],
    boot_driver_components_files_list: List[dict],
    shell_commands: Dict[str, List],
    core_realm_shards: List[FilePath],
    core_realm_includes: FileEntryList,
    bootfs_files_package: Optional[FilePath],
) -> Tuple[AssemblyInputBundle, FilePath, DepSet]:
    """
    Copy all the artifacts from the ImageAssemblyConfig into an AssemblyInputBundle that is in
    outdir, tracking all copy operations in a DepFile that is returned with the resultant bundle.

    Some notes on operation:
        - <outdir> is removed and recreated anew when called.
        - hardlinks are used for performance
        - the return value contains a list of all files read/written by the
        copying operation (ie. depfile contents)
    """
    aib_creator = AIBCreator(outdir)
    aib_creator.base.update(base)
    aib_creator.cache.update(cache)
    aib_creator.system.update(system)
    aib_creator.bootfs_packages.update(bootfs_packages)
    aib_creator.kernel = kernel
    aib_creator.boot_args.update(boot_args)

    aib_creator.base_drivers = set(base_driver_packages_list)

    if bootfs_files_package:
        aib_creator.bootfs_files_package = bootfs_files_package

    # Strip any base_driver and base pkgs from the cache set
    aib_creator.cache = aib_creator.cache.difference(
        aib_creator.base
    ).difference(aib_creator.base_drivers)
    aib_creator.base = aib_creator.base.difference(aib_creator.base_drivers)

    if len(aib_creator.base_drivers) != len(base_driver_packages_list):
        raise ValueError(
            f"Duplicate package specified "
            " in base_driver_packages: {base_driver_packages_list}"
        )
    aib_creator.base_driver_component_files = base_driver_components_files_list

    aib_creator.boot_drivers = set(boot_driver_packages_list)
    if len(aib_creator.boot_drivers) != len(boot_driver_packages_list):
        raise ValueError(
            f"Duplicate package specified "
            " in boot_driver_packages: {boot_driver_packages_list}"
        )
    aib_creator.boot_driver_component_files = boot_driver_components_files_list

    aib_creator.config_data = config_data_entries
    aib_creator.shell_commands = shell_commands

    if core_realm_shards:
        # Assume that the main cml for core is first (due to careful GN deps ordering)
        if os.path.basename(core_realm_shards[0]) != "core.cml":
            raise ValueError(
                f"The first shard listed must be the main 'core.cml' file: {core_realm_shards}"
            )

        core_package_definition = CompiledPackageMainDefinition("core")
        core_package_definition.components["core"] = core_realm_shards[0]

        # This is passing a Set[FileEntry] to what _should_ be a Set[FilePath],
        # but the aib_creator assumes that it's a set of FileEntry items in this
        # field as it does all the copies.  Not ideal, but it works for now.
        core_package_definition.includes = set(core_realm_includes)

        aib_creator.compiled_packages.append(core_package_definition)

        # Pass the rest as compiled_package_shards
        if len(core_realm_shards) > 1:
            additional_shards = CompiledPackageAdditionalShards(
                "core", {"core": set(core_realm_shards[1:])}
            )
            aib_creator.compiled_package_shards.append(additional_shards)

    return aib_creator.build()


def main():
    parser = argparse.ArgumentParser(
        description="Create an image assembly configuration"
    )

    parser.add_argument("--base-packages-list", type=argparse.FileType("r"))
    parser.add_argument("--cache-packages-list", type=argparse.FileType("r"))
    parser.add_argument(
        "--extra-files-packages-list", type=argparse.FileType("r")
    )
    parser.add_argument(
        "--extra-deps-files-packages-list", type=argparse.FileType("r")
    )
    parser.add_argument(
        "--kernel-cmdline", type=argparse.FileType("r"), required=True
    )
    parser.add_argument(
        "--kernel-clock-backstop", type=argparse.FileType("r"), required=True
    )
    parser.add_argument(
        "--kernel-image-metadata", type=argparse.FileType("r"), required=True
    )
    parser.add_argument("--kernel-image-name", required=True)
    parser.add_argument("--boot-args", type=argparse.FileType("r"))
    parser.add_argument("--bootfs-packages-list", type=argparse.FileType("r"))

    parser.add_argument("--config-data-entries", type=argparse.FileType("r"))
    parser.add_argument("--outdir", required=True)
    parser.add_argument("--depfile", type=argparse.FileType("w"))
    parser.add_argument("--export-manifest", type=argparse.FileType("w"))
    parser.add_argument(
        "--base-driver-packages-list", type=argparse.FileType("r")
    )
    parser.add_argument(
        "--base-driver-components-files-list", type=argparse.FileType("r")
    )
    parser.add_argument(
        "--boot-driver-packages-list", type=argparse.FileType("r")
    )
    parser.add_argument(
        "--boot-driver-components-files-list", type=argparse.FileType("r")
    )
    parser.add_argument(
        "--shell-commands-packages-list", type=argparse.FileType("r")
    )
    parser.add_argument("--core-realm-shards-list", type=argparse.FileType("r"))
    parser.add_argument(
        "--core-realm-includes-list", type=argparse.FileType("r")
    )
    parser.add_argument("--core-package-name", default="core")
    parser.add_argument("--bootfs-files-package", required=True)
    args = parser.parse_args()

    # Read in the config_data entries if available.
    if args.config_data_entries:
        config_data_entries = [
            FileEntry.from_dict(entry)
            for entry in json.load(args.config_data_entries)
        ]
    else:
        config_data_entries = []

    base_driver_packages_list = None
    if args.base_driver_packages_list:
        base_driver_packages_list = json.load(args.base_driver_packages_list)
        base_driver_components_files_list = json.load(
            args.base_driver_components_files_list
        )

    boot_driver_packages_list = None
    if args.boot_driver_packages_list:
        boot_driver_packages_list = json.load(args.boot_driver_packages_list)
        boot_driver_components_files_list = json.load(
            args.boot_driver_components_files_list
        )

    shell_commands = dict()
    shell_deps = set()
    if args.shell_commands_packages_list:
        shell_commands = defaultdict(set)
        for package in json.load(args.shell_commands_packages_list):
            manifest_path, package_name = (
                package["manifest_path"],
                package["package_name"],
            )
            with open(manifest_path, "r") as fname:
                package_aib = json_load(PackageManifest, fname)
                shell_deps.add(manifest_path)
                shell_commands[package_name].update(
                    {
                        blob.path
                        for blob in package_aib.blobs
                        if blob.path.startswith("bin/")
                    }
                )

    core_realm_shards: List[FilePath] = []
    core_realm_includes: FileEntryList = []
    if args.core_realm_shards_list:
        for shard in json.load(args.core_realm_shards_list):
            core_realm_shards.append(shard)

        # The source of a core realm include file is its location
        # relative to the root_build_dir, while the destination
        # is its location relative to the fuchsia root.
        for include in json.load(args.core_realm_includes_list):
            core_realm_includes.append(
                FileEntry(include["source"], include["destination"])
            )

    base = []
    if args.base_packages_list is not None:
        base_packages_list = json.load(args.base_packages_list)
        base = base_packages_list

    cache = []
    if args.cache_packages_list is not None:
        cache_packages_list = json.load(args.cache_packages_list)

        # Strip all base pkgs from the cache pkgs set, so there are no
        # duplicates across the two sets.
        if base:
            cache_packages_set = set(cache_packages_list)
            cache_packages_set.difference_update(base)
            cache_packages_list = list(sorted(cache_packages_set))

        cache = cache_packages_list

    system = []
    if args.extra_files_packages_list is not None:
        extra_file_packages = json.load(args.extra_files_packages_list)
        system.extend(extra_file_packages)
    if args.extra_deps_files_packages_list is not None:
        extra_deps_file_packages = json.load(
            args.extra_deps_files_packages_list
        )
        for extra_dep in extra_deps_file_packages:
            system.append(extra_dep["package_manifest"])

    # ZBI Config
    kernel_metadata = json.load(args.kernel_image_metadata)

    # The build_api_module("images") entry with name "kernel" and type "zbi"
    # is the kernel ZBI to include in the bootable ZBI.  There can be only one.
    [kernel_path] = [
        image["path"]
        for image in kernel_metadata
        if image["name"] == args.kernel_image_name and image["type"] == "zbi"
    ]
    kernel = KernelInfo()
    kernel.path = kernel_path
    kernel.args.update(json.load(args.kernel_cmdline))
    kernel.clock_backstop = json.load(args.kernel_clock_backstop)

    boot_args = []
    if args.boot_args is not None:
        boot_args = sorted(json.load(args.boot_args))

    bootfs_packages = []
    if args.bootfs_packages_list is not None:
        bootfs_packages_list = json.load(args.bootfs_packages_list)
        bootfs_packages = bootfs_packages_list

    # Create an Assembly Input Bundle from the remaining contents
    (
        assembly_input_bundle,
        assembly_config_manifest_path,
        deps,
    ) = copy_to_assembly_input_bundle(
        base,
        cache,
        system,
        bootfs_packages,
        kernel,
        boot_args,
        config_data_entries,
        args.outdir,
        base_driver_packages_list,
        base_driver_components_files_list,
        boot_driver_packages_list,
        boot_driver_components_files_list,
        {
            package: sorted(list(components))
            for (package, components) in shell_commands.items()
        },
        core_realm_shards,
        core_realm_includes,
        args.bootfs_files_package,
    )

    deps.update(shell_deps)
    # Write out a fini manifest of the files that have been copied, to create a
    # package or archive that contains all of the files in the bundle.
    if args.export_manifest:
        assembly_input_bundle.write_fini_manifest(
            args.export_manifest, base_dir=args.outdir
        )

    # Write out a depfile.
    if args.depfile:
        dep_file = DepFile(assembly_config_manifest_path)
        dep_file.update(deps)
        dep_file.write_to(args.depfile)


if __name__ == "__main__":
    try:
        main()
    except DuplicatePackageException as exc:
        logger.exception(
            "The Legacy Assembly Input Bundle could not be constructed due to \
        a duplicate package declaration in the build"
        )
    except PackageManifestParsingException as exc:
        logger.exception(
            "A problem occurred attempting to load a PackageManifest"
        )
    except AssemblyInputBundleCreationException as exc:
        logger.exception("A problem occured building the legacy bundle")
    finally:
        sys.exit()
