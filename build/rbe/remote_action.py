#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Construct and execution remote actions with rewrapper.

This script is both a library and standalone binary for
driving rewrapper.

Usage:
  $0 [remote-options...] -- command...
"""

import argparse
import dataclasses
import filecmp
import glob
import os
import re
import subprocess
import shlex
import sys

import fuchsia
import cl_utils
import output_leak_scanner

from pathlib import Path
from typing import AbstractSet, Callable, Iterable, Optional, Sequence, Tuple

_SCRIPT_BASENAME = Path(__file__).name

PROJECT_ROOT = fuchsia.project_root_dir()

# Needs to be computed with os.path.relpath instead of Path.relative_to
# to support testing a fake (test-only) value of PROJECT_ROOT.
PROJECT_ROOT_REL = cl_utils.relpath(PROJECT_ROOT, start=os.curdir)

# This is a known path where remote execution occurs.
# This should only be used for workarounds as a last resort.
_REMOTE_PROJECT_ROOT = Path('/b/f/w')

# Wrapper script to capture remote stdout/stderr, co-located with this script.
_REMOTE_LOG_SCRIPT = Path('build', 'rbe', 'log-it.sh')

_DETAIL_DIFF_SCRIPT = Path('build', 'rbe', 'detail-diff.sh')

_REMOTETOOL_SCRIPT = Path('build', 'rbe', 'remotetool.sh')

_CHECK_DETERMINISM_SCRIPT = Path('build', 'tracer', 'output_cacher.py')

_RECLIENT_ERROR_STATUS = 35
_RBE_SERVER_ERROR_STATUS = 45
_RBE_KILLED_STATUS = 137

_RETRIABLE_REWRAPPER_STATUSES = {
    _RECLIENT_ERROR_STATUS,
    _RBE_SERVER_ERROR_STATUS,
    _RBE_KILLED_STATUS,
}


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


def _path_or_default(path_or_none, default: Path) -> Path:
    """Expressions like 'arg or DEFAULT' worked if arg is a string,
    but Path('') is not false-y in Python.

    Where one could write 'arg or DEFAULT', you should use
    '_path_or_default(arg, DEFAULT)' for clairty.
    """
    return default if path_or_none is None else path_or_none


def _write_lines_to_file(path: Path, lines: Iterable[str]):
    path.parent.mkdir(parents=True, exist_ok=True)
    contents = '\n'.join(lines) + '\n'
    with open(path, 'w') as f:
        f.write(contents)


def _files_match(file1: Path, file2: Path) -> bool:
    """Compares two files, returns True if they both exist and match."""
    # filecmp.cmp does not invoke any subprocesses.
    return filecmp.cmp(file1, file2, shallow=False)


def _detail_diff(
        file1: Path, file2: Path, project_root_rel: Path = None) -> int:
    command = [
        _path_or_default(project_root_rel, PROJECT_ROOT_REL) /
        _DETAIL_DIFF_SCRIPT,
        file1,
        file2,
    ]
    return subprocess.call([str(x) for x in command])


def _text_diff(file1: Path, file2: Path) -> int:
    """Display textual differences to stdout."""
    return subprocess.call(['diff', '-u', str(file1), str(file2)])


def _files_under_dir(path: Path) -> Iterable[Path]:
    """'ls -R DIR' listing files relative to DIR."""
    yield from (
        Path(root, file).relative_to(path)
        for root, unused_dirs, files in os.walk(str(path))
        for file in files)


def _common_files_under_dirs(path1: Path, path2: Path) -> AbstractSet[Path]:
    files1 = set(_files_under_dir(path1))
    files2 = set(_files_under_dir(path2))
    return files1 & files2  # set intersection


def _expand_common_files_between_dirs(
        path_pairs: Iterable[Tuple[Path, Path]]) -> Iterable[Tuple[Path, Path]]:
    """Expands two directories into paths to their common files.

    Args:
      path_pairs: sequence of pairs of paths to compare.

    Yields:
      stream of pairs of files to compare.  Within each directory group,
      common sub-paths will be in sorted order.
    """
    for left, right in path_pairs:
        for f in sorted(_common_files_under_dirs(left, right)):
            yield left / f, right / f


def resolved_shlibs_from_ldd(lines: Iterable[str]) -> Iterable[Path]:
    """Parse 'ldd' output.

    Args:
      lines: stdout text of 'ldd'

    Example line:
      librustc_driver-897e90da9cc472c4.so => /home/my_project/tools/rust/bin/../lib/librustc_driver.so (0x00007f6fdf600000)

    Should yield:
      /home/my_project/tools/rust/bin/../lib/librustc_driver.so
    """
    for line in lines:
        lib, sep, resolved = line.strip().partition('=>')
        if sep == '=>':
            yield Path(resolved.strip().split(' ')[0])


def host_tool_shlibs(executable: Path) -> Iterable[Path]:
    """Identify shared libraries of an executable.

    This only works on platforms with `ldd`.

    Yields:
      paths to non-system shared libraries
    """
    # TODO: do this once in the entire build, as early as GN time
    # TODO: support Mac OS using `otool -L`
    ldd_output = subprocess.run(
        ['ldd', str(executable)], capture_output=True, text=True)
    if ldd_output.returncode != 0:
        raise Exception(
            f"Failed to determine shared libraries of '{executable}'.")

    yield from resolved_shlibs_from_ldd(ldd_output.stdout.splitlines())


def host_tool_nonsystem_shlibs(executable: Path) -> Iterable[Path]:
    """Identify non-system shared libraries of a host tool.

    The host tool's shared libraries will need to be uploaded
    for remote execution.  (The caller should verify that
    the shared library paths fall under the remote action's exec_root.)

    Limitation: this works for only linux-x64 ELF binaries, but this is
    fine because only linux-x64 remote workers are available.

    Yields:
      paths to non-system shared libraries
    """
    for lib in host_tool_shlibs(executable):
        if any(str(lib).startswith(prefix) for prefix in ('/usr/lib', '/lib')):
            continue  # filter out system libs
        yield lib


def relativize_to_exec_root(path: Path, start: Path = None) -> Path:
    return cl_utils.relpath(path, start=(_path_or_default(start, PROJECT_ROOT)))


def _reclient_canonical_working_dir_components(
        subdir_components: Iterable[str]) -> Iterable[str]:
    """Computes the path used by rewrapper --canonicalize_working_dir=true.

    The exact values returned are an implementation detail of reclient
    that is not reliable, so this should only be used as a last resort
    in workarounds.

    https://team.git.corp.google.com/foundry-x/re-client/+/refs/heads/master/internal/pkg/reproxy/action.go#177

    Args:
      subdir_components: a relative path like ('out', 'default', ...)

    Yields:
      Replacement path components like ('set_by_reclient', 'a', ...)
    """
    first = next(subdir_components, None)
    if first is None or first == '':
        return  # no components
    yield 'set_by_reclient'
    for _ in subdir_components:
        yield 'a'


def reclient_canonical_working_dir(build_subdir: Path) -> Path:
    new_components = list(
        _reclient_canonical_working_dir_components(iter(build_subdir.parts)))
    return Path(*new_components) if new_components else Path('')


def remove_working_dir_abspaths(line: str, build_subdir: Path) -> str:
    # Two substutions are necesssary to accommodate both cases
    # of rewrapper --canonicalize_working_dir={true,false}.
    # TODO: if the caller knows whether which case applies, then
    # you only need to apply one of the following substitutions.
    local_working_dir_abs = _REMOTE_PROJECT_ROOT / build_subdir
    canonical_working_dir = _REMOTE_PROJECT_ROOT / reclient_canonical_working_dir(
        build_subdir)
    return line.replace(str(local_working_dir_abs) + os.path.sep, '').replace(
        str(canonical_working_dir) + os.path.sep, '')


def _file_lines_matching(path: Path, substr: str) -> Iterable[str]:
    with open(path) as f:
        for line in f:
            if substr in line:
                yield line


def _transform_file_by_lines(
        src: Path, dest: Path, line_transform: Callable[[str], str]):
    with open(src) as f:
        new_lines = [line_transform(line) for line in f]

    _write_lines_to_file(dest, new_lines)


def _rewrite_file_by_lines_in_place(
        path: Path, line_transform: Callable[[str], str]):
    _transform_file_by_lines(path, path, line_transform)


def remove_working_dir_abspaths_from_depfile_in_place(
        depfile: Path, build_subdir: Path):
    # TODO(http://fxbug.dev/124714): This transformation would be more robust
    # if we properly lexed a depfile and operated on tokens instead of lines.
    _rewrite_file_by_lines_in_place(
        depfile,
        lambda line: remove_working_dir_abspaths(line, build_subdir),
    )


def _matches_file_not_found(line: str) -> bool:
    return "fatal error:" in line and "file not found" in line


def _should_retry_remote_action(status: cl_utils.SubprocessResult) -> bool:
    """Heuristic for deciding when it is worth retrying rewrapper.

    Retry once under these conditions:
      35: reclient error (includes infrastructural issues or local errors)
      45: remote execution (server) error, e.g. remote blob download failure
      137: SIGKILL'd (signal 9) by OS.
        Reasons may include segmentation fault, or out of memory.

    Args:
      status: exit code, stdout, stderr of a completed subprocess.
    """
    if status.returncode == 0:  # success, no need to retry
        return False

    # Do not retry missing file errors, the user should address those.
    if any(_matches_file_not_found(line) for line in status.stderr):
        return False

    return status.returncode in _RETRIABLE_REWRAPPER_STATUSES


def _reproxy_log_dir() -> Optional[str]:
    # Set by build/rbe/fuchsia-reproxy-wrap.sh.
    return os.environ.get("RBE_proxy_log_dir", None)


def _rewrapper_log_dir() -> Optional[str]:
    # Set by build/rbe/fuchsia-reproxy-wrap.sh.
    return os.environ.get("RBE_log_dir", None)


@dataclasses.dataclass
class ReproxyLogEntry(object):
    execution_id: str
    action_digest: str
    # add more useful fields as needed


def _remove_prefix(text: str, prefix: str) -> str:
    # Like string.removeprefix() in Python 3.9+
    return text[len(prefix):] if text.startswith(prefix) else text


def _remove_suffix(text: str, suffix: str) -> str:
    # Like string.removesuffix() in Python 3.9+
    return text[:-len(suffix)] if text.endswith(suffix) else text


def _extract_proto_field_value(line: str, field_name: str) -> str:
    return _remove_prefix(line, field_name + ':').strip(' "')


def _parse_reproxy_log_record_lines(lines: Iterable[str]) -> ReproxyLogEntry:
    """Extremely crude extraction of data from a .rrpl file.

    Args:
      lines: text from a rewrapper --action_log file

    TODO: properly parse reclient proxy.LogRecord textproto, which
      requires protoc -> .pb2.py from .protos from various sources.
      See build/rbe/proto/refresh.sh.
    """
    execution_id = None
    action_digest = None
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("execution_id:"):
            execution_id = _extract_proto_field_value(stripped, "execution_id")
        if stripped.startswith("action_digest:"):
            action_digest = _extract_proto_field_value(
                stripped, "action_digest")
    return ReproxyLogEntry(
        execution_id=execution_id,
        action_digest=action_digest,
    )


def _parse_rewrapper_action_log(log: Path) -> ReproxyLogEntry:
    with open(log) as f:
        return _parse_reproxy_log_record_lines(f.readlines())


def _diagnose_fail_to_dial(line: str):
    # Check connection to reproxy.
    if "Fail to dial" in line:
        print(line)
        msg(
            f'''"Fail to dial" could indicate that reproxy is not running.
`reproxy` is launched automatically by `fx build`.
If you are manually running a remote-wrapped build step,
you may need to wrap your build command with:

  {fuchsia.REPROXY_WRAP} -- command...

'Proxy started successfully.' indicates that reproxy is running.
''')


def _diagnose_rbe_permissions(line: str):
    # Check for permissions issues.
    if "Error connecting to remote execution client: rpc error: code = PermissionDenied" in line:
        print(line)
        msg(
            f'''You might not have permssion to access the RBE instance.
Contact fuchsia-build-team@google.com for support.
''')


_REPROXY_ERROR_MISSING_FILE_RE = re.compile(
    "Status:LocalErrorResultStatus.*Err:stat ([^:]+): no such file or directory"
)


def _diagnose_missing_input_file(line: str):
    # Check for missing files.
    # TODO(b/201697587): surface this diagnostic from rewrapper
    match = _REPROXY_ERROR_MISSING_FILE_RE.match(line)
    if match:
        filename = match.group(1)
        print(line)
        if filename.startswith("out/"):
            description = "generated file"
        else:
            description = "source"
        msg(
            f"Possibly missing a local input file for uploading: {filename} ({description})"
        )


def _diagnose_reproxy_error_line(line: str):
    for check in (
            _diagnose_fail_to_dial,
            _diagnose_rbe_permissions,
            _diagnose_missing_input_file,
    ):
        check(line)


def analyze_rbe_logs(rewrapper_pid: int, action_log: Path = None):
    """Attempt to explain failure by examining various logs.

    Prints additional diagnostics to stdout.

    Args:
      rewrapper_pid: process id of the failed rewrapper invocation.
      action_log: The .rrpl file from `rewrapper --action_log=LOG`,
        which is a proxy.LogRecord textproto.
    """
    # See build/rbe/fuchsia-reproxy-wrap.sh for the setup of these
    # environment variables.
    reproxy_logdir = _reproxy_log_dir()
    if not reproxy_logdir:
        return  # give up
    reproxy_logdir = Path(reproxy_logdir)

    rewrapper_logdir = _rewrapper_log_dir()
    if not rewrapper_logdir:
        return  # give up
    rewrapper_logdir = Path(rewrapper_logdir)

    # The reproxy.ERROR symlink is stable during a build.
    reproxy_errors = reproxy_logdir / "reproxy.ERROR"

    # Logs are named:
    #   rewrapper.{host}.{user}.log.{severity}.{date}-{time}.{pid}
    # The "rewrapper.{severity}" symlinks are useless because
    # the link destination is constantly being updated during a build.
    rewrapper_logs = rewrapper_logdir.glob(f"rewrapper.*.{rewrapper_pid}")
    if rewrapper_logs:
        msg("See rewrapper logs:")
        for log in rewrapper_logs:
            print("  " + str(log))
        print()  # blank line

    if not action_log:
        return
    if not action_log.is_file():
        return

    msg(f"Action log: {action_log}")
    rewrapper_info = _parse_rewrapper_action_log(action_log)
    execution_id = rewrapper_info.execution_id
    action_digest = rewrapper_info.action_digest
    print(f"  execution_id: {execution_id}")
    print(f"  action_digest: {action_digest}")
    print()  # blank line

    if not reproxy_errors.is_file():
        return

    msg(f"Scanning {reproxy_errors} for clues:")
    # Find the lines that mention this execution_id
    for line in _file_lines_matching(reproxy_errors, execution_id):
        _diagnose_reproxy_error_line(line)

    # TODO: further analyze remote failures in --action_log (.rrpl)


_RBE_DOWNLOAD_STUB_IDENTIFIER = '# RBE download stub'


def _write_download_stub(
    path: Path,
    rrpl: Path,
    build_id: str,
):
    lines = [
        _RBE_DOWNLOAD_STUB_IDENTIFIER,
        str(rrpl),
        build_id,
    ]
    _write_lines_to_file(path, lines)


def make_download_stubs(
    files: Iterable[Path],
    dirs: Iterable[Path],
    rrpl: Path,
    build_id: str,
):
    """Establish placeholders from which real artifacts can be retrieved later.

    Args:
      files: files to create download stubs for.
      dirs: directories to create download stubs for.
      rrpl: single-action reproxy log (reproxy.LogRecord), which contains
          file digests.
      build_id: any string that corresponds to a unique build.
    """
    # TODO(http://fxbug.dev/123178): validate that we are creating stubs for
    # paths that are covered in the .rrpl log.  This requires parsing
    # more of the .rrpl log.
    for f in files:
        _write_download_stub(path=f, rrpl=rrpl, build_id=build_id)
    for d in dirs:
        _write_download_stub(path=d / '.dlstub', rrpl=rrpl, build_id=build_id)


# TODO(http://fxbug.dev/123178): download using stub links


class RemoteAction(object):
    """RemoteAction represents a command that is to be executed remotely."""

    def __init__(
        self,
        rewrapper: Path,
        command: Sequence[str],
        options: Sequence[str] = None,
        exec_root: Optional[Path] = None,
        working_dir: Optional[Path] = None,
        cfg: Optional[Path] = None,
        exec_strategy: Optional[str] = None,
        inputs: Sequence[Path] = None,
        output_files: Sequence[Path] = None,
        output_dirs: Sequence[Path] = None,
        disable: bool = False,
        save_temps: bool = False,
        remote_log: str = "",
        fsatrace_path: Optional[Path] = None,
        diagnose_nonzero: bool = False,
        check_determinism: bool = False,
    ):
        """RemoteAction constructor.

        Args:
          rewrapper: path to rewrapper binary
          options: rewrapper options (not already covered by other parameters)
          command: the command to execute remotely
          cfg: rewrapper config file (optional)
          exec_strategy: rewrapper --exec_strategy (optional)
          exec_root: an absolute path location that is parent to all of this
            remote action's inputs and outputs.
          working_dir: directory from which command is to be executed.
            This must be a sub-directory of 'exec_root'.
          inputs: inputs needed for remote execution, relative to the current working dir.
          output_files: files to be fetched after remote execution, relative to the
            current working dir.
          output_dirs: directories to be fetched after remote execution, relative to the
            current working dir.
          disable: if true, execute locally.
          check_determinism: if true, compare outputs of two local executions.
          save_temps: if true, keep around temporarily generated files after execution.
          remote_log: "" means disabled.  Any other value, remote logging is
            enabled, and stdout/stderr of the remote execution is captured
            to a file and downloaded.
            if "<AUTO>":
              if there is at least one remote output file:
                name the log "${output_files[0]}.remote-log"
              else:
                name the log "rbe-action-output.remote-log"
            else:
              use the given name appended with ".remote-log"
          fsatrace_path: Given a path to an fsatrace tool
              (located under exec_root), this will wrap the remote command
              to trace and log remote file access.
              if there is at least one remote output file:
                the trace name is "${output_files[0]}.remote-fsatrace"
              else:
                the trace name "rbe-action-output.remote-fsatrace"
          diagnose_nonzero: if True, attempt to examine logs and determine
            a cause of error.
        """
        self._rewrapper = rewrapper
        self._config = cfg  # can be None
        self._exec_strategy = exec_strategy  # can be None
        self._save_temps = save_temps
        self._diagnose_nonzero = diagnose_nonzero
        self._working_dir = (working_dir or Path(os.curdir)).absolute()
        self._exec_root = (exec_root or PROJECT_ROOT).absolute()
        # Parse and strip out --remote-* flags from command.
        self._remote_command = command
        self._remote_disable = disable
        self._check_determinism = check_determinism
        self._options = (options or [])

        # Detect some known rewrapper options
        self._rewrapper_known_options, _ = _REWRAPPER_ARG_PARSER.parse_known_args(
            self._options)

        # Inputs and outputs parameters are relative to current working dir,
        # but they will be relativized to exec_root for rewrapper.
        # It is more natural to copy input/output paths that are relative to the
        # current working directory.
        self._inputs = inputs or []
        self._output_files = output_files or []
        self._output_dirs = output_dirs or []

        # Amend input/outputs when logging remotely.
        self._remote_log_name = self._name_remote_log(remote_log)
        if self._remote_log_name:
            # These paths are relative to the working dir.
            self._output_files.append(self._remote_log_name)
            self._inputs.append(self._remote_log_script_path)

        self._fsatrace_path = fsatrace_path  # relative to working dir
        if self._fsatrace_path:
            self._inputs.extend([self._fsatrace_path, self._fsatrace_so])
            self._output_files.append(self._fsatrace_remote_log)

        self._cleanup_files = []

    @property
    def exec_root(self) -> Optional[str]:
        return self._exec_root

    @property
    def exec_strategy(self) -> Optional[str]:
        return self._exec_strategy

    @property
    def config(self) -> str:
        return self._config

    @property
    def _default_auxiliary_file_basename(self) -> str:
        # Return a str instead of Path because most callers will want to
        # append a suffix (str + str).
        if self._output_files:
            return str(self._output_files[0])
        else:  # pick something arbitrary, but deterministic
            return 'rbe-action-output'

    def _name_remote_log(self, remote_log: str) -> Path:
        if remote_log == '<AUTO>':
            return Path(self._default_auxiliary_file_basename + '.remote-log')

        if remote_log:
            return Path(remote_log + '.remote-log')

        return None

    @property
    def _remote_log_script_path(self) -> Path:
        return self.exec_root_rel / _REMOTE_LOG_SCRIPT

    @property
    def _fsatrace_local_log(self) -> Path:
        return Path(self._default_auxiliary_file_basename + '.local-fsatrace')

    @property
    def _fsatrace_remote_log(self) -> Path:
        return Path(self._default_auxiliary_file_basename + '.remote-fsatrace')

    @property
    def _fsatrace_so(self) -> Path:
        # fsatrace needs the corresponding .so to work
        return self._fsatrace_path.with_suffix('.so')

    @property
    def _action_log(self) -> Path:
        # The --action_log is a single entry of the cumulative log
        # of remote actions in the reproxy_*.rrpl file.
        # The information contained in this log is the same,
        # but is much easier to find than in the cumulative log.
        return Path(self._default_auxiliary_file_basename + '.rrpl')

    @property
    def local_command(self) -> Sequence[str]:
        """This is the original command that would have been run locally.
        All of the --remote-* flags have been removed at this point.
        """
        return cl_utils.auto_env_prefix_command(self._remote_command)

    def _generate_options(self) -> Iterable[str]:
        if self.config:
            yield '--cfg'
            yield str(self.config)

        if self.exec_strategy:
            yield f'--exec_strategy={self.exec_strategy}'

        yield from self._options

    @property
    def options(self) -> Sequence[str]:
        return list(self._generate_options())

    @property
    def canonicalize_working_dir(self) -> Optional[bool]:
        return self._rewrapper_known_options.canonicalize_working_dir

    @property
    def download_outputs(self) -> bool:
        return self._rewrapper_known_options.download_outputs

    @property
    def save_temps(self) -> bool:
        return self._save_temps

    @property
    def working_dir(self) -> Path:
        return self._working_dir

    @property
    def remote_disable(self) -> bool:
        return self._remote_disable

    @property
    def check_determinism(self) -> bool:
        return self._check_determinism

    @property
    def diagnose_nonzero(self) -> bool:
        return self._diagnose_nonzero

    def _relativize_path_to_exec_root(self, path: Path) -> Path:
        return relativize_to_exec_root(
            self.working_dir / path, start=self.exec_root)

    def _relativize_paths_to_exec_root(self,
                                       paths: Sequence[Path]) -> Sequence[Path]:
        return [self._relativize_path_to_exec_root(path) for path in paths]

    @property
    def exec_root_rel(self) -> Path:
        return cl_utils.relpath(self.exec_root, start=self.working_dir)

    @property
    def build_subdir(self) -> Path:
        """This is the relative path from the exec_root to the current working dir.

        Note that this intentionally uses Path.relative_to(), which requires
        that the working dir be a subpath of exec_root.

        Raises:
          ValueError if self.exec_root is not a parent of self.working_dir.
        """
        return self.working_dir.relative_to(self.exec_root)

    @property
    def inputs_relative_to_working_dir(self) -> Sequence[Path]:
        return self._inputs

    @property
    def output_files_relative_to_working_dir(self) -> Sequence[Path]:
        return self._output_files

    @property
    def output_dirs_relative_to_working_dir(self) -> Sequence[Path]:
        return self._output_dirs

    @property
    def inputs_relative_to_project_root(self) -> Sequence[Path]:
        return self._relativize_paths_to_exec_root(
            self.inputs_relative_to_working_dir)

    @property
    def output_files_relative_to_project_root(self) -> Sequence[Path]:
        return self._relativize_paths_to_exec_root(
            self.output_files_relative_to_working_dir)

    @property
    def output_dirs_relative_to_project_root(self) -> Sequence[Path]:
        return self._relativize_paths_to_exec_root(
            self.output_dirs_relative_to_working_dir)

    def _inputs_list_file(self) -> Path:
        inputs_list_file = Path(
            self._default_auxiliary_file_basename + '.inputs')
        _write_lines_to_file(
            inputs_list_file,
            (str(x) for x in self.inputs_relative_to_project_root),
        )
        return inputs_list_file

    def _generate_rewrapper_command_prefix(self) -> Iterable[str]:
        yield str(self._rewrapper)
        yield f"--exec_root={self.exec_root}"

        # The .rrpl contains detailed information for improved
        # diagnostics and troubleshooting.
        # When NOT downloading outputs, we need the .rrpl file for the
        # output digests to be able to retrieve them from the CAS later.
        if self.diagnose_nonzero or not self.download_outputs:
            yield f"--action_log={self._action_log}"

        yield from self.options

        if self._inputs:
            # TODO(http://fxbug.dev/124186): use --input_list_paths only if list is sufficiently long
            inputs_list_file = self._inputs_list_file()
            self._cleanup_files.append(inputs_list_file)
            yield f"--input_list_paths={inputs_list_file}"

        # outputs (files and dirs) need to be relative to the exec_root,
        # even as we run from inside the build_dir under exec_root.
        if self._output_files:
            output_files = ','.join(
                str(x) for x in self.output_files_relative_to_project_root)
            yield f"--output_files={output_files}"

        if self._output_dirs:
            output_dirs = ','.join(
                str(x) for x in self.output_dirs_relative_to_project_root)
            yield f"--output_directories={output_dirs}"

    @property
    def _remote_log_command_prefix(self) -> Sequence[str]:
        return [
            str(self._remote_log_script_path),
            '--log',
            str(self._remote_log_name),
            '--',
        ]

    def _fsatrace_command_prefix(self, log: Path) -> Sequence[str]:
        return cl_utils.auto_env_prefix_command(
            [
                'FSAT_BUF_SIZE=5000000',
                str(self._fsatrace_path),
                'erwdtmq',
                str(log),
                '--',
            ])

    def _generate_remote_launch_command(self) -> Iterable[str]:
        # No need to prepend with fuchsia.REPROXY_WRAP here,
        # because auto_relaunch_with_reproxy() does that.
        yield from self._generate_rewrapper_command_prefix()
        yield '--'

        if self._remote_log_name:
            yield from self._remote_log_command_prefix

        # When requesting both remote logging and fsatrace,
        # use fsatrace as the inner wrapper because the user is not
        # likely to be interested in fsatrace entries attributed
        # to the logging wrapper.
        if self._fsatrace_path:
            yield from self._fsatrace_command_prefix(self._fsatrace_remote_log)

        yield from self.local_command

    def _generate_check_determinism_prefix(self) -> Iterable[str]:
        yield from [
            sys.executable,  # same Python interpreter
            '-S',
            str(self.exec_root_rel / _CHECK_DETERMINISM_SCRIPT),
            '--check-repeatability',
            '--outputs',
        ]

        # Don't bother mentioning the fsatrace file as an output,
        # for checking determinism, as it may be sensitive to process id,
        # and other temporary file accesses.
        for f in self.output_files_relative_to_working_dir:
            yield str(f)

        # TODO: The comparison script does not support directories yet.
        # When it does, yield from self.output_dirs_relative_to_working_dir.

        yield '--'

    def _generate_local_launch_command(self) -> Iterable[str]:
        # When requesting fsatrace, log to a different file than the
        # remote log, so they can be compared.
        if self.check_determinism:
            yield from self._generate_check_determinism_prefix()

        if self._fsatrace_path:
            yield from self._fsatrace_command_prefix(self._fsatrace_local_log)

        yield from self.local_command

    def _generate_launch_command(self) -> Iterable[str]:
        """Generates the rewrapper command, one token at a time."""
        if not self._remote_disable:
            yield from self._generate_remote_launch_command()
        else:
            yield from self._generate_local_launch_command()

    @property
    def launch_command(self) -> Sequence[str]:
        """This is the fully constructed command to be executed on the host.

        In remote enabled mode, this is a rewrapper command wrapped around
        the original command.
        In remote disabled mode, this is just the original command.
        """
        return list(self._generate_launch_command())

    def _make_download_stubs(self):
        make_download_stubs(
            files=self.output_files_relative_to_working_dir,
            dirs=self.output_dirs_relative_to_working_dir,
            rrpl=self._action_log,
            build_id=_reproxy_log_dir(),  # unique per build
        )

    def _cleanup(self):
        for f in self._cleanup_files:
            if f.exists():
                f.unlink()  # does os.remove for files, rmdir for dirs

    def _run_maybe_remotely(self) -> cl_utils.SubprocessResult:
        return cl_utils.subprocess_call(
            self.launch_command, cwd=self.working_dir)

    def run(self) -> int:
        """Remotely execute the command.

        Returns:
          rewrapper's exit code, which is the remote execution exit code in most cases,
            but sometimes an re-client internal error code like 35 or 45.
        """
        # When using a remote canonical_working_dir, make sure the command
        # being launched does not reference the non-canonical local working
        # dir explicitly.
        if self.canonicalize_working_dir:
            leak_status = output_leak_scanner.preflight_checks(
                paths=self.output_files_relative_to_working_dir,
                command=self.local_command,
                pattern=output_leak_scanner.PathPattern(self.build_subdir),
            )
            if leak_status != 0:
                msg(
                    f"Error: Detected local output dir leaks '{self.build_subdir}' in the command.  Aborting remote execution."
                )
                return leak_status

        try:
            result = self._run_maybe_remotely()

            # Under certain error conditions, do a one-time retry
            # for flake/fault-tolerance.
            if _should_retry_remote_action(result):
                result = self._run_maybe_remotely()

            if result.returncode == 0:  # success, nothing to see
                # TODO: output_leak_scanner.postflight_checks() here,
                #   but only when requested, because inspecting output
                #   contents takes time.
                if not self.download_outputs:
                    self._make_download_stubs()
                return result.returncode
            if not self.diagnose_nonzero:
                return result.returncode

            # Not an RBE issue if local execution failed.
            exec_strategy = self.exec_strategy or "remote"  # rewrapper default
            if exec_strategy in {"local", "remote_local_fallback"}:
                return result.returncode

            # Otherwise, continue to analyze log files
            analyze_rbe_logs(
                rewrapper_pid=result.pid,
                action_log=self._action_log,
            )
            return result.returncode

        finally:
            if not self._save_temps:
                self._cleanup()

    def run_with_main_args(self, main_args: argparse.Namespace) -> int:
        """Run depending on verbosity and dry-run mode.

        This serves as a template for main() programs whose
        primary execution action is RemoteAction.run().

        Args:
          main_args: struct with (.verbose, .dry_run, .label, ...)

        Returns:
          exit code
        """
        command_str = cl_utils.command_quoted_str(self.launch_command)
        if main_args.verbose and not main_args.dry_run:
            msg(command_str)
        if main_args.dry_run:
            label_str = " "
            if main_args.label:
                label_str += f"[{main_args.label}] "
            msg(f"[dry-run only]{label_str}{command_str}")
            return 0

        main_exit_code = self.run()

        if main_args.compare and not main_args.local:
            # Also run locally, and compare outputs.
            # If running only --local, there is nothing to compare.
            return self._compare_against_local()

        return main_exit_code

    def _rewrite_local_outputs_for_comparison_workaround(self):
        # TODO: tag each output with information about its type,
        # rather than inferring it based on file extension.
        for f in self.output_files_relative_to_working_dir:
            if f.suffix == '.map':  # intended for linker map files
                # Workaround https://fxbug.dev/89245: relative-ize absolute path of
                # current working directory in linker map files.
                _rewrite_file_by_lines_in_place(
                    f,
                    lambda line: line.replace(
                        self.exec_root, self.exec_root_rel),
                )
            if f.suffix == '.d':  # intended for depfiles
                # TEMPORARY WORKAROUND until upstream fix lands:
                #   https://github.com/pest-parser/pest/pull/522
                # Remove redundant occurrences of the current working dir absolute path.
                # Paths should be relative to the root_build_dir.
                _rewrite_file_by_lines_in_place(
                    f,
                    lambda line: line.replace(
                        str(self.working_dir) + os.path.sep, ''),
                )

    def _compare_fsatraces(self) -> int:
        msg("Comparing local (-) vs. remote (+) file access traces.")
        # Normalize absolute paths.
        local_norm = Path(str(self._fsatrace_local_log) + '.norm')
        remote_norm = Path(str(self._fsatrace_remote_log) + '.norm')
        _transform_file_by_lines(
            self._fsatrace_local_log,
            local_norm,
            lambda line: line.replace(str(self.exec_root) + os.path.sep, ''),
        )
        _transform_file_by_lines(
            self._fsatrace_remote_log,
            remote_norm,
            lambda line: line.replace(
                str(_REMOTE_PROJECT_ROOT) + os.path.sep, ''),
        )
        return _text_diff(local_norm, remote_norm)

    def _run_locally(self) -> int:
        # Run the job locally.
        # Local command may include an fsatrace prefix.
        local_command = list(self._generate_local_launch_command())
        local_command_str = cl_utils.command_quoted_str(local_command)
        exit_code = subprocess.call(local_command)
        if exit_code != 0:
            # Presumably, we want to only compare against local successes.
            msg(
                f"Local command failed for comparison (exit={exit_code}): {local_command_str}"
            )
        return exit_code

    def _compare_against_local(self) -> int:
        # Backup outputs from remote execution first to '.remote'.

        # The fsatrace files will be handled separately because they are
        # already named differently between their local/remote counterparts.
        direct_compare_output_files = [
            (f, Path(str(f) + '.remote'))
            for f in self.output_files_relative_to_working_dir
            if f.is_file() and f.suffix != '.remote-fsatrace'
        ]

        # We have the option to keep the remote or local outputs in-place.
        # Use the results from the local execution, as they are more likely
        # to be what the user expected if something went wrong remotely.
        for f, bkp in direct_compare_output_files:
            os.rename(f, bkp)

        compare_output_dirs = [
            (d, Path(str(d) + '.remote'))
            for d in self.output_dirs_relative_to_working_dir
            if d.is_dir()
        ]
        for d, bkp in compare_output_dirs:
            os.rename(d, bkp)

        # Run the job locally, for comparison.
        local_exit_code = self._run_locally()
        if local_exit_code != 0:
            return local_exit_code

        # Apply workarounds to make comparisons more meaningful.
        self._rewrite_local_outputs_for_comparison_workaround()

        # Translate output directories into list of files.
        all_compare_files = direct_compare_output_files + list(
            _expand_common_files_between_dirs(compare_output_dirs))

        output_diffs = []
        # Quick file comparison first.
        for f, remote_out in all_compare_files:
            if _files_match(f, remote_out):
                # reclaim space when remote output matches, keep only diffs
                os.remove(remote_out)
            else:
                output_diffs.append((f, remote_out))

        # Report detailed differences.
        if output_diffs:
            msg(
                "*** Differences between local (-) and remote (+) build outputs found. ***"
            )
            for local_out, remote_out in output_diffs:
                msg(f"  {local_out} vs. {remote_out}:")
                _detail_diff(
                    local_out,
                    remote_out,
                    project_root_rel=self.exec_root_rel,
                )  # ignore exit status
                msg("------------------------------------")

            # Also compare file access traces, if available.
            if self._fsatrace_path:
                self._compare_fsatraces()

            return 1

        # No output content differences: success.
        return 0


def _rewrapper_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        "Understand some rewrapper flags, so they may be used as attributes.",
        argument_default=None,
        add_help=False,  # do not intercept --help
    )
    parser.add_argument(
        "--exec_root",
        type=str,
        metavar="ABSPATH",
        help="Root directory from which all inputs/outputs are contained.",
    )
    parser.add_argument(
        "--canonicalize_working_dir",
        type=cl_utils.bool_golang_flag,
        help=
        "If true, remotely execute the command in a working dir location, that has the same depth as the actual build output dir relative to the exec_root.  This makes remote actions cacheable across different output dirs.",
    )
    parser.add_argument(
        "--download_outputs",
        type=cl_utils.bool_golang_flag,
        default=True,
        help=
        "Set to false to avoid downloading outputs after remote execution succeeds."
        "",
    )
    return parser


_REWRAPPER_ARG_PARSER = _rewrapper_arg_parser()


def inherit_main_arg_parser_flags(
    parser: argparse.ArgumentParser,
    default_cfg: Path = None,
    default_bindir: Path = None,
):
    """Extend an existing argparser with standard flags.

    These flags are available for tool-specific remote command wrappers to use.
    """
    default_cfg = default_cfg or Path(
        PROJECT_ROOT_REL, 'build', 'rbe', 'fuchsia-rewrapper.cfg')
    default_bindir = default_bindir or Path(
        PROJECT_ROOT_REL, fuchsia.RECLIENT_BINDIR)

    rewrapper_group = parser.add_argument_group(
        title="rewrapper",
        description="rewrapper options that are intercepted and processed",
    )
    rewrapper_group.add_argument(
        "--cfg",
        type=Path,
        default=default_cfg,
        help="rewrapper config file.",
    )
    rewrapper_group.add_argument(
        "--exec_strategy",
        type=str,
        choices=['local', 'remote', 'remote_local_fallback', 'racing'],
        help="rewrapper execution strategy.",
    )
    rewrapper_group.add_argument(
        "--inputs",
        action='append',
        # leave the type as [str], so commas can be processed downstream
        default=[],
        metavar="PATHS",
        help=
        "Specify additional remote inputs, comma-separated, relative to the current working dir (repeatable, cumulative).  Note: This is different than `rewrapper --inputs`, which expects exec_root-relative paths.",
    )
    rewrapper_group.add_argument(
        "--output_files",
        action='append',
        # leave the type as [str], so commas can be processed downstream
        default=[],
        metavar="FILES",
        help=
        "Specify additional remote output files, comma-separated, relative to the current working dir (repeatable, cumulative).  Note: This is different than `rewrapper --output_files`, which expects exec_root-relative paths.",
    )
    rewrapper_group.add_argument(
        "--output_directories",
        action='append',
        # leave the type as [str], so commas can be processed downstream
        default=[],
        metavar="DIRS",
        help=
        "Specify additional remote output directories, comma-separated, relative to the current working dir (repeatable, cumulative).  Note: This is different than `rewrapper --output_directories`, which expects exec_root-relative paths.",
    )

    main_group = parser.add_argument_group(
        title="common",
        description="Generic remote action options",
    )
    main_group.add_argument(
        "--bindir",
        type=Path,
        default=default_bindir,
        metavar="PATH",
        help="Path to reclient tools like rewrapper, reproxy.",
    )
    main_group.add_argument(
        "--local",
        action="store_true",
        default=False,
        help="Disable remote execution, run the original command locally.",
    )
    main_group.add_argument(
        "--dry-run",
        action="store_true",
        default=False,
        help="Show final rewrapper command and exit.",
    )
    main_group.add_argument(
        "--verbose",
        action="store_true",
        default=False,
        help="Print additional debug information while running.",
    )
    main_group.add_argument(
        "--label",
        type=str,
        default="",
        help="Build system identifier, for diagnostic messages",
    )
    main_group.add_argument(
        "--check-determinism",
        action="store_true",
        default=False,
        help="Run locally twice and compare outputs [requires: --local].",
    )
    main_group.add_argument(
        "--log",
        type=str,
        dest="remote_log",
        const="<AUTO>",  # pick name based on ${output_files[0]}
        default="",  # blank means to not log
        metavar="BASE",
        nargs='?',
        help="""Capture remote execution's stdout/stderr to a log file.
If a name argument BASE is given, the output will be 'BASE.remote-log'.
Otherwise, BASE will default to the first output file named.""",
    )
    main_group.add_argument(
        "--save-temps",
        action="store_true",
        default=False,
        help="Keep around intermediate files that are normally cleaned up.",
    )
    main_group.add_argument(
        "--auto-reproxy",
        action="store_true",
        default=False,
        help="OBSOLETE: reproxy is already automatically launched if needed.",
    )
    main_group.add_argument(
        "--fsatrace-path",
        type=Path,
        default=None,  # None means do not trace
        metavar="PATH",
        help="""Given a path to an fsatrace tool (located under exec_root), this will trace a remote execution's file accesses.  This is useful for diagnosing unexpected differences between local and remote builds.  The trace file will be named '{output_files[0]}.remote-fsatrace' (if there is at least one output), otherwise 'remote-action-output.remote-fsatrace'.""",
    )
    main_group.add_argument(
        "--compare",
        action="store_true",
        default=False,
        help=
        "In 'compare' mode, run both locally and remotely (sequentially) and compare outputs.  Exit non-zero (failure) if any of the outputs differs between the local and remote execution, even if those executions succeeded.  When used with --fsatrace-path, also compare file access traces.  No comparison is done with --local mode.",
    )
    main_group.add_argument(
        "--diagnose-nonzero",
        action="store_true",
        default=False,
        help=
        """On nonzero exit statuses, attempt to diagnose potential RBE issues.  This scans various reproxy logs for information, and can be noisy."""
    )
    # Positional args are the command and arguments to run.
    parser.add_argument(
        "command", nargs="*", help="The command to run remotely")


