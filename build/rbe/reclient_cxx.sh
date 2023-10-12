#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Bare-minimal execution of rewrapper, with no additional logic or features,
# purely for the sake of performance with minimal overhead.
# Intended for clang and gcc.
# Goal is to eliminate all the workarounds for re-client,
# and eventually eliminate the need for this layer of wrapping.

# Avoid invoking any subprocesses, use shell built-in equivalents where possible.

# Portability notes:
# readarray is only available in bash 4+
# but 'read -r -a' works with bash 3.2.

set -eu

readonly script="$0"
# assume script is always with path prefix, e.g. "./$script"
readonly script_dir="${script%/*}"  # dirname
readonly script_basename="${script##*/}"  # basename

# not normalized
readonly exec_root="$script_dir/../.."

# string join
# $1 is delimiter character
# the rest are strings to be joined
function join_by_char() {
  local IFS="$1"
  shift
  printf "%s" "$*"
}

# Relative path to exec_root without calling realpath/readlink.
# Assume working dir is at or below exec_root.
_exec_root_rel_parts=()
IFS=/ read -r -a _components <<< "$script_dir"
for p in "${_components[@]}"
do
  case "$p" in
    ..) _exec_root_rel_parts+=( "$p" ) ;;
    *) break ;;
  esac
done

readonly exec_root_rel="$(join_by_char '/' "${_exec_root_rel_parts[@]}")"

# hard-coded assumptions about remote environment
readonly remote_exec_root="/b/f/w"

# remote working dir (internal implementation detail of re-client):
# * based on depth of working-dir relative to exec_root, e.g. ("out/foo")
# * when using --canonicalize_working_dir=true
_rwd_parts=()
for p in "${_exec_root_rel_parts[@]}"
do
  if [[ "${#_rwd_parts[@]}" == 0 ]]
  then _rwd_parts+=( 'set_by_reclient' )
  else _rwd_parts+=( 'a' )
  fi
done
readonly remote_canonical_working_subdir="$(join_by_char '/' "${_rwd_parts[@]}")"
readonly remote_working_dir="$remote_exec_root"/"$remote_canonical_working_subdir"

# OSTYPE and HOSTTYPE are always set by bash
case "$OSTYPE" in
  linux*) _HOST_OS=linux ;;
  darwin*) _HOST_OS=mac ;;
  *) echo >&2 "Unknown OS: $OSTYPE" ; exit 1 ;;
esac
case "$HOSTTYPE" in
  x86_64) _HOST_ARCH=x64 ;;
  arm64) _HOST_ARCH=arm64 ;;
  *) echo >&2 "Unknown ARCH: $HOSTTYPE" ; exit 1 ;;
esac

readonly HOST_PLATFORM="$_HOST_OS-$_HOST_ARCH"

readonly reclient_bindir="$exec_root/prebuilt/proprietary/third_party/reclient/$HOST_PLATFORM"
readonly rewrapper="$reclient_bindir"/rewrapper
readonly cfg="$script_dir"/fuchsia-rewrapper.cfg
readonly reproxy_wrap="$script_dir"/fuchsia-reproxy-wrap.sh

function msg() {
  echo "[$script_basename]" "$@"
}

function dmsg() {
  # Uncomment for verbose debug messages:
  # echo "[$script_basename]" "$@"
  :
}

# Normalize path: return an absolute path without any .. in the middle.
# Following-symlinks is optional.
if which realpath 2>&1 > /dev/null; then
  function normalize_path() {
    realpath "$1"
  }
elif which readlink 2>&1 > /dev/null; then
  function normalize_path() {
    readlink -f "$1"
  }
else
  msg "Error: Unable to normalize paths."
  exit 1
fi

if [[ -z "$RBE_server_address" ]]
then
  # Uncommon case: manual repro.  Automatically start/shutdown reproxy.
  dmsg "No env RBE_server_address found.  Re-launching with $reproxy_wrap"
  exec "$reproxy_wrap" -- "$0" "$@"
  # no return
else
  # Common case: already inside `fx build`
  dmsg "Using RBE_server_address=$RBE_server_address"
fi

if [[ "$#" == 0 ]]
then exit
fi

# rewrapper expects input paths to be named relative to the exec_root
# and not the working directory.
# Pass this in from GN/ninja to avoid calculating relpath here.
working_subdir=

# Separate rewrapper options from the command, based on '--'.
local_only=0
use_py_wrapper=0
rewrapper_opts=()

for opt
do
  keep_opt=1
  # Extract optarg from --opt=optarg
  optarg=
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac
  case "$opt" in
    --check-determinism | --compare )
      # These are special modes that are only implemented in the Python wrapper.
      use_py_wrapper=1
      ;;
    --miscomparison-export-dir )
      # expects one argument, forwarded to next wrapper
      use_py_wrapper=1
      ;;
    --local) local_only=1 ;;
    --working-subdir=*) working_subdir="$optarg"; keep_opt=0 ;;
    --) shift; break ;;
  esac
  if [[ "$keep_opt" == 1 ]]
  then rewrapper_opts+=( "$opt" )
  fi
  shift
