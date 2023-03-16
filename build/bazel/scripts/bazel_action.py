#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"Run a Bazel command from Ninja. See bazel_action.gni for details."

import argparse
import errno
import filecmp
import json
import os
import shlex
import shutil
import stat
import subprocess
import sys

_SCRIPT_DIR = os.path.dirname(__file__)

# NOTE: Assume this script is located under build/bazel/scripts/
_FUCHSIA_DIR = os.path.abspath(os.path.join(_SCRIPT_DIR, '..', '..', '..'))

# A list of built-in Bazel workspaces like @bazel_tools// which are actually
# stored in the prebuilt Bazel install_base directory with a timestamp *far* in
# the future, e.g. 2033-01-01. This is a hack that Bazel uses to determine when
# its install base has changed unexpectedly.
#
# If any file from these directories are listed in a depfile, they will force
# rebuilds on *every* Ninja invocations, because the tool will consider that
# the outputs are always older (as 2022 < 2033).
#
# This list is thus used to remove these from depfile inputs. Given that the
# files are part of Bazel's installation, their content would hardly change
# between build invocations anyway, so this is safe.
#
_BAZEL_BUILTIN_REPOSITORIES = ('@bazel_tools//',)

# Generated repositories are under bazel's control, so should not need any
# hashing examination, hopefully.
_BAZEL_IGNORE_GENERATED_REPOSITORIES = ('@fuchsia_icu_config//',)

