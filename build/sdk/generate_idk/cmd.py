#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generate an IDK from multiple subbuild manifests.

The `idk` build rule runs multiple "subbuilds", building the libraries (etc)
that go into the IDK for each (cpu, API level) combination. Each of these runs
produces a "build manifest" file (the kind produced by the `sdk_molecule` rule),
listing all the files that make up each atom, along with additional metadata.

This command takes the list of those manifests (as well as some top-level IDK
metadata), and builds a single IDK directory, with symlinks to the relevant
files.
"""

import argparse
import json
import os
import pathlib
import sys

import depfile
import generate_idk


# rmtree manually removes all subdirectories and files instead of using
# shutil.rmtree, to avoid registering spurious reads on stale
# subdirectories. See https://fxbug.dev/42153728.
def rmtree(path: pathlib.Path) -> None:
    if not os.path.exists(path):
        return
    for root, dirs, files in os.walk(path, topdown=False):
        for file in files:
            os.unlink(os.path.join(root, file))
        for dir in dirs:
            full_path = os.path.join(root, dir)
            if os.path.islink(full_path):
                os.unlink(full_path)
            else:
                os.rmdir(full_path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--subbuild-directory",
        help="List of paths to each of the subbuilds",
        action="append",
        type=pathlib.Path,
        required=True,
    )
    parser.add_argument(
        "--relative-manifest",
        help="Relative path to the build manifest within each subbuild directory",
        type=pathlib.Path,
        required=True,
    )
    parser.add_argument(
        "--output-directory",
        type=pathlib.Path,
        help="Path where the IDK will be built",
        required=True,
    )
    parser.add_argument(
        "--target-arch",
        help="List of target architectures supported by the IDK",
        action="append",
        required=True,
    )
    parser.add_argument(
        "--host-arch", help="Architecture of host tools", required=True
    )
    parser.add_argument(
        "--release-version",
        help="Version identifier for the IDK",
        required=True,
    )
    parser.add_argument(
        "--stamp-file",
        help="Path to the stamp file",
        type=pathlib.Path,
        required=True,
    )
    parser.add_argument(
        "--depfile",
        help="Path to the stamp file",
        type=pathlib.Path,
    )
    args = parser.parse_args()

    # Collect all possible input files, to make a depfile.
    input_files: set[pathlib.Path] = set()

    merged = generate_idk.MergedIDK()
    for build_dir in args.subbuild_directory:
        manifest = generate_idk.PartialIDK.load(
            build_dir, args.relative_manifest
        )
        input_files |= manifest.input_files()
        merged = merged.merge_with(manifest)

    output_dir: pathlib.Path = args.output_directory

    # NOTE: Delete any directory that may already be there from a
    # previous run, which could contain stale junk. This is very
    # important because everything in the resulting directory will
    # be shipped!
    rmtree(output_dir)

    # Write metadata for each atom.
    for path, meta in merged.atoms.items():
        dest_path = output_dir / path
        dest_path.parent.mkdir(exist_ok=True, parents=True)
        with dest_path.open("w") as f:
            json.dump(meta, f, indent=2, sort_keys=True)

    # Symlink all the other files.
    for dest, src in merged.dest_to_src.items():
        dest_path = output_dir / dest
        dest_path.parent.mkdir(exist_ok=True, parents=True)
        dest_path.symlink_to(os.path.relpath(src, dest_path.parent))

    # Write the overall manifest.
    manifest_json = merged.sdk_manifest_json(
        host_arch=args.host_arch,
        target_arch=args.target_arch,
        release_version=args.release_version,
    )

    (output_dir / "meta").mkdir(exist_ok=True)
    with (output_dir / "meta/manifest.json").open("w") as meta_file:
        json.dump(
            manifest_json,
            meta_file,
            indent=2,
            sort_keys=True,
        )

    args.stamp_file.touch()

    if args.depfile:
        depfile_path: pathlib.Path = args.depfile
        depfile_path.parent.mkdir(parents=True, exist_ok=True)
        with depfile_path.open("w") as depfile_out:
            depfile.DepFile.from_deps(
                str(args.stamp_file), set(str(d) for d in input_files)
            ).write_to(depfile_out)

    return 0


if __name__ == "__main__":
    sys.exit(main())