def _main_arg_parser() -> argparse.ArgumentParser:
    """Construct the argument parser, called by main()."""
    parser = argparse.ArgumentParser(
        description="Executes a build action command remotely.",
        argument_default=[],
    )
    inherit_main_arg_parser_flags(parser)
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def remote_action_from_args(
        main_args: argparse.Namespace,
        remote_options: Sequence[str] = None,
        command: Sequence[str] = None,
        # These inputs and outputs can come from application-specific logic.
        inputs: Sequence[Path] = None,
        output_files: Sequence[Path] = None,
        output_dirs: Sequence[Path] = None,
        **kwargs,  # other RemoteAction __init__ params
) -> RemoteAction:
    """Construct a remote action based on argparse parameters."""
    inputs = (inputs or []) + [
        Path(p) for p in cl_utils.flatten_comma_list(main_args.inputs)
    ]
    output_files = (output_files or []) + [
        Path(p) for p in cl_utils.flatten_comma_list(main_args.output_files)
    ]
    output_dirs = (output_dirs or []) + [
        Path(p)
        for p in cl_utils.flatten_comma_list(main_args.output_directories)
    ]
    return RemoteAction(
        rewrapper=main_args.bindir / "rewrapper",
        options=(remote_options or []),
        command=command or main_args.command,
        cfg=main_args.cfg,
        exec_strategy=main_args.exec_strategy,
        inputs=inputs,
        output_files=output_files,
        output_dirs=output_dirs,
        disable=main_args.local,
        check_determinism=main_args.check_determinism,
        save_temps=main_args.save_temps,
        remote_log=main_args.remote_log,
        fsatrace_path=main_args.fsatrace_path,
        diagnose_nonzero=main_args.diagnose_nonzero,
        **kwargs,
    )