# Technical notes on input (source and build files) located in Bazel external
# repositories.
#
# This script uses Bazel queries to extract the list of implicit input files
# from Bazel. This list typically includes a very large number of files that
# are located in Bazel external repositories, for example, consider the
# following labels:
#
#    @bazel_skylib//lib:BUILD
#    @platforms//cpu:BUILD
#    @rules_cc/cc:find_cc_toolchain.bzl
#    @fuchsia_clang//:cc_toolchain_config.bzl
#    @fuchsia_sdk//:BUILD.bazel
#    @fuchsia_sdk//pkg/fdio:include/lib/fdio/vfs.h
#    @fuchsia_sdk//fidl/fuchsia.unknown:unknown.fidl
#    @internal_sdk//src/lib/fxl_fxl:command_line.cc
#
# These labels need to be converted to actual file paths in the Bazel
# output_base, which consists in converting the repository name '@foo' into a
# repository directory such as `$BAZEL_TOPDIR/output_base/external/foo`.
#
# Simply listing these paths in the depfile unfortunately triggers flaky Ninja
# no-op check failures on our infra builders during incremental builds.
# Because Bazel will sometimes decide to re-generate the content of a
# repository (by re-running its repository rule function) under certain
# conditions, which can be triggered semi-randomly between two consecutive
# Ninja build actions.
#
# Also many times a repository `@foo` is regenerated because one of its
# repository dependency (e.g. `@bar`) has to be updated, but the new content
# for `@foo` would be identical nonetheless.
#
# This regeneration is mostly unpredictable, but can be worked-around in the
# following way, implemented in the script below:
#
# - Most repository directories are symlinks that simply point outside of the
#   Bazel workspace. This happens for any repository declared with
#   `local_repository()` in the WORKSPACE.bazel file. These are never
#   regenerated. For example, for @bazel_skylib:
#
#     $TOPDIR/output_base/external/bazel_skylib
#        --symlink--> $TOPDIR/workspace/third_party/bazel_skylib
#          --symlink--> $FUCHSIA_DIR/third_party/bazel_skylib
#
#   In this case, replacing the repository directory with its final target
#   directly is safe and cannot trigger no-op failures, so:
#
#      @bazel_skylib//lib:BUILD
#          ---mapped--> ../../third_party/bazel_skylib/lib/BUILD
#
#      @platforms//cpu:BUILD
#          ---mapped--> ../../third_party/bazel_platforms/cpu/BUILD
#
#   There is one important exception though: the @bazel_tools repository which
#   is best ignored entirely (see comment above this one).
#
# - Many repositories, even if their content is generated by a repository rule,
#   contain files or directories that are symlinks pointing outside of the
#   Bazel workspace. For example:
#
#      @fuchsia_sdk//pkg/fdio:include/lib/fdio/vfs.h
#        --symlink--> $NINJA_OUTPUT_DIR/sdk/exported/core/pkg/fdio/include/lib/fdio/vfs.h
#          --symlink--> $FUCHSIA_DIR/sdk/lib/fdio/include/lib/fdio/vfs.h
#
#      @fuchsia_clang//:lib
#         --symlink--> $FUCHSIA_DIR/prebuilt/third_party/clang/{host_tag}/lib
#
#   It is sufficient to replace the label with the final link target, as in:
#
#      @fuchsia)sdk//pkg/fdio:include/lib/fdio/vfs.h
#        --mapped--> ../../sdk/lib/fdio/include/lib/fdio/vfs.h
#
#      @fuchsia_clang//:lib/x86_64-unknown/fuchsia/libunwind.so.1
#        --mapped--> ../../prebuilt/third_party/clang/{host_tag}/lib/x86_64-unknown-fuchsia/libunwind.so.1
#
#   There is still a minor issue here: what if re-generating the repository
#   would change the symlink's content (i.e. pointing to a different location,
#   replacing the symlink with a regular file, or removing the symlink?). Then
#   the depfile would have recorded a now stale, and thus incorrect, implicit
#   input dependency.
#
#   Most of the time, this will create un-necessary build actions on the next
#   Ninja invocation, and in rare cases, may even break the build. However,
#   this is no different from regular Ninja issues with depfiles (see
#   https://fxbug.dev/83397), which cannot guarantee, by design, correct
#   incremental build.
#
#   So resolving the symlink seems an acceptable solution, as it would not be
#   worse than Ninja's current limitations.
#
# - As a special case, @legacy_ninja_build_outputs//:BUILD.bazel is generated
#   from the content of the file at:
#   $BAZEL_TOPDIR/legacy_ninja_build_outputs.inputs_manifest.json
#   which is never part of the GN or Ninja build plan (for reasons explained
#   in //build/bazel/legacy_ninja_build_outputs.gni).
#
# - Other files are generated by a repository rule, for example
#   @fuchsia_sdk//:BUILD.bazel, is generated by the fuchsia_sdk_repository()
#   rule implementation function, which reads the content of many input JSON
#   metadata files to determine what goes in that file.
#
#   Fortunately, this rule also uses a special file containing a content hash
#   for all these input metadata files, which is:
#
#     $BAZEL_TOPDIR/workspace/generated_repository_hashes/fuchsia_sdk.hash
#
#   This file is generated by the `update-workspace.py` script which is invoked
#   from the GN //build/bazel:generate_main_workspace target, and which also
#   creates depfile entries to link the fuchsia_sdk.hash output file to
#   all input manifests.
#
#   Thus if any of the input manifest file changes, Ninja will force re-running
#   the //build/bazel:generate_main_workspace action, which will update the
#   content hash file, which will force a Bazel build command invoked from
#   this bazel_action.py script to regenerate all of @fuchsia_sdk//.
#
#   For the depfile entries generated by this script, it is thus enough to
#   replace any label to a generated (non-symlink) repository file, to the path
#   to the corresponding content hash file, if it exists, e.g.:
#
#      @fuchsia_sdk//:BUILD.bazel
#        --mapped-->  $BAZEL_TOPDIR/workspace/generated_repository_hashes/fuchsia_sdk
#
#      @fuchsia_sdk//pkg/fdio:BUILD.bazel
#        --mapped-->  $BAZEL_TOPDIR/workspace/generated_repository_hashes/fuchsia_sdk
#
#   There are several files under $BAZEL_TOPDIR/generated_repository_hashes/,
#   each one matching a given repository name. So the algorithm used to map a
#   label like `@foo//:package/file` that does not point to a symlink is:
#
#      1) Lookup if a content hash file exists for repository @foo, e.g. look
#         for $BAZEL_TOPDIR/generated_repository_hashes/foo
#
#      2) If the content hash file exists, map the label directly to that file.
#
#            @foo//:package/file --mapped-->
#               $BAZEL_TOPDIR/generated_repository_hashes/foo.hash
#
#      3) If the content hash file does not exist, print an error message
#         or ignore the file (controlled by the _ASSERT_ON_IGNORED_FILES
#         definition below). Asserting is preferred to detect these cases
#         as soon as possible, but can be disabled locally during development.
#
# - Finally, files whose name is a content-based hash are ignored as well,
#   because adding them to the depfile can only lead to stale depfile entries
#   that will trigger Ninja errors.
#
#   This is because if the corresponding content changes, the file's name
#   also changes, and the file's timestamp becomes irrelevant and can only
#   trigger un-necessary rebuilds. Moreover, a stale output dependency can
#   appear as a Ninja no-op error that looks like:
#
#   output obj/build/images/fuchsia/fuchsia/legacy/blobs/0a892655d873b0431095e9b51e1dad42b520af43db7ef76aa2a08074948a1565 of phony edge with no inputs doesn't exist
#
#   Detecting content-based file names looks for files which only include
#   hexadecimal chars, and of at least 16 chars long, or which include
#   a .build-id/xx/ prefix, where <xx> is a 2-char hexadecimal string.
#

