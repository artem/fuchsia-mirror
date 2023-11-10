#!/usr/bin/env fuchsia-vendored-python

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import sys
from typing import Sequence, Dict, AbstractSet, Any
from pathlib import Path


class Package:
    def __init__(self, manifest: Path, ignored_blobs: Sequence[str] = None):
        pkg = json.load(manifest)
        self.repository: str = pkg.get("repository", "")
        self.blobs: Dict[str, str] = {}

        ignored_blobs = ignored_blobs or []

        for blob in pkg.get("blobs", []):
            path = blob["path"]
            merkle = blob["merkle"]
            # Special case the meta/ blob
            if path == "meta/":
                self.meta_merkle = blob
            elif path not in ignored_blobs:
                self.blobs[path] = blob

    def blob_paths(self) -> AbstractSet[str]:
        return set(self.blobs.keys())

    def blob_merkle(self, path) -> str:
        if path in self.blobs:
            return self.blobs[path]["merkle"]
        else:
            return {}

    def blob_size(self, path) -> int:
        if path in self.blobs:
            return self.blobs[path]["size"]
        else:
            return 0


def calculate_diff(
    gn_package: Package, bazel_package: Package, size_check_blobs: Sequence[str]
) -> Sequence[str]:
    # If the blobs have the same merkle for their meta/ directory then they can
    # be considered the same and we will return no findings. However, if they
    # are not the same we need to check each individual blob. We need to do this
    # for 2 reasons:
    #  1) If the meta files differ we can only find out what the differences are
    #     by extracting the far contents and looking at each file. We already have
    #     most of this information, with the exception of cml files and bind objects,
    #     so we can just look at our package manifest to report our the diffs.
    #  2) We know that there are packages that have different content but we choose
    #     to ignore those files for the purposes of this verification via the
    #     ignored_blobs. The meta.far file contains a contents file which holds
    #     all of the blobs which means that even if we ignore a file the meta.far
    #     files will differ.
    if gn_package.meta_merkle == bazel_package.meta_merkle:
        return []

    findings: Sequence[str] = []
    if gn_package.repository != bazel_package.repository:
        findings.append(
            f"Repositories do not match '{gn_package.repository}' != '{bazel_package.repository}'"
        )

    # find all the blob diffs
    bazel_blobs: AbstractSet[str] = bazel_package.blob_paths()
    gn_blobs: AbstractSet[str] = gn_package.blob_paths()
    common_blobs: AbstractSet[str] = gn_blobs.intersection(bazel_blobs)

    def compare_blob_merkles(path):
        gn_merkle: int = gn_package.blob_merkle(path)
        bazel_merkle: int = bazel_package.blob_merkle(path)
        if gn_merkle != bazel_merkle:
            findings.append(
                f"Blobs at '{path}' have different merkle roots '{gn_merkle}' != '{bazel_merkle}'"
            )

    def compare_blob_size(path):
        gn_size: int = gn_package.blob_size(path)
        bazel_size: int = bazel_package.blob_size(path)
        if gn_size != bazel_size:
            findings.append(
                f"Blobs at '{path}' have different sizes '{gn_size}' != '{bazel_size}'"
            )

    for blob in common_blobs:
        if blob in size_check_blobs:
            compare_blob_size(blob)
        else:
            compare_blob_merkles(blob)

    for blob in gn_blobs.difference(common_blobs):
        findings.append(f"Blob at '{blob}' only exists in gn package")

    for blob in bazel_blobs.difference(common_blobs):
        findings.append(f"Blob at '{blob}' only exists in bazel package")

    return findings


def main(argv: Sequence[str]):
    parser = argparse.ArgumentParser(description="Compares drivers")
    parser.add_argument(
        "--gn-package-manifest", type=argparse.FileType("r"), required=True
    )
    parser.add_argument(
        "--bazel-package-manifest", type=argparse.FileType("r"), required=True
    )
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    parser.add_argument(
        "--blobs-to-ignore",
        nargs="*",
        default=[],
        help="List of blob install paths to ignore.",
        required=False,
    )
    parser.add_argument(
        "--size-check-blobs",
        nargs="*",
        default=[],
        help="List of blob install paths to ignore.",
        required=False,
    )
    args = parser.parse_args(argv)

    gn_package = Package(
        args.gn_package_manifest, ignored_blobs=args.blobs_to_ignore
    )
    bazel_package = Package(
        args.bazel_package_manifest, ignored_blobs=args.blobs_to_ignore
    )

    findings = calculate_diff(gn_package, bazel_package, args.size_check_blobs)

    if len(findings) > 0:
        args.output.write("\n".join(findings) + "\n")
        return 1
    else:
        args.output.write("no issues\n")
        return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