_FORWARDED_REMOTE_FLAGS = cl_utils.FlagForwarder(
    # Mapped options can be wrapper script options (from
    # inherit_main_arg_parser_flags) or rewrapper options.
    [
        cl_utils.ForwardedFlag(
            name="--remote-disable",
            has_optarg=False,
            mapped_name="--local",
        ),
        cl_utils.ForwardedFlag(
            name="--remote-inputs",
            has_optarg=True,
            mapped_name="--inputs",
        ),
        cl_utils.ForwardedFlag(
            name="--remote-outputs",
            has_optarg=True,
            mapped_name="--output_files",
        ),
        cl_utils.ForwardedFlag(
            name="--remote-output-dirs",
            has_optarg=True,
            mapped_name="--output_directories",
        ),
        cl_utils.ForwardedFlag(
            name="--remote-flag",
            has_optarg=True,
            mapped_name="",
        ),
    ])


def forward_remote_flags(
        argv: Sequence[str]) -> Tuple[Sequence[str], Sequence[str]]:
    """Propagate --remote-* flags from the wrapped command to main args.

    This allows late-appended flags to influence wrapper scripts' behavior.
    This works around limitations and difficulties of specifying
    tool options in some build systems.
    This should be done *before* passing argv to _MAIN_ARG_PARSER.
    Unlike using argparse.ArgumentParser, this forwarding approache
    preserves the left-to-right order in which flags appear.

    Args:
      argv: the full command sequence seen my main, like sys.argv[1:]
          Script args appear before the first '--', and the wrapped command
          is considered everything thereafter.

    Returns:
      1) main script args, including those propagated from the wrapped command.
      2) wrapped command, but with --remote-* flags filtered out.
    """
    # Split the whole command around the first '--'.
    script_args, sep, unfiltered_command = cl_utils.partition_sequence(
        argv, '--')
    if sep == None:
        return (['--help'], [])  # Tell the caller to trigger help and exit

    forwarded_flags, filtered_command = _FORWARDED_REMOTE_FLAGS.sift(
        unfiltered_command)
    return script_args + forwarded_flags, filtered_command


