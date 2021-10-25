#!/usr/bin/env python3.8
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Create a Component Manifest from a list of items"""

import json
import argparse


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--distribution_manifest_file',
        type=argparse.FileType('r', encoding='UTF-8'),
        help=
        'Path to a distribution_manifest file which is a JSON file that contains driver paths.'
    )
    parser.add_argument(
        '--output',
        type=argparse.FileType('w', encoding='UTF-8'),
        help='The path where this script will output the driver manifest.')
    parser.add_argument(
        '--is_v1',
        action='store_const',
        const=True,
        default=False,
        help=
        'Generates a DFv1 component manifest. (If this is not included a DFv2 manifest is generated)'
    )
    parser.add_argument(
        '--colocate',
        action='store_true',
        help='If this exists then the driver should be colocated with its parent'
    )
    args = parser.parse_args()

    distribution_manifest = json.load(args.distribution_manifest_file)

    program = False
    bind = False

    for entry in distribution_manifest:
        destination = entry['destination']
        if destination.startswith("driver/"):
            if destination == "driver/compat.so":
                continue
            if program:
                raise Exception(
                    "fuchsia_driver_component cannot depend on two drivers: " +
                    program + " " + destination)
            program = destination
        if destination.startswith("meta/bind/"):
            if bind:
                raise Exception(
                    "fuchsia_driver_component cannot depend on two bind programs: "
                    + bind + " " + destination)
            bind = destination

    if not program:
        raise Exception("fuchsia_driver_component must contain a driver")
    if not bind:
        raise Exception("fuchsia_driver_component must contain a bind file")

    manifest = {
        'program': {
            'runner': 'driver',
            'binary': program,
            'bind': bind
        }
    }
    if args.is_v1:
        manifest["program"]["binary"] = "driver/compat.so"
        manifest["program"]["compat"] = program
    else:
        manifest["program"]["binary"] = program

    if args.colocate:
        manifest["program"]["colocate"] = "true"

    json_manifest = json.dumps(manifest)
    args.output.write(json_manifest)


if __name__ == "__main__":
    main()
