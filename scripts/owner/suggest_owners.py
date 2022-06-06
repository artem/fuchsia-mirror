#!/usr/bin/env python3.8
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Suggests OWNERS for projects.

For each project in a given jiri manifest file or at a given path, do the
following:
1. Check if it has an OWNERS file at the root path. If so, skip.
2. Find references to the project via `gn refs`. If none, skip.
3. Find immediate owners for all referrers. If none, for each referrer travel
   one directory up and continue the search. Ignore root owners.

Example usage:
$ fx set ...
$ scripts/owner/suggest_owners.py \
--jiri_manifest integration/third_party/flower --csv csv.out

$ scripts/owner/suggest_owners.py --path third_party/crashpad --csv csv.out

$ scripts/owner/suggest_owners.py --path third_party/* --csv csv.out

$ scripts/owner/suggest_owners.py --path third_party/* --all_refs --csv csv.out

$ scripts/owner/suggest_owners.py --path third_party/* --all_refs --generate_missing
"""

import argparse
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

# Matches a valid email address, more or less.
EMAIL = re.compile("^[\w%+-]+@[\w.-]+\.[a-zA-Z]{2,}$")


def check_output(*command, **kwargs):
    try:
        return subprocess.check_output(
            command, stderr=subprocess.STDOUT, encoding="utf8", **kwargs)
    except subprocess.CalledProcessError as e:
        print("Failed: " + " ".join(command), file=sys.stderr)
        print(e.output, file=sys.stderr)
        raise e


def get_project_paths(jiri_manifest_path):
    manifest = ET.parse(jiri_manifest_path)
    root = manifest.getroot()
    projects = root.find("projects")
    return [project.get("path") for project in projects.findall("project")]


def get_referencing_paths(*args):
    build_dir = check_output("fx", "get-build-dir").strip()
    try:
        refs_out = check_output("fx", "gn", "refs", build_dir, *args)
    except Exception as e:
        print(f"Failed to find refs to {args}", file=sys.stderr)
        print(e.output, file=sys.stderr)
        return []
    # Remove empty lines, turn labels to paths, unique, sort
    return sorted(
        {line[2:].partition(":")[0] for line in refs_out.splitlines() if line})


def get_filenames(path):
    filenames = []
    for dirpath, dirnames, names in os.walk(path):
        for name in names:
            filepath = os.path.join(dirpath, name)
            filenames.append(filepath)
    return filenames


def get_owners(path):
    owners_path = os.path.join(path, "OWNERS")
    if not os.path.exists(owners_path):
        return set()
    owners = set()
    for line in open(owners_path):
        line = line.strip()
        if line.startswith("include "):
            owners.update(
                get_owners(line[len("include /"):(len(line) - len("/OWNERS"))]))
        elif line.startswith("per-file "):
            owners.update(
                line[len("per-file "):].partition("=")[2].strip().split(","))
        elif line.startswith("#"):
            continue
        else:
            owners.add(line)
    # Remove stuff that doesn't look like an email address
    return {owner for owner in owners if EMAIL.match(owner)}


def get_owners_file_path(path):
    # Look for the OWNERS file in the given path.
    owners_path = os.path.join(path, "OWNERS")
    if os.path.exists(owners_path):
        return owners_path
    # If not found, search one directory up.
    parent_path = os.path.dirname(path)
    if parent_path and parent_path != path:
        return get_owners_file_path(parent_path)
    return ""


def generate_owners_files(project_path, refs):
    # Find and include the OWNERS files of all references.
    includes = set()
    for ref in refs:
        path = get_owners_file_path(ref)
        if path:
            includes.add("include /" + path + "\n")

    generated_owners_path = os.path.join(project_path, "OWNERS")
    print(f"Generating {generated_owners_path}...")
    file = open(generated_owners_path, "w")
    file.write("".join(sorted(includes)))
    file.close()


def main():
    parser = argparse.ArgumentParser(description="Suggests owners for projects")
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--jiri_manifest", help="Input jiri manifest file")
    group.add_argument(
        "--path",
        nargs='+',
        help="Input project path, relative to fuchsia root")
    parser.add_argument(
        "--all_refs",
        action='store_true',
        help=
        "Search for references to all targets and all files in input projects")
    parser.add_argument(
        "--csv", help="Output csv file", type=argparse.FileType('w'))
    parser.add_argument(
        "--generate_missing",
        action='store_true',
        help="Generates OWNERS files for projects that are missing owners.")
    args = parser.parse_args()

    # Set working directory to //
    fuchsia_root = os.path.dirname(  # //
        os.path.dirname(             # scripts/
        os.path.dirname(             # owner/
        os.path.abspath(__file__))))
    os.chdir(fuchsia_root)

    if args.jiri_manifest:
        project_paths = get_project_paths(args.jiri_manifest)
    else:
        project_paths = [
            project.strip("/")
            for project in args.path
            if os.path.isdir(project)
        ]

    for project_path in project_paths:
        if get_owners(project_path):
            print(f"{project_path} has OWNERS, skipping.")
            continue
        # Search for references to any of the project's targets if `--all_refs`
        # is set, or for the top-level targets otherwise.
        refs = get_referencing_paths(
            project_path + ("/*" if args.all_refs else ":*"))
        if not refs and args.all_refs:
            print(
                f"{project_path} has no target references, searching for all file references."
            )
            files = get_filenames(project_path)
            refs = get_referencing_paths(project_path, *files)
        if not refs:
            print(f"{project_path} has no references, skipping.")
            continue
        # Filter // and internal references (paths inside the project)
        refs = {ref for ref in refs if ref and not ref.startswith(project_path)}

        owners = set()
        while not owners and refs:
            for ref in refs:
                owners.update(get_owners(ref))
            if not owners:
                # Go one directory up, terminating before //
                refs = {os.path.dirname(ref) for ref in refs}
                refs = {
                    ref for ref in refs
                    if ref and not ref.startswith(project_path)
                }
        if not owners:
            print(f"{project_path} not referenced by anything owned, skipping.")
            continue

        print()
        print(f"{project_path} suggested owners:")
        print("\n".join(sorted(owners)))
        print()
        print(f"This is based on incoming references from:")
        print("\n".join(sorted(refs)))
        print()
        args.csv.write(f"{project_path},{owners},{refs}\n")

        if args.generate_missing:
            generate_owners_files(project_path, refs)


if __name__ == "__main__":
    sys.exit(main())