# Set this to True to debug operations locally in this script.
# IMPORTANT: Settings this to True will result in Ninja timeouts in CQ
# due to the stdout/stderr logs being too large.
_DEBUG = False

# Set this to True to assert when non-symlink repository files are found.
# This is useful to find them when performing expensive builds on CQ
_ASSERT_ON_IGNORED_FILES = True


def debug(msg):
    # Print debug message to stderr if _DEBUG is True.
    if _DEBUG:
        print('DEBUG: ' + msg, file=sys.stderr)


def copy_file_if_changed(src_path, dst_path):
    """Copy |src_path| to |dst_path| if they are different."""
    # NOTE: For some reason, filecmp.cmp() will return True if
    # dst_path does not exist, even if src_path is not empty!?
    if os.path.exists(dst_path) and filecmp.cmp(src_path, dst_path,
                                                shallow=False):
        return

    # Use lexists to make sure broken symlinks are removed as well.
    if os.path.lexists(dst_path):
        if os.path.isdir(dst_path):
            shutil.rmtree(dst_path)
        else:
            os.remove(dst_path)

    # See https://fxbug.dev/121003 for context.
    # If the file is writable, try to hard-link it directly. Otherwise,
    # or if hard-linking fails due to a cross-device link, do a simple
    # copy.
    do_copy = True
    file_mode = os.stat(src_path).st_mode
    is_src_readonly = file_mode & stat.S_IWUSR == 0
    if not is_src_readonly:
        try:
            os.link(src_path, dst_path)
            do_copy = False
        except OSError as e:
            if e.errno != errno.EXDEV:
                raise

    def make_writable(p):
        file_mode = os.stat(p).st_mode
        is_readonly = file_mode & stat.S_IWUSR == 0
        if is_readonly:
            os.chmod(p, file_mode | stat.S_IWUSR)

    def copy_writable(src, dst):
        shutil.copy2(src, dst)
        make_writable(dst)

    if do_copy:
        if os.path.isdir(src_path):
            shutil.copytree(src_path, dst_path, copy_function=copy_writable)
            # Make directories writable so their contents can be removed and
            # repopulated in incremental builds.
            make_writable(dst_path)
            for (root, dirs, _) in os.walk(dst_path):
                for d in dirs:
                    make_writable(os.path.join(root, d))
        else:
            copy_writable(src_path, dst_path)


_HEXADECIMAL_SET = set("0123456789ABCDEFabcdef")


def is_hexadecimal_string(s: str) -> bool:
    """Return True if input string only contains hexadecimal characters."""
    return all([c in _HEXADECIMAL_SET for c in s])


_BUILD_ID_PREFIX = '.build-id/'
_BUILD_ID_PREFIX_LEN = len(_BUILD_ID_PREFIX)