done

[[ -n "$working_subdir" ]] || { msg "Missing required option --working-subdir." ; exit 1 ;}

# Everything else is the compiler command.
local_compile_cmd=( "$@" )

if [[ "${#local_compile_cmd[@]}" == 0 ]]
then exit
fi

# Only in special debug cases, fallback to the more elaborate Python wrapper.
if [[ "$use_py_wrapper" == 1 ]]
then
  readonly python="$exec_root_rel"/prebuilt/third_party/python3/"${HOST_PLATFORM}"/bin/python3
  exec "$python" -S "$script_dir"/cxx_remote_wrapper.py "${rewrapper_opts[@]}" -- "${local_compile_cmd[@]}"
  # no return
fi

# Assume first token is the compiler (which is what rewrapper does).
compiler="${local_compile_cmd[0]}"
# Infer language from compiler.
case "$compiler" in
  *++*) lang=cxx ;;
  *) lang=c ;;
esac

case "$compiler" in
  *clang*) compiler_type=clang ;;
  *gcc* | *g++* ) compiler_type=gcc ;;
esac
dmsg "compiler type: $compiler_type"

# Detect cases that are unsupported by re-client.
save_temps=0
output=
depfile=
crash_diagnostics_dir=

prev_opt=
for opt in "${local_compile_cmd[@]}"
do
  # handle --option arg
  if [[ -n "$prev_opt" ]]
  then
    eval "$prev_opt"=\$opt
    prev_opt=
    continue
  fi

  # Extract optarg from --opt=optarg
  optarg=
  case "$opt" in
    -*=*) optarg="${opt#*=}" ;;  # remove-prefix, shortest-match
  esac

  case "$opt" in
    -save-temps | --save-temps) save_temps=1 ;;
    -MF ) prev_opt=depfile ;;
    -fcrash-diagnostics-dir) prev_opt=crash_diagnostics_dir ;;
    -fcrash-diagnostics-dir=*) crash_diagnostics_dir="$optarg" ;;
    -o) prev_opt=output ;;
    *.S) local_only=1 ;;  # b/220030106: no plan to support asm preprocessing.
  esac
done

if [[ "$local_only" == 1 ]]
then
  exec "${local_compile_cmd[@]}"
  # no return
fi

[[ -n "$output" ]] || { msg "Missing required compiler option -o" ; exit 1 ;}

# Paths must be relative to exec_root.
remote_input_files=()
remote_output_files=()
remote_output_dirs=()

# TODO(b/302613832): delete this once upstream supports it.
if [[ "$save_temps" == 1 ]]
then
  temp_base="${output##*/}"  # remove paths
  temp_base="${temp_base%%.*}"  # remove all extensions (like ".cc.o")
  temp_exts=( .bc .s )
  if [[ "$lang" == cxx ]]
  then temp_exts+=( .ii )
  else temp_exts+=( .i )
  fi
  for ext in "${temp_exts[@]}"
  do remote_output_files+=( "$working_subdir/$temp_base$ext" )
  done
fi

# TODO(b/272865494): delete this once upstream supports it.
if [[ -n "$crash_diagnostics_dir" ]]
then remote_output_dirs+=( "$working_subdir/$crash_diagnostics_dir" )
fi

# In very special cases, we need to alter the remote compiler command:
# -fdebug-prefix-map maps from local absolute paths to relative paths.
# These local paths need to be adjusted for the remote build environment.
# llvm-dwarfdump can be used to verify that the .o outputs do not
# leak absolute paths.
# TODO(b/303493325): remove this workaround after reclient implements it