def auto_relaunch_with_reproxy(
        script: Path, argv: Sequence[str], args: argparse.Namespace):
    """If reproxy is not already running, re-launch with reproxy running.

    Args:
      script: the invoking script
      argv: the original complete invocation
      args: argparse Namespace for argv.  Only needs to be partially
          processed as far as inherit_main_arg_parser_flags().

    Returns:
      nothing when reproxy is already running.
      If a re-launch is necessary, this does not return, as the process
      is replaced by a os.exec*() call.
    """
    if args.auto_reproxy:
        msg(
            'You no longer need to pass --auto-reproxy, reproxy is launched automatically when needed.'
        )

    if args.dry_run or args.local:
        # Don't need reproxy when no call to rewrapper is expected.
        return

    proxy_log_dir = _reproxy_log_dir()
    rewrapper_log_dir = _rewrapper_log_dir()
    if args.verbose:
        msg(f'Detected RBE_proxy_log_dir={proxy_log_dir}')
        msg(f'Detected RBE_log_dir={rewrapper_log_dir}')

    if proxy_log_dir and rewrapper_log_dir:
        # Ok for caller to proceed and eventually invoke rewrapper
        return

    python = cl_utils.relpath(Path(sys.executable), start=Path(os.curdir))
    relaunch_args = ['--', str(python), '-S', str(script)] + argv
    if args.verbose:
        cmd_str = cl_utils.command_quoted_str(
            [str(fuchsia.REPROXY_WRAP)] + relaunch_args)
        msg(f'Automatically re-launching: {cmd_str}')
    cl_utils.exec_relaunch([fuchsia.REPROXY_WRAP] + relaunch_args)
    assert False, "exec_relaunch() should never return"


def main(argv: Sequence[str]) -> int:
    # Move --remote-* flags from the wrapped command to equivalent main args.
    main_argv, filtered_command = forward_remote_flags(argv)

    # forward all unknown flags to rewrapper
    # forwarded rewrapper options with values must be written as '--flag=value',
    # not '--flag value' because argparse doesn't know what unhandled flags
    # expect values.
    main_args, other_remote_options = _MAIN_ARG_PARSER.parse_known_args(
        main_argv)

    # Re-launch self with reproxy if needed.
    auto_relaunch_with_reproxy(
        script=Path(__file__),
        argv=argv,
        args=main_args,
    )
    # At this point, reproxy is guaranteed to be running.

    remote_action = remote_action_from_args(
        main_args=main_args,
        remote_options=other_remote_options,
        command=filtered_command,
    )
    return remote_action.run_with_main_args(main_args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
