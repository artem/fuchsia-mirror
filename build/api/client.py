#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""A tool providing information about the Fuchsia build graph(s).

This is not intended to be called directly by developers, but by
specialized tools and scripts like `fx`, `ffx` and others.

See https://fxbug.dev/42084664 for context.
"""

# TECHNICAL NOTE: Reduce imports to a strict minimum to keep startup time
# of this script as low a possible. You can always perform an import lazily
# only when you need it (e.g. see how json and difflib are imported below).
import argparse
import os
import sys
from pathlib import Path
from typing import List, Sequence

_SCRIPT_FILE = Path(__file__)
_SCRIPT_DIR = _SCRIPT_FILE.parent
_FUCHSIA_DIR = (_SCRIPT_DIR / ".." / "..").resolve()
sys.path.insert(0, _SCRIPT_DIR)


def _get_host_platform() -> str:
    """Return host platform name, following Fuchsia conventions."""
    if sys.platform == "linux":
        return "linux"
    elif sys.platform == "darwin":
        return "mac"
    else:
        return os.uname().sysname


def _get_host_arch() -> str:
    """Return host CPU architecture, following Fuchsia conventions."""
    host_arch = os.uname().machine
    if host_arch == "x86_64":
        return "x64"
    elif host_arch.startswith(("armv8", "aarch64")):
        return "arm64"
    else:
        return host_arch


def _get_host_tag() -> str:
    """Return host tag, following Fuchsia conventions."""
    return "%s-%s" % (get_host_platform(), get_host_arch())


def _warning(msg: str):
    """Print a warning message to stderr."""
    if sys.stderr.isatty():
        print(f"\033[1;33mWARNING:\033[0m {msg}", file=sys.stderr)
    else:
        print(f"WARNING: {msg}", file=sys.stderr)


def _error(msg: str):
    """Print an error message to stderr."""
    if sys.stderr.isatty():
        print(f"\033[1;31mERROR:\033[0m {msg}", file=sys.stderr)
    else:
        print(f"ERROR: {msg}", file=sys.stderr)


def _printerr(msg) -> int:
    """Like _error() but returns 1."""
    _error(msg)
    return 1


# NOTE: Do not use dataclasses because its import adds 20ms of startup time
# which is _massive_ here.
class BuildApiModule:
    """Simple dataclass-like type describing a given build API module."""

    def __init__(self, name: str, file_path: Path):
        self.name = name
        self.path = file_path

    def get_content(self):
        """Return content as sttring."""
        return self.path.read_text()

    def get_content_as_json(self) -> object:
        """Return content as a JSON object + lazy-loads the 'json' module."""
        import json

        return json.load(self.path.open())


class BuildApiModuleList(object):
    """Models the list of all build API module files."""

    def __init__(self, build_dir: Path):
        self._modules: List[BuildApiModule] = []
        self.list_path = build_dir / "build_api_client_info"
        if not self.list_path.exists():
            return

        for line in self.list_path.read_text().splitlines():
            name, equal, file_path = line.partition("=")
            assert equal == "=", f"Invalid format for input file: {list_path}"
            self._modules.append(BuildApiModule(name, build_dir / file_path))

        self._modules.sort(key=lambda x: x.name)  # Sort by name.
        self._map = {m.name: m for m in self._modules}

    def empty(self) -> bool:
        """Return True if modules list is empty."""
        return len(self._modules) == 0

    def modules(self) -> Sequence[BuildApiModule]:
        """Return the sequence of BuildApiModule instances, sorted by name."""
        return self._modules

    def find(self, name: str) -> BuildApiModule:
        """Find a BuildApiModule by name, return None if not found."""
        return self._map.get(name)

    def names(self) -> Sequence[str]:
        """Return the sorted list of build API module names."""
        return [m.name for m in self._modules]


class OutputsDatabase(object):
    """Manage a lazily-created / updated NinjaOutputsTabular database.

    Usage is:
        1) Create instance.
        2) Call load() to load the database from the Ninja build directory.
        3) Call gn_label_to_paths() or path_to_gn_label() as many times
           as needed.
    """

    def __init__(self):
        self._database = None

    def load(self, build_dir: Path) -> bool:
        """Load the database from the given build directory.

        This takes care of converting the ninja_outputs.json file generated
        by GN into the more efficient tabular format, whenever this is needed.

        Args:
          build_dir: Ninja build directory.

        Returns:
          On success return True, on failure, print an error message to stderr
          then return False.
        """
        json_file = build_dir / "ninja_outputs.json"
        tab_file = build_dir / "ninja_outputs.tabular"
        if not json_file.exists():
            if tab_file.exists():
                tab_file.unlink()
            print(
                f"ERROR: Missing Ninja outputs file: {json_file}",
                file=sys.stderr,
            )
            return False

        from gn_ninja_outputs import NinjaOutputsTabular as OutputsDatabase

        self._database = OutputsDatabase()

        if (
            not tab_file.exists()
            or tab_file.stat().st_mtime < json_file.stat().st_mtime
        ):
            # Re-generate database file when needed
            self._database.load_from_json(json_file)
            self._database.save_to_file(tab_file)
        else:
            # Load previously generated database.
            self._database.load_from_file(tab_file)
        return True

    def gn_label_to_paths(self, label: str) -> List[str]:
        return self._database.gn_label_to_paths(label)

    def path_to_gn_label(self, path: str) -> str:
        return self._database.path_to_gn_label(path)


def get_build_dir(fuchsia_dir: Path) -> Path:
    """Get current Ninja build directory."""
    file = fuchsia_dir / ".fx-build-dir"
    if not file.exists():
        return Path()
    return fuchsia_dir / file.read_text().strip()


def cmd_list(args: argparse.Namespace) -> int:
    """Implement the `list` command."""
    for name in args.modules.names():
        print(name)
    return 0


def cmd_print(args: argparse.Namespace) -> int:
    """Implement the `print` command."""
    module = args.modules.find(args.api_name)
    if not module:
        return _printerr(
            f"Unknown build API module name {args.api_name}, must be one of:\n\n %s\n"
            % "\n ".join(args.modules.names())
        )

    if not module.path.exists():
        return _printerr(
            f"Missing input file, please use `fx set` or `fx gen` command: {module.path}"
        )

    print(module.get_content())
    return 0


def cmd_print_all(args: argparse.Namespace) -> int:
    """Implement the `print_all` command."""
    result = {}
    for module in args.modules.modules():
        if module.name != "api":
            result[module.name] = {
                "file": os.path.relpath(module.path, args.build_dir),
                "json": module.get_content_as_json(),
            }
    import json

    if args.pretty:
        print(
            json.dumps(result, sort_keys=True, indent=2, separators=(",", ": "))
        )
    else:
        print(json.dumps(result, sort_keys=True))
    return 0


def cmd_ninja_path_to_gn_label(args: argparse.Namespace) -> int:
    """Implement the `ninja_path_to_gn_label` command."""
    outputs = OutputsDatabase()
    if not outputs.load(args.build_dir):
        return 1

    failure = False
    labels = set()
    for path in args.paths:
        label = outputs.path_to_gn_label(path)
        if label:
            labels.add(label)
            continue

        if args.allow_unknown:
            labels.add(path)
            continue

        print(
            f"ERROR: Unknown Ninja target path: {path}",
            file=sys.stderr,
        )
        failure = True

    if failure:
        return 1

    print("\n".join(sorted(labels)))
    return 0


def _get_target_cpu(build_dir: Path) -> str:
    args_json_path = build_dir / "args.json"
    if not args_json_path.exists():
        return "unknown_cpu"
    import json

    args_json = json.load(args_json_path.open())
    if not isinstance(args_json, dict):
        return "unknown_cpu"
    return args_json.get("target_cpu", "unknown_cpu")


def cmd_gn_label_to_ninja_paths(args: argparse.Namespace) -> int:
    """Implement the `gn_label_to_ninja_paths` command."""
    outputs = OutputsDatabase()
    if not outputs.load(args.build_dir):
        return 1

    failure = False
    all_paths = []
    for label in args.labels:
        paths = outputs.gn_label_to_paths(label)
        if paths:
            all_paths.extend(paths)
            continue
        if args.allow_unknown:
            all_paths.append(label)
            continue
        _error(f"Unknown GN label (not in the configured graph): {label}")
        failure = True

    if failure:
        return 1

    for path in sorted(all_paths):
        print(path)
    return 0


def cmd_fx_build_args_to_labels(args: argparse.Namespace) -> int:
    outputs = OutputsDatabase()
    if not outputs.load(args.build_dir):
        return 1

    from gn_labels import GnLabelQualifier

    host_cpu = args.host_tag.split("-")[1]
    target_cpu = _get_target_cpu(args.build_dir)
    qualifier = GnLabelQualifier(host_cpu, target_cpu)

    failure = False

    def ninja_path_to_gn_label(path: str) -> str:
        label = outputs.path_to_gn_label(path)
        if label:
            label_args = qualifier.label_to_build_args(label)
            _warning(
                f"Use '{' '.join(label_args)}' instead of Ninja path '{path}'"
            )
            return label

        if args.allow_unknown:
            return path

        _error(f"Unknown Ninja path: {path}")
        nonlocal failure
        failure = True
        return ""

    qualifier.set_ninja_path_to_gn_label(ninja_path_to_gn_label)

    labels = qualifier.build_args_to_labels(args.args)
    if failure:
        return 1

    for label in labels:
        print(label)

    return 0


def main(main_args: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawTextHelpFormatter
    )
    parser.add_argument(
        "--fuchsia-dir",
        default=_FUCHSIA_DIR,
        type=Path,
        help="Specify Fuchsia source directory.",
    )
    parser.add_argument(
        "--build-dir",
        type=Path,
        help="Specify Ninja build directory.",
    )
    parser.add_argument(
        "--host-tag",
        help="Host platform tag, using Fuchsia conventions (auto-detected).",
        # NOTE: Do not set a default with _get_host_tag() here for fastser startup,
        # since the //build/api/client wrapper script will always set this option.
    )
    subparsers = parser.add_subparsers(required=True, help="sub-command help.")
    print_parser = subparsers.add_parser(
        "print",
        help="Print build API module content.",
        description="Print the content of a given build API module, given its name. "
        "Use the 'list' command to print the list of all available names.",
    )
    print_parser.add_argument("api_name", help="Name of build API module.")
    print_parser.set_defaults(func=cmd_print)

    print_all_parser = subparsers.add_parser(
        "print_all",
        help="Print single JSON containing the content of all build API modules.",
    )
    print_all_parser.add_argument(
        "--pretty", action="store_true", help="Pretty print the JSON output."
    )
    print_all_parser.set_defaults(func=cmd_print_all)

    list_parser = subparsers.add_parser(
        "list",
        help="Print list of all build API module names.",
        description="Print list of all build API module names.",
    )
    list_parser.set_defaults(func=cmd_list)

    ninja_path_to_gn_label_parser = subparsers.add_parser(
        "ninja_path_to_gn_label",
        help="Print the GN label of a given Ninja output path.",
    )
    ninja_path_to_gn_label_parser.add_argument(
        "--allow-unknown",
        action="store_true",
        help="Keep unknown input Nija paths in result.",
    )
    ninja_path_to_gn_label_parser.add_argument(
        "paths",
        metavar="NINJA_PATH",
        nargs="+",
        help="Ninja output path, relative to the build directory.",
    )
    ninja_path_to_gn_label_parser.set_defaults(func=cmd_ninja_path_to_gn_label)

    gn_label_to_ninja_paths_parser = subparsers.add_parser(
        "gn_label_to_ninja_paths",
        help="Print the Ninja output paths of one or more GN labels.",
        description="Print the Ninja output paths of one or more GN labels.",
    )
    gn_label_to_ninja_paths_parser.add_argument(
        "--allow-unknown",
        action="store_true",
        help="Keep unknown input GN labels in result.",
    )
    gn_label_to_ninja_paths_parser.add_argument(
        "labels",
        metavar="GN_LABEL",
        nargs="+",
        help="A qualified GN label (begins with //, may include full toolchain suffix).",
    )
    gn_label_to_ninja_paths_parser.set_defaults(
        func=cmd_gn_label_to_ninja_paths
    )

    fx_build_args_to_labels_parser = subparsers.add_parser(
        "fx_build_args_to_labels",
        help="Parse fx build arguments into qualified GN labels.",
        description="Convert a series of `fx build` arguments into a list of fully qualified GN labels.",
    )
    fx_build_args_to_labels_parser.add_argument(
        "--allow-unknown", action="store_true"
    )
    fx_build_args_to_labels_parser.add_argument(
        "--args", required=True, nargs=argparse.REMAINDER
    )
    fx_build_args_to_labels_parser.set_defaults(
        func=cmd_fx_build_args_to_labels
    )

    args = parser.parse_args(main_args)

    if not args.build_dir:
        args.build_dir = get_build_dir(args.fuchsia_dir)

    if not args.build_dir.exists():
        return _printerr(
            "Could not locate build directory, please use `fx set` command or use --build-dir=DIR.",
        )

    if not args.host_tag:
        args.host_tag = get_host_tag()

    args.modules = BuildApiModuleList(args.build_dir)
    if args.modules.empty():
        return _printerr(
            f"Missing input file, did you run `fx gen` or `fx set`?: {args.modules.list_path}"
        )

    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
