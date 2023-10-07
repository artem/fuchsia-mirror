#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generates licenses SPDX from GN metadata."""

import argparse
import sys
import logging
from file_access import FileAccess
from spdx_writer import SpdxWriter
from readme_fuchsia import ReadmesDB
from collector import Collector
from pathlib import Path
from gn_license_metadata import GnLicenseMetadataDB
from spdx_comparator import SpdxComparator


def main():
    """
    Generates licenses SPDX json file from GN license metadata.
    """
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--generated-license-metadata",
        type=str,
        required=True,
        help="Path to the license GN metadata file, generated via the GN genereated_file action",
    )
    parser.add_argument(
        "--fuchsia-source-path",
        type=str,
        required=True,
        help="The path to the root of the Fuchsia source tree",
    )
    parser.add_argument(
        "--spdx-root-package-name",
        type=str,
        required=True,
        help="The name of the SPDX root package",
    )
    parser.add_argument(
        "--spdx-output",
        type=str,
        required=True,
        help="Path to the output SPDX file",
    )
    parser.add_argument(
        "--dep-file",
        type=str,
        required=True,
        help="Path to the generated dep file",
    )
    parser.add_argument(
        "--fail-on-collector-errors",
        type=bool,
        default=False,
        help="Tool will fail when encountering collector errors",
    )
    # TODO(132725): Remove once migration completes.
    parser.add_argument(
        "--compare-with-legacy-spdx",
        type=str,
        default=None,
        help="The tool will compare the contents of the actual spdx output with the given legacy file",
    )

    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO, force="format='%(levelname)s:%(message)s'"
    )

    fuchsia_source_path = Path(args.fuchsia_source_path).expanduser()
    assert fuchsia_source_path.exists()

    file_access = FileAccess(fuchsia_source_path=fuchsia_source_path)

    readmes_db = ReadmesDB(file_access=file_access)

    # Collect licenses information
    collector = Collector(
        file_access=file_access,
        metadata_db=GnLicenseMetadataDB.from_file(
            Path(args.generated_license_metadata)
        ),
        readmes_db=readmes_db,
    )

    collector.collect()

    if collector.errors:
        if args.fail_on_collector_errors:
            collector.log_errors(log_level=logging.ERROR)
            return -1
        else:
            collector.log_errors(log_level=logging.WARNING)

    logging.info(f"Collection stats: {collector.stats}")

    # Generate an SPDX file:
    spdx_writer = SpdxWriter.create(
        root_package_name=args.spdx_root_package_name,
        file_access=file_access,
    )

    for collected_license in collector.unique_licenses:
        spdx_writer.add_license(
            public_package_name=collected_license.public_name,
            license_labels=collected_license.license_files,
            collection_hint=collected_license.debug_hint,
        )

    spdx_output_path = Path(args.spdx_output)
    spdx_writer.save(spdx_output_path)
    logging.info(
        f"Wrote spdx {spdx_output_path} (licenses={len(collector.unique_licenses)} size={spdx_output_path.stat().st_size})"
    )

    if args.compare_with_legacy_spdx:
        # Compare with legacy spdx file
        comparator = SpdxComparator(
            current_file=spdx_output_path,
            legacy_file=Path(args.compare_with_legacy_spdx),
        )
        comparator.compare()
        if comparator.found_differences():
            comparator.log_differences(logging.ERROR)
            return -1

    # Generate a GN depfile
    dep_file_path = Path(args.dep_file)
    logging.info(f"writing depfile {dep_file_path}")
    file_access.write_depfile(dep_file_path, main_entry=spdx_output_path)

    return 0


if __name__ == "__main__":
    sys.exit(main())