if [[ "$compiler_type" == "gcc" ]]
then
  _local_exec_root="$(normalize_path "$exec_root")" || {
    msg "Unable to normalize path: $exec_root"
    exit 1
  }
  remote_compile_cmd=()
  for tok in "${local_compile_cmd[@]}"
  do
    new_tok="$tok"  # unchanged by default
    case "$tok" in
      -fdebug-prefix-map=*=* | -fcanon-prefix-map=*=* | -ffile-prefix-map=*=* )
        new_tok="${tok/$PWD/$remote_working_dir}"
        new_tok="${new_tok/$_local_exec_root/$remote_exec_root}"
        ;;
    esac
    remote_compile_cmd+=( "$new_tok" )
  done
  # Caveat: this remote_compile_cmd won't work with any local execution by
  # rewrapper, e.g. in racing or remote_local_fallback.

  # Need to include gcc support tools for remote execution.
  # See build/rbe/fuchsia.py:gcc_support_tools() definition.
  _gcc_bindir="${compiler%/*}"  # dirname
  _gcc_basename="${compiler##*/}"  # basename
  # Split gcc name, e.g. {x64_64,aarch64}-elf-{g++,gcc}
  IFS=- read -r -a _gcc_components <<< "$_gcc_basename"
  _gcc_arch="${_gcc_components[0]}"
  _gcc_objtype="${_gcc_components[1]}"
  _gcc_tool="${_gcc_components[2]}"
  _gcc_target="$_gcc_arch-$_gcc_objtype"
  _gcc_install_root="${_gcc_bindir%/*}"  # dirname

  _gcc_asm="$_gcc_install_root/$_gcc_target/bin/as"

  _gcc_libexec_base="$_gcc_install_root/libexec/gcc/$_gcc_target"
  # under the libexec_base is a version dir
  for p in "$_gcc_libexec_base"/*  # effectively, does 'ls'
  do _gcc_libexec_dir="$p"
  done
  case "$_gcc_tool" in
    'gcc' ) _parser='cc1' ;;
    'g++' ) _parser='cc1plus' ;;
  esac
  _gcc_parser="$_gcc_libexec_dir/$_parser"

  # Workaround: gcc builds a COMPILER_PATH to its related tools with
  # non-normalized paths like:
  # ".../gcc/linux-x64/bin/../lib/gcc/x86_64-elf/12.2.1/../../../../x86_64-elf/bin"
  # The problem is that every partial path of the non-normalized path needs
  # to exist, even if nothing in the partial path is actually used.
  # Here we need the "lib/gcc/x86_64-elf/VERSION" path to exist in the
  # remote environment.  One way to achieve this is to pick an arbitrary
  # file in that directory to include as a remote input, and all of its
  # parent directories will be created in the remote environment.
  _gcc_version="${_gcc_libexec_dir##*/}"  # basename
  _gcc_lib_base="$_gcc_install_root/lib/gcc/$_gcc_target/$_gcc_version"

  dmsg "gcc parser: $_gcc_parser"
  dmsg "gcc asm: $_gcc_asm"

  remote_input_files+=(
    "${_gcc_parser#"$exec_root_rel/"}"
    "${_gcc_asm#"$exec_root_rel/"}"
    # arbitrarily chosen file, wanted for its parent directories
    "${_gcc_lib_base#"$exec_root_rel/"}"/"crtbegin.o"
  )

else  # clang, doesn't need any command transformations
  remote_compile_cmd=( "${local_compile_cmd[@]}" )
fi

remote_input_files_opt=()
if [[ "${#remote_input_files[@]}" > 0 ]]
then
  # This is an uncommon case and thus allowed to be slow.
  remote_input_files_opt+=( "--inputs=$(join_by_char ',' "${remote_input_files[@]}")" )
fi

remote_output_files_opt=()
if [[ "${#remote_output_files[@]}" > 0 ]]
then
  # This is an uncommon case and thus allowed to be slow.
  remote_output_files_opt+=( "--output_files=$(join_by_char ',' "${remote_output_files[@]}")" )
fi

remote_output_dirs_opt=()
if [[ "${#remote_output_dirs[@]}" > 0 ]]
then
  # This is an uncommon case and thus allowed to be slow.
  remote_output_dirs_opt+=( "--output_directories=$(join_by_char ',' "${remote_output_dirs[@]}")" )
fi

# Set the rewrapper options.
remote_cmd_prefix+=(
  # RBE_v=3  # for verbose logging
  "$rewrapper"
  --exec_root "$exec_root"
  --cfg "$cfg"

  # for C++
  --labels=type=compile,compiler=clang,lang=cpp
  --canonicalize_working_dir=true

  "${remote_input_files_opt[@]}"
  "${remote_output_files_opt[@]}"
  "${remote_output_dirs_opt[@]}"

  "${rewrapper_opts[@]}"
  --
)

# set -x
if [[ "$compiler_type" == "clang" ]]
then
  # fast
  exec "${remote_cmd_prefix[@]}" "${remote_compile_cmd[@]}"

else  # gcc (needs additional workarounds)
  "${remote_cmd_prefix[@]}" "${remote_compile_cmd[@]}"
  status="$?"

  if [[ "$status" == 0 && -n "$depfile" && -f "$depfile" ]]
  then
    dmsg "Rewriting depfile: $depfile"
    # Remote-built depfile may still leak remote environment paths,
    # even when '-no-canonical-prefix' is used.  Relativize:
    sed -i -e "s|$remote_working_dir/|./|g" -e "s|$remote_exec_root/|../../|g" "$depfile"
    # Performance: This incurs subprocess overhead from calling sed.
  fi
  exit "$status"
fi