def is_likely_build_id_path(path: str) -> bool:
    """Return True if path is a .build-id/xx/yyyy* file name."""
    size = len(path)
    plen = _BUILD_ID_PREFIX_LEN

    # Look for .build-id/XX/ where XX is an hexadecimal string.
    pos = path.find(_BUILD_ID_PREFIX)
    if pos < 0:
        return False

    path = path[pos + len(_BUILD_ID_PREFIX):]
    if len(path) < 3 or path[2] != '/':
        return False

    return is_hexadecimal_string(path[0:2])


assert is_likely_build_id_path('/src/.build-id/ae/23094.so')
assert not is_likely_build_id_path('/src/.build-id/log.txt')


def is_likely_content_hash_path(path: str) -> bool:
    """Return True if file path is likely based on a content hash.

    Args:
        path: File path
    Returns:
        True if the path is a likely content-based file path, False otherwise.
    """
    # Look for .build-id/XX/ where XX is an hexadecimal string.
    if is_likely_build_id_path(path):
        return True

    # Look for a long hexadecimal sequence filename.
    filename = os.path.basename(path)
    return len(filename) >= 16 and is_hexadecimal_string(filename)


assert is_likely_content_hash_path('/src/.build-id/ae/23094.so')
assert not is_likely_content_hash_path('/src/.build-id/log.txt')


def find_bazel_execroot(workspace_dir):
    return os.path.normpath(
        os.path.join(workspace_dir, '..', 'output_base', 'execroot', 'main'))


