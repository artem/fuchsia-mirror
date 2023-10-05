# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Python types for Assembly Input Bundles.

Assembly Input Bundles are a set of artifacts that need to be delivered to out-
of-tree (OOT) assembly as a unit, but whose contents should be opaque to the
delivery system itself.

"""
import functools
from collections import defaultdict
from dataclasses import dataclass, field
import os
import pathlib
import shutil
import json
from typing import (
    Any,
    Dict,
    List,
    ItemsView,
    Optional,
    Set,
    TextIO,
    Tuple,
    Union,
)

import serialization
from serialization import json_dump, json_dumps, json_load, serialize_json

from .image_assembly_config import ImageAssemblyConfig, KernelInfo
from .common import FileEntry, FilePath, fast_copy, fast_copy_makedirs
from .package_manifest import BlobEntry, PackageManifest, SubpackageEntry

__all__ = [
    "AssemblyInputBundle",
    "AIBCreator",
    "ConfigDataEntries",
    "DriverDetails",
]

PackageManifestList = List[FilePath]
PackageName = str
ComponentName = str
DepSet = Set[FilePath]
FileEntryList = List[FileEntry]
FileEntrySet = Set[FileEntry]
ComponentShards = Set[FilePath]
Merkle = str
BlobList = List[Tuple[Merkle, FilePath]]
SubpackageManifests = Dict[Merkle, FilePath]
ConfigDataEntries = Dict[PackageName, Set[FileEntry]]


class AssemblyInputBundleCreationException(Exception):
    """To be raised when AIB creation fails for some reason"""

    ...


class DuplicatePackageException(AssemblyInputBundleCreationException):
    """To be raised when an attempt is made to add multiple packages with the same name to the same
    invocation of the AIBCreator"""

    ...


class PackageManifestParsingException(Exception):
    """To be raised when an attempt to parse a json file into a PackageManifest object fails"""

    ...


@dataclass
@serialize_json
class DriverDetails:
    """Details for constructing a driver manifest fragment from a driver package"""

    package: FilePath = field()  # Path to the package manifest
    components: Set[FilePath] = field(default_factory=set)


@dataclass
@serialization.serialize_json
class PackageDetails:
    """Details for a package"""

    package: FilePath = field()  # Path to the package manifest
    set: str = field()  # Package set that includes the package

    def __hash__(self):
        """
        This intentionally only hashes the package manifest in order to
        deduplicate packages across package sets.
        """
        return hash(self.package)

    def __lt__(self, other):
        return self.package < other.package


@dataclass
@serialize_json
class CompiledPackageMainDefinition:
    """Primary definition of a compiled package which is created by Assembly"""

    name: str = field()  # Name of the package
    # Dictionary mapping components to cml files by name
    components: Dict[str, FilePath] = field(default_factory=dict)
    # Other files to include in the compiled package
    contents: Set[FileEntry] = field(default_factory=set)
    # CML files included by the component cml
    includes: Set[FilePath] = field(default_factory=set)
    # Whether to extract the contents of this package into bootfs
    bootfs_unpackaged: bool = field(default=False)


@dataclass
@serialize_json
class CompiledPackageAdditionalShards:
    """
    Additional contents for a package defined by a CompiledPackageMainDefinition
    """

    # Name of the package
    name: str = field()
    # Additional component shards
    component_shards: Dict[str, Set[FilePath]] = field(default_factory=dict)


@dataclass
@serialize_json
class AssemblyInputBundle:
    """AssemblyInputBundle wraps a set of artifacts together for use by out-of-tree assembly, both
    the manifest of the artifacts, and the artifacts themselves.

    The archived artifacts are placed into a nominal layout that is for readability, but the
    JSON manifest itself is the arbiter of what artifacts are in what categories:

    file layout::

        ./
        assembly_config.json
        packages/
            base/
                <package name>
            base_drivers/
                <package name>
            boot_drivers/
                <package name>
            cache/
                <package name>
            flexible/
                <package name>
            system/
                <package name>
            bootfs_packages/
                name_of_aib.bootfs_files_package
                <package name>
        blobs/
            <merkle>
        subpackages/
            <merkle>
        bootfs/
            path/to/file/in/bootfs
        config_data/
            <package name>/
                <file name>
        compiled_packages/
            include/
                path/to/shard/file/in/tree
            <compiled package name>/
                <component_name>/
                    name.shard.cml
                files/
                    path/to/file/in/package
        kernel/
            kernel.zbi
            multiboot.bin

    Files matching the patterns `packages/*/<package name>` and
    `subpackages/<merkle>` are JSON representations of the
    `PackageManifest` type (see `package_manifest.py`). (The `<merkle>` in
    `subpackages/` is the merkle of the subpackage's metafar.) A named package
    that is also referenced as a subpackage will have a copy of its manifest in
    both directories. (This is an intentional duplication to allow the
    simplification of the creation and use of the AIBs.)

    Each `PackageManifest` contains a `blobs` list of `BlobEntry` and an
    optional `subpackages` list of `SubpackageEntry`. The `source_path` of a
    `BlobEntry` references a file containing the blob bytes, in the `blobs/`
    directory. The `manifest_path` of a `SubpackageEntry` references another
    `PackageManifest` file in `subpackages/<merkle>`.

    assembly_config.json schema::

        {
            "packages": [
                {
                    "package": "package1",
                    "set": "base",
                },
                {
                    "package": "package2",
                    "set": "base",
                },
                {
                    "package": "package3",
                    "set": "cache",
                },
                {
                    "package": "packageS1",
                    "set": "system",
                },
                {
                    "package": "packageB1",
                    "set": "bootfs",
                },
            ],
            "base_drivers": [ "packageD1", ... ],
            "boot_drivers": [ "packageD1", ... ],
            "bootfs_files_package": "packages/packageB2",
            "bootfs_files": [
                { "destination": "path/in/bootfs", source: "path/in/layout" },
                ...
            ],
            "config_data": {
                "package1": [
                    { "destination": "path/in/data/dir", "source": "path/in/layout" },
                    { "destination": "path/in/data/dir", "source": "path/in/layout" },
                    ...
                ],
                ...
            },
            "blobs": [ "blobs/<hash1>", "blobs/<hash2>", ... ],
            "kernel": {
                "path": "kernel/kernel.zbi",
                "args": [ "arg1", "arg2", ... ],
                "clock_backstop": 01234
            },
            "qemu_kernel": "kernel/multiboot.bin",
            "boot_args": [ "arg1", "arg2", ... ],
            "shell_commands": {
                "package1":
                    ["path/to/binary1", "path/to/binary2"]
            },
            "packages_to_compile": [
               {
                   "name": "package_name",
                   "components": {
                       "component1": "path/to/component1.cml",
                       "component2": "path/to/component2.cml",
                   },
                   "contents": [
                       {
                           "source": "path/to/source",
                           "destination": "path/to/destination",
                       }
                   ]
               },
               {
                  "name": "package_name",
                  "component_shards": {
                       "component1": [
                           "path/to/shard1.cml",
                           "path/to/shard2.cml"
                       ]
                  }
               }
             ]
        }

    All items are optional.  Files for `config_data` should be in the config_data section,
    not in a package called `config_data`.
    """

    # Fields shared with ImageAssemblyConfig.
    kernel: KernelInfo = field(default_factory=KernelInfo)
    qemu_kernel: Optional[FilePath] = None
    boot_args: Set[str] = field(default_factory=set)
    bootfs_files: Set[FileEntry] = field(default_factory=set)
    bootfs_packages: Set[FilePath] = field(default_factory=set)

    packages: Set[PackageDetails] = field(default_factory=set)
    config_data: ConfigDataEntries = field(default_factory=dict)
    blobs: Set[FilePath] = field(default_factory=set)
    base_drivers: List[DriverDetails] = field(default_factory=list)
    boot_drivers: List[DriverDetails] = field(default_factory=list)
    shell_commands: Dict[str, List[str]] = field(
        default_factory=functools.partial(defaultdict, list)
    )
    packages_to_compile: List[
        Union[CompiledPackageMainDefinition, CompiledPackageAdditionalShards]
    ] = field(default_factory=list)
    bootfs_files_package: Optional[FilePath] = None

    def __repr__(self) -> str:
        """Serialize to a JSON string"""
        return serialization.json_dumps(self, indent=2)

    def add_packages(self, packages: List[PackageDetails]):
        for details in packages:
            # This 'in' check only looks at the package manifest file path and
            # ignores the package set. This is intentional in order to
            # deduplicate packages across sets.
            if details in self.packages:
                raise ValueError(f"Duplicate package {details.package}")
            self.packages.add(details)

    def all_file_paths(self) -> List[FilePath]:
        """Return a list of all files that are referenced by this AssemblyInputBundle."""
        file_paths = []
        file_paths.extend([p.package for p in self.packages])
        if self.kernel.path is not None:
            file_paths.append(self.kernel.path)
        if self.qemu_kernel is not None:
            file_paths.append(self.qemu_kernel)
        for entries in self.config_data.values():
            file_paths.extend([entry.source for entry in entries])
        if self.blobs is not None:
            file_paths.extend(self.blobs)

        for package in self.packages_to_compile:
            if isinstance(package, CompiledPackageMainDefinition):
                file_paths.extend(package.includes)
                file_paths.extend(package.components.values())
                file_paths.extend([entry.source for entry in package.contents])
            if isinstance(package, CompiledPackageAdditionalShards):
                file_paths.extend(
                    [
                        # flatten
                        component_path
                        for shards in package.component_shards.values()
                        for component_path in shards
                    ]
                )

        return file_paths

    def write_fini_manifest(
        self,
        file: TextIO,
        base_dir: Optional[FilePath] = None,
        rebase: Optional[FilePath] = None,
    ) -> None:
        """Write a fini-style manifest of all files in the AssemblyInputBundle
        to the given |file|.

        fini manifests are in the format::

          destination/path=path/to/source/file

        As all file paths in the AssemblyInputBundle are relative to the root of
        the bundle, `destination/path` is the existing path.  However, the path
        to the source file cannot be known (absolutely) without additional
        information.

        Arguments:
        - file -- The |TextIO| file to write to.
        - base_dir -- The folder to assume that file paths are relative from.
        - rebase -- The folder to make the source paths relative to, if `base_dir` is also provided.
          By default this is the cwd.

        If `base_dir` is given, it's used to construct the path to the source files, if not, the cwd
        is assumed to be the path the files are from.

        If `rebase` is also given, the path to the source files are then made relative to it.
        """
        file_paths = self.all_file_paths()
        if base_dir is not None:
            file_path_entries = [
                FileEntry(
                    os.path.relpath(os.path.join(base_dir, path), rebase), path
                )
                for path in file_paths
            ]
            file_path_entries += [
                FileEntry(
                    os.path.join(base_dir, "assembly_config.json"),
                    "assembly_config.json",
                )
            ]
        else:
            file_path_entries = [FileEntry(path, path) for path in file_paths]

        FileEntry.write_fini_manifest(file_path_entries, file)


class AIBCreator:
    """The AIBCreator is a builder for AIBs that will copy all the files into
    the correct layout for the AIB structure, rewriting package manifests
    as needed to make them relative to the AIB manifest location.

    The AIBCreator has fields that match the AIB itself, but isn't an AIB
    because the paths it contains are not valid for an AIB.
    """

    package_url_template = "{repository}/{package_name}"

    def __init__(self, outdir: FilePath):
        # The directory to create the AIB in.  The manifest will be placed in
        # the root of this dir.
        self.outdir = outdir

        # The package sets (paths to package manifests)
        self.base: Set[FilePath] = set()
        self.cache: Set[FilePath] = set()
        self.flexible: Set[FilePath] = set()
        self.system: Set[FilePath] = set()
        self.shell_commands: Dict[str, List[str]] = defaultdict(list)

        # The kernel info
        self.kernel = KernelInfo()
        self.boot_args: Set[str] = set()

        # The emulator kernel.
        self.qemu_kernel: Optional[FilePath] = None

        # Bootfs info
        self.bootfs_files: Set[FileEntry] = set()
        self.bootfs_files_package: Optional[FilePath] = None
        self.bootfs_packages: Set[FilePath] = set()

        # The config_data entries
        self.config_data: FileEntryList = []

        # Base driver package manifests
        self.base_drivers: Set[FilePath] = set()

        # Base driver component distribution manifests
        self.base_driver_component_files: List[dict] = list()

        # Additional base drivers directly specified without requiring
        # us to parse GN generated files
        self.provided_base_driver_details: List[dict] = list()

        # Boot driver package manifests
        self.boot_drivers: Set[FilePath] = set()

        # Boot driver component distribution manifests
        self.boot_driver_component_files: List[dict] = list()

        # Additional boot drivers directly specified without requiring
        # us to parse GN generated files
        self.provided_boot_driver_details: List[dict] = list()

        # A set containing all the unique packageUrls seen by the AIBCreator instance
        self.package_urls: Set[str] = set()

        # A set containing the unique subpackage merkles, used to keep track of
        # which subpackages have already been copied.
        self.subpackages: Set[Merkle] = set()

        # A list of CompiledPackageMainDefinitions from either a parsed json GN
        # scope, or directly set by the legacy AIB creator.
        self.compiled_packages: List[CompiledPackageMainDefinition] = list()

        # Additional component cml shards to include in package's components
        self.compiled_package_shards: List[CompiledPackageAdditionalShards] = []

    def build(self) -> Tuple[AssemblyInputBundle, FilePath, DepSet]:
        """
        Copy all the artifacts from the ImageAssemblyConfig into an AssemblyInputBundle that is in
        outdir, tracking all copy operations in a DepFile that is returned with the resultant bundle.

        Some notes on operation:
            - <outdir> is removed and recreated anew when called.
            - hardlinks are used for performance
            - the return value contains a list of all files read/written by the
            copying operation (ie. depfile contents)
        """
        # Remove the existing <outdir>, and recreate it and the "subpackages"
        # subdirectory.
        if os.path.exists(self.outdir):
            shutil.rmtree(self.outdir)
        os.makedirs(os.path.join(self.outdir, "subpackages"))

        # Track all files we read
        deps: DepSet = set()

        # Init an empty resultant AssemblyInputBundle
        result = AssemblyInputBundle()

        # Copy over the boot args and zbi kernel args, unchanged, into the resultant
        # assembly bundle
        result.boot_args = self.boot_args
        kernel_args = self.kernel.args
        if kernel_args:
            result.kernel.args = kernel_args
        kernel_backstop = self.kernel.clock_backstop
        if kernel_backstop:
            result.kernel.clock_backstop = kernel_backstop

        # Copy the manifests for the base package set into the assembly bundle
        (base_pkgs, base_blobs, base_deps) = self._copy_packages("base")
        deps.update(base_deps)
        result.add_packages([PackageDetails(m, "base") for m in base_pkgs])

        # Copy the manifests for the cache package set into the assembly bundle
        (cache_pkgs, cache_blobs, cache_deps) = self._copy_packages("cache")
        deps.update(cache_deps)
        result.add_packages([PackageDetails(m, "cache") for m in cache_pkgs])

        (
            flexible_pkgs,
            flexible_blobs,
            flexible_deps,
        ) = self._copy_packages("flexible")
        deps.update(flexible_deps)
        result.packages.update(
            [PackageDetails(m, "flexible") for m in flexible_pkgs]
        )

        # Copy base driver packages into the base driver list of the assembly bundle
        for d in self.provided_base_driver_details:
            if d.package not in self.base_drivers:
                self.base_drivers.add(d.package)
            else:
                raise ValueError(
                    f"Duplicate driver package {d} specified in"
                    " base drivers list: {self.base_drivers}"
                )
        (
            base_driver_pkgs,
            base_driver_blobs,
            base_driver_deps,
        ) = self._copy_packages("base_drivers")
        deps.update(base_driver_deps)

        (base_driver_details, base_driver_deps) = self._get_driver_details(
            self.base_driver_component_files,
            self.provided_base_driver_details,
            base_driver_pkgs,
        )
        result.base_drivers.extend(base_driver_details)
        deps.update(base_driver_deps)

        # Copy boot driver packages into the boot driver list of the assembly bundle
        for d in self.provided_boot_driver_details:
            if d.package not in self.boot_drivers:
                self.boot_drivers.add(d.package)
            else:
                raise ValueError(
                    f"Duplicate driver package {d} specified in"
                    " base drivers list: {self.boot_drivers}"
                )
        (
            boot_driver_pkgs,
            boot_driver_blobs,
            boot_driver_deps,
        ) = self._copy_packages("boot_drivers")
        deps.update(boot_driver_deps)

        (boot_driver_details, boot_driver_deps) = self._get_driver_details(
            self.boot_driver_component_files,
            self.provided_boot_driver_details,
            boot_driver_pkgs,
        )
        result.boot_drivers.extend(boot_driver_details)
        deps.update(boot_driver_deps)

        # Copy the manifests for the system package set into the assembly bundle
        (system_pkgs, system_blobs, system_deps) = self._copy_packages("system")
        deps.update(system_deps)
        result.add_packages([PackageDetails(m, "system") for m in system_pkgs])

        # Copy the manifests for the bootfs package set into the assembly bundle
        (bootfs_pkgs, bootfs_pkg_blobs, bootfs_pkg_deps) = self._copy_packages(
            "bootfs_packages"
        )
        deps.update(bootfs_pkg_deps)
        result.add_packages([PackageDetails(m, "bootfs") for m in bootfs_pkgs])

        if self.bootfs_files_package:
            (
                bootfs_files_pkg,
                bootfs_files_pkg_blobs,
                bootfs_files_pkg_deps,
            ) = self._copy_package_from_path(
                self.bootfs_files_package, "bootfs_packages"
            )
            deps.update(bootfs_files_pkg_deps)
            result.bootfs_files_package = bootfs_files_pkg
            bootfs_pkg_blobs.extend(bootfs_files_pkg_blobs)

        # Add shell_commands field to assembly_config.json field in AIBCreator
        result.shell_commands = self.shell_commands

        # Deduplicate all blobs by merkle, but don't validate unique sources for
        # each merkle, last one wins (we trust that in the in-tree build isn't going
        # to make invalid merkles).
        all_blobs = {}
        for merkle, source in [
            *base_blobs,
            *cache_blobs,
            *flexible_blobs,
            *base_driver_blobs,
            *boot_driver_blobs,
            *system_blobs,
            *bootfs_pkg_blobs,
        ]:
            all_blobs[merkle] = source

        # Copy all the blobs to their dir in the out-of-tree layout
        (all_blobs, blob_deps) = self._copy_blobs(all_blobs)
        deps.update(blob_deps)
        result.blobs = set(
            [os.path.relpath(blob_path) for blob_path in all_blobs]
        )

        # Copy the bootfs entries
        (bootfs, bootfs_deps) = self._copy_file_entries(
            self.bootfs_files, "bootfs"
        )
        deps.update(bootfs_deps)
        result.bootfs_files.update(bootfs)

        # Rebase the path to the kernel into the out-of-tree layout
        if self.kernel.path:
            kernel_src_path: Any = self.kernel.path
            kernel_filename = os.path.basename(kernel_src_path)
            kernel_dst_path = os.path.join("kernel", kernel_filename)
            result.kernel.path = kernel_dst_path

            # Copy the kernel itself into the out-of-tree layout
            local_kernel_dst_path = os.path.join(self.outdir, kernel_dst_path)
            deps.add(fast_copy_makedirs(kernel_src_path, local_kernel_dst_path))

        # Rebase the path to the qemu kernel into the out-of-tree layout
        if self.qemu_kernel:
            kernel_src_path: Any = self.qemu_kernel
            kernel_filename = os.path.basename(kernel_src_path)
            kernel_dst_path = os.path.join("kernel", kernel_filename)
            result.qemu_kernel = kernel_dst_path

            # Copy the kernel itself into the out-of-tree layout
            local_kernel_dst_path = os.path.join(self.outdir, kernel_dst_path)
            deps.add(fast_copy_makedirs(kernel_src_path, local_kernel_dst_path))

        all_copied_include_entries: Set[FilePath] = set()
        for package in self.compiled_packages:
            copied_component_cmls = {}
            for component_name, cml_file in package.components.items():
                copied_cml, component_deps = self._copy_component_shard(
                    cml_file,
                    package_name=package.name,
                    component_name=component_name,
                )
                copied_component_cmls[component_name] = copied_cml

                deps.update(component_deps)

            # This assumes that package.includes has actually been passed to the
            # AIB creator as Set[FileEntry] instead of a Set[FilePath].  This is
            # not ideal, but it allows the reuse of the MainPackageDefinition
            # type without any other changes.
            #
            # The FileEntries are only needed because of the SDK include paths
            # used by some component shards.
            #
            # TODO(): Remove the use of the 'include' statement in component shards
            # compiled by assembly, for all included cml files that aren't in the
            # SDK itself (these can be found via a separate path to the SDK's set
            # of cml include files).  These files should either be incorporated
            # into the body of the component shards in the AIB, or added to the
            # AIBs as another shard for the same component.
            #
            # Once that's complete, this whole mechanism can be removed.
            #
            # _copy_component_includes will check for and validate inconsistent
            # include entries.
            (
                copied_include_entries,
                component_includes_deps,
            ) = self._copy_component_includes(
                package.includes, all_copied_include_entries
            )
            deps.update(component_includes_deps)

            # Save the copied entries so we can check for inconsistent
            # duplicate include entries in other packages in this AIB
            # in the following loop iterations
            all_copied_include_entries.update(copied_include_entries)

            # The final copied includes to add to the AIB.
            copied_includes = set(
                map(lambda x: x.destination, copied_include_entries)
            )

            # Copy the package contents entries
            (copied_package_files, package_deps) = self._copy_file_entries(
                package.contents,
                os.path.join("compiled_packages", package.name, "files"),
            )

            copied_definition = CompiledPackageMainDefinition(
                name=package.name,
                components=copied_component_cmls,
                includes=copied_includes,
                contents=set(copied_package_files),
                bootfs_unpackaged=package.bootfs_unpackaged,
            )
            result.packages_to_compile.append(copied_definition)

            deps.update(package_deps)

        for package in self.compiled_package_shards:
            copied_component_shards = {}
            for name, shards in package.component_shards.items():
                copied_shards, component_deps = self._copy_component_shards(
                    shards, package_name=package.name, component_name=name
                )

                deps.update(component_deps)
                copied_component_shards[name] = set(copied_shards)

            result.packages_to_compile.append(
                CompiledPackageAdditionalShards(
                    name=package.name, component_shards=copied_component_shards
                )
            )

        # Copy the config_data entries into the out-of-tree layout
        (config_data, config_data_deps) = self._copy_config_data_entries()
        deps.update(config_data_deps)
        result.config_data = config_data

        # Sort the shell commands alphabetically
        result.shell_commands = dict(sorted(result.shell_commands.items()))

        # Write the AIB manifest
        assembly_config_path = os.path.join(self.outdir, "assembly_config.json")
        with open(assembly_config_path, "w") as file:
            result.json_dump(file, indent=2)

        return (result, assembly_config_path, deps)

    def _get_driver_details(
        self,
        driver_component_files: List[dict],
        provided_driver_details: List[dict],
        driver_pkgs: Set[FilePath],
    ) -> List[DriverDetails]:
        """Read the driver package manifests and produce DriverDetails for the AIB config"""
        driver_details_list: List[DriverDetails] = list()

        # The deps touched by this function.
        deps: DepSet = set()

        # Associate the set of base driver component files with their packages
        component_files: Dict[str, List[str]] = dict()
        for component_manifest in driver_component_files:
            with open(
                component_manifest["distribution_manifest"], "r"
            ) as manifest_file:
                manifest: List[Dict] = json.load(manifest_file)
                component_manifest_list = component_files.setdefault(
                    component_manifest["package_name"], []
                )
                component_manifest_list += [
                    f["destination"]
                    for f in manifest
                    if f["destination"].startswith("meta/")
                    and f["destination"].endswith(".cm")
                ]

            deps.add(component_manifest["distribution_manifest"])

        # Include the component lists which were provided directly for
        # packages instead of those which were generated by GN metadata walks
        for driver_details in provided_driver_details:
            with open(driver_details.package, "r") as file:
                try:
                    manifest = json_load(PackageManifest, file)
                except Exception as exc:
                    raise PackageManifestParsingException(
                        f"loading PackageManifest from {driver_details.package}"
                    ) from exc

                package_name = manifest.package.name
                if component_files.get(package_name):
                    raise ValueError(
                        f"Duplicate package {package_name}"
                        " specified in driver_packages and"
                        " provided_driver_details list"
                    )
                component_files[package_name] = driver_details.components

        for package_manifest_path in sorted(driver_pkgs):
            with open(
                os.path.join(self.outdir, package_manifest_path), "r"
            ) as file:
                try:
                    manifest = json_load(PackageManifest, file)
                except Exception as exc:
                    raise PackageManifestParsingException(
                        f"loading PackageManifest from {package_manifest_path}"
                    ) from exc

                package_name = manifest.package.name
                driver_details_list.append(
                    DriverDetails(
                        package_manifest_path,
                        # Include the driver components specified for this package
                        component_files[package_name],
                    )
                )

        return driver_details_list, deps

    def _copy_packages(
        self,
        set_name: str,
    ) -> Tuple[PackageManifestList, BlobList, DepSet]:
        """Copy package manifests to the assembly bundle outdir, returning the set of blobs
        that need to be copied as well (so that they blob copying can be done in a
        single, deduplicated step).
        """
        package_manifests = getattr(self, set_name)

        # Resultant paths to package manifests
        packages = []

        # All of the blobs to copy, deduplicated by merkle, and validated for
        # conflicting sources.
        blobs: BlobList = []

        # The deps touched by this function.
        deps: DepSet = set()

        # Bail early if empty
        if len(package_manifests) == 0:
            return (packages, blobs, deps)

        # Open each manifest, record the blobs, and then copy it to its destination,
        # sorted by path to the package manifest.
        for package_manifest_path in sorted(package_manifests):
            (manifest, blob_list, dep_set) = self._copy_package_from_path(
                package_manifest_path, set_name
            )
            if manifest:
                # Track the package manifest in our set of packages
                packages.append(manifest)
                blobs.extend(blob_list)
                deps.update(dep_set)

        return (packages, blobs, deps)

    def _copy_package_from_path(
        self,
        package_manifest_path: FilePath,
        set_name: str,
    ) -> Tuple[Optional[FilePath], BlobList, DepSet]:
        """Copy a package manifest by its path to the assembly bundle outdir,
        adding its blobs to the set of blobs that need to be copied as well.
        If the package has subpackages, recursively copy those as well, skipping
        any subpackages that have already been copied.
        """

        def validate_unique_packages(package_url, package_path):
            invalid = False
            if package_url in self.package_urls:
                invalid = True
                message = f"There is a duplicate declaration of {package_url} in {set_name}"
            if os.path.exists(package_path):
                invalid = True
                message = f"The package path {package_path} already exists, and can't be replaced."
            if invalid:
                raise DuplicatePackageException(message)
            self.package_urls.add(package_url)

        # All of the blobs to copy, deduplicated by merkle, and validated for
        # conflicting sources.
        blobs: BlobList = []

        # The deps touched by this function.
        deps: DepSet = set()

        with open(package_manifest_path, "r") as file:
            try:
                manifest = json_load(PackageManifest, file)
            except Exception as exc:
                raise PackageManifestParsingException(
                    f"loading PackageManifest from {package_manifest_path}"
                ) from exc

            package_name = manifest.package.name
            # Track in deps, since it was opened.
            deps.add(package_manifest_path)

        # Create the directory for the packages, now that we know it will exist
        packages_dir = os.path.join("packages", set_name)
        os.makedirs(os.path.join(self.outdir, packages_dir), exist_ok=True)

        # Path to which we will write the new manifest within the input bundle.
        rebased_destination = os.path.join(packages_dir, package_name)

        # Bail if we are trying to add a duplicate package
        validate_unique_packages(
            AIBCreator.package_url_template.format(
                repository=manifest.repository,
                package_name=manifest.package.name,
            ),
            rebased_destination,
        )

        # But skip config-data, if we find it.
        if "config-data" == package_name:
            return (None, [], [])

        try:
            self._copy_package(
                manifest,
                os.path.dirname(package_manifest_path),
                rebased_destination,
                blobs,
                deps,
            )
        except Exception as e:
            raise AssemblyInputBundleCreationException(
                f"Copying '{set_name}' package '{package_name}' with manifest: {package_manifest_path}"
            ) from e

        return (rebased_destination, blobs, deps)

    def _copy_package(
        self,
        manifest: PackageManifest,
        package_manifest_dir: FilePath,
        rebased_destination: FilePath,
        blobs: BlobList,
        deps: DepSet,
    ):
        """Copy a package manifest to the assembly bundle outdir, adding its
        blobs to the set of blobs that need to be copied as well. If the package
        has subpackages, recursively copy those as well, skipping any
        subpackages that have already been copied.
        """

        # Instead of copying the package manifest itself, the contents of the
        # manifest needs to be rewritten to reflect the new location of the
        # blobs within it.
        new_manifest = PackageManifest(manifest.package, [])
        new_manifest.repository = manifest.repository
        new_manifest.set_paths_relative(True)

        # For each blob in the manifest:
        #  1) add it to set of all blobs
        #  2) add it to the PackageManifest that will be written to the Assembly
        #     Input Bundle, using the correct source path for within the
        #     Assembly Input Bundle.
        for blob in manifest.blobs:
            source = blob.source_path
            if source is None:
                raise ValueError(
                    f"Found a blob with no source path: {blob.path}"
                )

            # Make the path relative to the package manifest if necessary.
            if manifest.blob_sources_relative == "file":
                source = os.path.join(package_manifest_dir, source)

            blobs.append((blob.merkle, source))

            blob_destination = _make_internal_blob_path(blob.merkle)
            relative_blob_destination = os.path.relpath(
                blob_destination, os.path.dirname(rebased_destination)
            )
            new_manifest.blobs.append(
                BlobEntry(
                    blob.path,
                    blob.merkle,
                    blob.size,
                    source_path=relative_blob_destination,
                )
            )

        for subpackage in manifest.subpackages:
            # Copy the SubpackageEntry to the new_manifest, with the
            # updated `subpackages/<merkle>` path
            subpackage_destination = _make_internal_subpackage_path(
                subpackage.merkle
            )
            relative_subpackage_destination = os.path.relpath(
                subpackage_destination, os.path.dirname(rebased_destination)
            )
            new_manifest.subpackages.append(
                SubpackageEntry(
                    subpackage.name,
                    subpackage.merkle,
                    manifest_path=relative_subpackage_destination,
                )
            )

            if subpackage.merkle not in self.subpackages:
                # This is a new subpackage. Track it and copy it and any of its
                # subpackages, recursively.
                self.subpackages.add(subpackage.merkle)

                # Make the path relative to the package manifest if necessary.
                subpackage_manifest_path = subpackage.manifest_path
                if manifest.blob_sources_relative == "file":
                    subpackage_manifest_path = os.path.join(
                        package_manifest_dir, subpackage_manifest_path
                    )

                with open(subpackage_manifest_path, "r") as file:
                    try:
                        subpackage_manifest = json_load(PackageManifest, file)
                    except Exception as exc:
                        raise PackageManifestParsingException(
                            f"loading PackageManifest from {subpackage_manifest_path}"
                        ) from exc

                    # Track in deps, since it was opened.
                    deps.add(subpackage_manifest_path)

                try:
                    self._copy_package(
                        subpackage_manifest,
                        os.path.dirname(subpackage_manifest_path),
                        subpackage_destination,
                        blobs,
                        deps,
                    )
                except Exception as e:
                    raise AssemblyInputBundleCreationException(
                        f"Copying subpackage '{subpackage.name}' with manifest: {subpackage_manifest_path}"
                    ) from e

        package_manifest_destination = os.path.join(
            self.outdir, rebased_destination
        )
        with open(package_manifest_destination, "w") as new_manifest_file:
            json_dump(new_manifest, new_manifest_file)

    def _copy_blobs(
        self, blobs: Dict[Merkle, FilePath]
    ) -> Tuple[List[FilePath], DepSet]:
        blob_paths: List[FilePath] = []
        deps: DepSet = set()

        # Bail early if empty
        if len(blobs) == 0:
            return (blob_paths, deps)

        # Create the directory for the blobs, now that we know it will exist.
        blobs_dir = os.path.join(self.outdir, "blobs")
        os.makedirs(blobs_dir)

        # Copy all blobs
        for merkle, source in blobs.items():
            blob_path = _make_internal_blob_path(merkle)
            blob_destination = os.path.join(self.outdir, blob_path)
            blob_paths.append(blob_path)
            deps.add(fast_copy(source, blob_destination))

        return (blob_paths, deps)

    def _copy_component_includes(
        self,
        component_includes: Union[FileEntrySet, FileEntryList],
        existing_shard_includes: FileEntrySet,
    ) -> Tuple[FileEntrySet, DepSet]:
        deps: DepSet = set()
        shard_includes: FileEntrySet = set()
        for entry in component_includes:
            rebased_destination = os.path.join(
                "compiled_packages", "include", entry.destination
            )
            copy_destination = os.path.join(self.outdir, rebased_destination)

            rebased_entry = FileEntry(entry.source, rebased_destination)

            # Check whether we have previously specified a different
            # source for the same include file
            if rebased_destination in map(
                lambda x: x.destination,
                existing_shard_includes | shard_includes,
            ):
                if rebased_entry not in existing_shard_includes:
                    raise AssemblyInputBundleCreationException(
                        f"Include file already exists with a different source: {copy_destination}"
                    )
            else:
                # Hardlink the file from the source to the destination
                deps.add(fast_copy_makedirs(entry.source, copy_destination))

            shard_includes.add(rebased_entry)

        return shard_includes, deps

    def _copy_component_shard(
        self, component_shard: FilePath, package_name: str, component_name: str
    ) -> Tuple[FilePath, DepSet]:
        deps: DepSet = set()
        # The shard is copied to a path based on the name of the package, the
        # name of the component, and the filename of the shard:
        # f"compiled_packages/{package_name}/{component_name}/{filename}"
        #
        bundle_destination = os.path.join(
            "compiled_packages",
            package_name,
            component_name,
            os.path.basename(component_shard),
        )

        # The copy destination is the above path, with the bundle's outdir.
        copy_destination = os.path.join(self.outdir, bundle_destination)

        # Hardlink the file from the source to the destination
        deps.add(fast_copy_makedirs(component_shard, copy_destination))
        return bundle_destination, deps

    def _copy_component_shards(
        self,
        component_shards: Union[ComponentShards, List[FilePath]],
        package_name: str,
        component_name: str,
    ) -> Tuple[List[FilePath], DepSet]:
        shard_file_paths: List[FilePath] = list()
        deps: DepSet = set()
        for shard in component_shards:
            destination, copy_deps = self._copy_component_shard(
                shard, package_name, component_name
            )
            deps.update(copy_deps)
            shard_file_paths.append(destination)
        return shard_file_paths, deps

    def _copy_file_entries(
        self, entries: Union[FileEntrySet, FileEntryList], subdirectory: str
    ) -> Tuple[FileEntryList, DepSet]:
        results: FileEntryList = []
        deps: DepSet = set()

        # Bail early if nothing to do
        if len(entries) == 0:
            return (results, deps)

        for entry in entries:
            rebased_destination = os.path.join(subdirectory, entry.destination)
            copy_destination = os.path.join(self.outdir, rebased_destination)

            # Hardlink the file from source to the destination, relative to the
            # directory for all entries.
            deps.add(fast_copy_makedirs(entry.source, copy_destination))

            # Make a new FileEntry, which has a source of the path within the
            # out-of-tree layout, and the same destination.
            results.append(
                FileEntry(
                    source=rebased_destination, destination=entry.destination
                )
            )

        return (results, deps)

    def _copy_config_data_entries(self) -> Tuple[ConfigDataEntries, DepSet]:
        """
        Take a list of entries for the config_data package, copy them into the
        appropriate layout for the assembly input bundle, and then return the
        config data entries and the set of DepEntries from the copying

        This expects the entries to be destined for:
        `meta/data/<package>/<path/to/file>`

        and creates a ConfigDataEntries dict of PackageName:FileEntryList.
        """
        results: ConfigDataEntries = {}
        deps: DepSet = set()

        if len(self.config_data) == 0:
            return (results, deps)

        # Make a sorted list of a deduplicated set of the entries.
        for entry in sorted(set(self.config_data)):
            # Crack the in-package path apart
            #
            # "meta" / "data" / package_name / path/to/file
            parts = pathlib.Path(entry.destination).parts
            if parts[:2] != ("meta", "data"):
                raise ValueError(
                    "Found an unexpected destination path: {}".format(parts)
                )
            package_name = parts[2]
            file_path = os.path.join(*parts[3:])

            rebased_source_path = os.path.join(
                "config_data", package_name, file_path
            )
            copy_destination = os.path.join(self.outdir, rebased_source_path)

            # Hardlink the file from source to the destination
            deps.add(fast_copy_makedirs(entry.source, copy_destination))

            # Append the entry to the set of entries for the package
            results.setdefault(package_name, set()).add(
                FileEntry(source=rebased_source_path, destination=file_path)
            )

        return (results, deps)


def _make_internal_blob_path(merkle: str) -> FilePath:
    """Common function to compute the destination path to a blob within the
    AssemblyInputBundle's folder hierarchy.
    """
    return os.path.join("blobs", merkle)


def _make_internal_subpackage_path(merkle: str) -> FilePath:
    """Common function to compute the destination path to a subpackage within
    the AssemblyInputBundle's folder hierarchy.
    """
    return os.path.join("subpackages", merkle)