class BazelLabelMapper(object):
    """Provides a way to map Bazel labels to file paths.

    Usage is:
      1) Create instance, passing the path to the Bazel workspace.
      2) Call source_label_to_path(<label>) where the label comes from
         a query.
    """

    def __init__(self, bazel_workspace, output_dir):
        # Get the $OUTPUT_BASE/external directory from the $WORKSPACE_DIR,
        # the following only works in the context of the Fuchsia platform build
        # because the workspace/ and output_base/ directories are always
        # parallel entries in the $BAZEL_TOPDIR.
        #
        # Another way to get it is to call `bazel info output_base` and append
        # /external to the result, but this would slow down every call to this
        # script, and is not worth it for now.
        #
        self._root_workspace = os.path.abspath(bazel_workspace)
        self._output_dir = os.path.abspath(output_dir)
        output_base = os.path.normpath(
            os.path.join(bazel_workspace, '..', 'output_base'))
        assert os.path.isdir(output_base), f'Missing directory: {output_base}'
        self._external_dir = os.path.join(output_base, 'external')

        # Some repositories have generated files that are associated with
        # a content hash file generated by update-workspace.py. This map is
        # used to return the path to such file if it exists, or an empty
        # string otherwise.
        self._repository_hash_map: Dict[str, str] = {}

    def _get_repository_content_hash(self, repository_name):
        """Check whether a repository name has an associated content hash file.

        Args:
            repository_name: Bazel repository name, must start with an @,
               e.g. '@foo' or '@@foo.1.0'

        Returns:
            If the corresponding repository has a content hash file, return
            its path. Otherwise, return an empty string.
        """
        hash_file = self._repository_hash_map.get(repository_name, None)
        if hash_file is None:
            # Canonical names like @@foo.<version> need to be converted to just `foo` here.
            file_prefix = repository_name[1:]
            if file_prefix.startswith('@'):
                name, dot, version = file_prefix[1:].partition('.')
                if dot == '.':
                    file_prefix = name
                else:
                    # No version, get rid of initial @@
                    file_prefix = file_prefix[1:]

            if file_prefix == 'legacy_ninja_build_outputs':
                hash_file = os.path.join(
                    self._root_workspace, '..',
                    'legacy_ninja_build_outputs.inputs_manifest.json')
            else:
                hash_file = os.path.join(
                    self._root_workspace, 'generated_repository_hashes',
                    file_prefix + '.hash')
                if not os.path.exists(hash_file):
                    hash_file = ""

            self._repository_hash_map[repository_name] = hash_file

        return hash_file

    def source_label_to_path(self, label, relative_to=None):
        """Convert a Bazel label to a source file into the corresponding file path.

        Args:
          label: A fully formed Bazel label, as return by a query. If BzlMod is
              enabled, this expects canonical repository names to be present
              (e.g. '@foo.12//src/lib:foo.cc' and no '@foo//src/lib:foo.cc').
          relative_to: Optional directory path string.
        Returns:
          If relative_to is None, the absolute path to the corresponding source
          file, otherwise, the same path relative to `relative_to`.

          This returns an empty string if the file should be ignored, i.e.
          not added to the depfile.
        """
        #
        # NOTE: Only the following input label formats are supported
        #
        #    //<package>:<target>
        #    @//<package>:<target>
        #    @<name>//<package>:<target>
        #    @@<name>.<version>//<package>:<target>
        #
        repository, sep, package_label = label.partition('//')
        assert sep == '//', f'Missing // in source label: {label}'
        if repository == '' or repository == '@':
            # @// references the root project workspace, it should normally
            # not appear in queries, but handle it here just in case.
            #
            # // references a path relative to the current workspace, but the
            # queries are always performed from the root project workspace, so
            # is equivalent to @// for this function.
            repository_dir = self._root_workspace
            is_external_repository = False
        else:
            # A note on canonical repository directory names.
            #
            # An external repository named 'foo' in the project's WORKSPACE.bazel
            # file will be stored under `$OUTPUT_BASE/external/foo` when BzlMod
            # is not enabled.
            #
            # However, it will be stored under `$OUTPUT_BASE/external/@foo.<version>`
            # instead when BzlMod is enabled, where <version> is determined statically
            # by Bazel at startup after resolving the dependencies expressed from
            # the project's MODULE.bazel file.
            #
            # It is not possible to guess <version> here but queries will always
            # return labels for items in the repository that look like:
            #
            #   @@foo.<version>//...
            #
            # This is called a "canonical label", this allows the project to use
            # @foo to reference the repository in its own BUILD.bazel files, while
            # a dependency module would call it @com_acme_foo instead. All three
            # labels will point to the same location.
            #
            # Queries always return canonical labels, so removing the initial @
            # and the trailing // allows us to get the correct repository directory
            # in all cases.
            assert repository.startswith('@'), (
                f'Invalid repository name in source label {label}')

            repository_dir = os.path.join(self._external_dir, repository[1:])
            is_external_repository = True

        package, colon, target = package_label.partition(':')
        assert colon == ':', f'Missing colon in source label {label}'
        path = os.path.join(repository_dir, package, target)

        # Check whether this path is a symlink to something else.
        # Use os.path.realpath() which always return an absolute path
        # after resolving all symlinks to their final destination, then
        # compare it with os.path.abspath(path):
        real_path = os.path.realpath(path)
        if real_path != os.path.abspath(path):
            path = real_path
        elif is_external_repository:
            # If the external repository has an associated content hash file,
            # use this directly. Otherwise ignore the file.
            path = self._get_repository_content_hash(repository)
            if not path:
                return ""

        if relative_to:
            path = os.path.relpath(path, relative_to)

        return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--bazel-launcher',
        required=True,
        help='Path to Bazel launcher script.')
    parser.add_argument(
        '--workspace-dir', required=True, help='Bazel workspace path')
    parser.add_argument(
        '--command',
        required=True,
        help='Bazel command, e.g. `build`, `run`, `test`')
    parser.add_argument(
        '--inputs-manifest',
        help=
        'Path to the manifest file describing Ninja outputs as bazel inputs.')
    parser.add_argument(
        '--bazel-inputs',
        default=[],
        nargs='*',
        help='Labels of GN bazel_input_xxx() targets for this action.')
    parser.add_argument(
        '--bazel-targets',
        action='append',
        default=[],
        help='List of bazel target patterns.')
    parser.add_argument(
        '--bazel-outputs', default=[], nargs='*', help='Bazel output paths')
    parser.add_argument(
        '--ninja-outputs',
        default=[],
        nargs='*',
        help='Ninja output paths relative to current directory.')
    parser.add_argument(
        '--fuchsia-dir',
        help='Path to Fuchsia source directory, auto-detected.')
    parser.add_argument('--depfile', help='Ninja depfile output path.')
    parser.add_argument(
        "--allow-directory-in-outputs",
        action="store_true",
        default=False,
        help=
        'Allow directory outputs in `--bazel-outputs`, NOTE timestamps on directories do NOT accurately reflect content freshness, which can lead to incremental build incorrectness.'
    )
    parser.add_argument('extra_bazel_args', nargs=argparse.REMAINDER)

    args = parser.parse_args()

    if not args.bazel_targets:
        return parser.error('A least one --bazel-targets value is needed!')

    if not args.bazel_outputs:
        return parser.error('At least one --bazel-outputs value is needed!')

    if not args.ninja_outputs:
        return parser.error('At least one --ninja-outputs value is needed!')

    if len(args.bazel_outputs) != len(args.ninja_outputs):
        return parser.error(
            'The --bazel-outputs and --ninja-outputs lists must have the same size!'
        )

    if args.bazel_inputs:
        if not args.inputs_manifest:
            return parser.error(
                '--inputs-manifest is required with --bazel-inputs')

        # Verify that all bazel input labels are pare of the inputs manifest.
        # If not, print a user-friendly message that explains the situation and
        # how to fix it.
        with open(args.inputs_manifest) as f:
            inputs_manifest = json.load(f)

        all_input_labels = set(e['gn_label'] for e in inputs_manifest)
        unknown_labels = set(args.bazel_inputs) - all_input_labels
        if unknown_labels:
            print(
                '''ERROR: The following bazel_inputs labels are not listed in the Bazel inputs manifest:

  %s

These labels must be in one of `gn_labels_for_bazel_inputs` or `extra_gn_labels_for_bazel_inputs`.
For more details, see the comments in //build/bazel/legacy_ninja_build_outputs.gni.
''' % '\n  '.join(list(unknown_labels)),
                file=sys.stderr)
            return 1

    if args.extra_bazel_args and args.extra_bazel_args[0] != '--':
        return parser.error(
            'Extra bazel args should be seperate with script args using --')
    args.extra_bazel_args = args.extra_bazel_args[1:]

    if not os.path.exists(args.workspace_dir):
        return parser.error(
            'Workspace directory does not exist: %s' % args.workspace_dir)

    if not os.path.exists(args.bazel_launcher):
        return parser.error(
            'Bazel launcher does not exist: %s' % args.bazel_launcher)

    cmd = [args.bazel_launcher, args.command]

    configured_args = []

    configured_args += [shlex.quote(arg) for arg in args.extra_bazel_args]

    cmd += configured_args + args.bazel_targets

    ret = subprocess.run(cmd)
    if ret.returncode != 0:
        print(
            'ERROR when calling Bazel. To reproduce, run this in the Ninja output directory:\n\n  %s\n'
            % ' '.join(shlex.quote(c) for c in cmd),
            file=sys.stderr)
        return 1

    src_paths = [
        os.path.join(args.workspace_dir, bazel_out)
        for bazel_out in args.bazel_outputs
    ]
    if not args.allow_directory_in_outputs:
        dirs = [p for p in src_paths if os.path.isdir(p)]
        if dirs:
            print(
                '\nDirectories are not allowed in --bazel-outputs when --allow-directory-in-outputs is not specified, got directories:\n\n%s\n'
                % '\n'.join(dirs))
            return 1

    for src_path, dst_path in zip(src_paths, args.ninja_outputs):
        copy_file_if_changed(src_path, dst_path)

    if args.depfile:
        # Perform a cquery to get all source inputs for the targets, this
        # returns a list of Bazel labesl followed by "(null)" because these
        # are never configured. E.g.:
        #
        #  //build/bazel/examples/hello_world:hello_world (null)
        #
        if args.fuchsia_dir:
            fuchsia_dir = os.path.abspath(args.fuchsia_dir)
        else:
            fuchsia_dir = _FUCHSIA_DIR

        current_dir = os.getcwd()
        source_dir = os.path.relpath(fuchsia_dir, current_dir)

        mapper = BazelLabelMapper(args.workspace_dir, current_dir)

        # All bazel targets as a set() expression for Bazel queries below.
        # See https://bazel.build/query/language#set
        query_targets = 'set(%s)' % ' '.join(args.bazel_targets)

        # This query lists all BUILD.bazel and .bzl files, because the
        # buildfiles() operator cannot be used in the cquery below.
        #
        # --config=quiet is used to remove Bazel verbose output.
        #
        # --noimplicit_deps removes 11,000 files from the result corresponding
        # to the C++ and Python prebuilt toolchains
        #
        # "--output label" ensures the output contains one label per line,
        # which will be followed by '(null)' for source files (or more
        # generally by a build configuration name or hex value for non-source
        # targets which should not be returned by this cquery).
        #
        query_cmd = [
            args.bazel_launcher,
            'query',
            '--config=quiet',
            '--noimplicit_deps',
            '--output',
            'label',
            f'buildfiles(deps({query_targets}))',
        ]

        debug('QUERY_CMD: %s' % ' '.join(shlex.quote(c) for c in query_cmd))
        ret = subprocess.run(query_cmd, capture_output=True, text=True)
        if ret.returncode != 0:
            print(
                'WARNING: Error when calling Bazel query: %s\n%s\n%s\n' % (
                    ' '.join(shlex.quote(c)
                             for c in query_cmd), ret.stderr, ret.stdout),
                file=sys.stderr)
            return 1

        # A function that returns True if the label of a build or source file
        # should be ignored.
        def is_ignored_file_label(label):
            return (
                label.startswith(_BAZEL_BUILTIN_REPOSITORIES) or
                label.startswith(_BAZEL_IGNORE_GENERATED_REPOSITORIES))

        all_inputs = [
            label for label in ret.stdout.splitlines()
            if not is_ignored_file_label(label)
        ]

        # This cquery lists all source files. The output is one label per line
        # which will be followed by '(null)' for source files.
        #
        # (More generally this would be a build configuration name or hex
        # value for non-source targets which should not be returned by this
        # cquery).
        #
        cquery_cmd = [
            args.bazel_launcher,
            'cquery',
            '--config=quiet',
            '--noimplicit_deps',
            '--output',
            'label',
            f'kind("source file", deps({query_targets}))',
        ] + configured_args

        debug('CQUERY_CMD: %s' % ' '.join(shlex.quote(c) for c in cquery_cmd))
        ret = subprocess.run(cquery_cmd, capture_output=True, text=True)
        if ret.returncode != 0:
            print(
                'WARNING: Error when calling Bazel cquery: %s\n%s\n%s\n' % (
                    ' '.join(shlex.quote(c)
                             for c in cquery_cmd), ret.stderr, ret.stdout),
                file=sys.stderr)
            return 1

        for line in ret.stdout.splitlines():
            label, _, config = line.partition(' ')
            assert config == '(null)', f'Invalid cquery output: {line} (config {config})'

            if is_ignored_file_label(label):
                continue

            all_inputs.append(label)

        # Convert output labels to paths relative to the current directory.
        if _DEBUG:
            debug('ALL INPUTS:\n%s' % '\n'.join(all_inputs))

        ignored_labels = []
        all_sources = set()
        for label in all_inputs:
            path = mapper.source_label_to_path(label, relative_to=current_dir)
            if path:
                if is_likely_content_hash_path(path):
                    debug(f'{path} ::: IGNORED CONTENT HASH NAME')
                else:
                    debug(f'{path} <-- {label}')
                    all_sources.add(path)
            else:
                debug(f'IGNORED: {label}')
                ignored_labels.append(label)

        if _ASSERT_ON_IGNORED_FILES and ignored_labels:
            print(
                'ERROR: Found ignored external repository files:',
                file=sys.stderr)
            for label in ignored_labels:
                print(f'  {label}', file=sys.stderr)
            print('', file=sys.stderr)
            return 1

        depfile_content = '%s: %s\n' % (
            ' '.join(shlex.quote(c) for c in args.ninja_outputs), ' '.join(
                shlex.quote(c) for c in sorted(all_sources)))

        if _DEBUG:
            debug('DEPFILE[%s]\n' % depfile_content)

        with open(args.depfile, 'w') as f:
            f.write(depfile_content)

    # Done!
    return 0


if __name__ == '__main__':
    sys.exit(main())
