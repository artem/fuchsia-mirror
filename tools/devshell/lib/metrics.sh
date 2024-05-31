#!/bin/bash
# Copyright 2019 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Common methods for metrics collection.
#
# Note: For non-shell programs, use metrics_custom_report.sh instead.
#
# Report events to the metrics collector from `fx`, the `fx metrics`, and the
# rare subcommand that has special metrics needs.
#
# How to use it: This file is sourced in //scripts/fx for tracking command
# execution and in //tools/devshell/metrics for managing the metrics collection
# settings. Developers of shell-based subcommands can source this file in their
# subcommand if they need custom event tracking, but only the method
# track-subcommand-custom-event can be used in this context.
#
# This script assumes that vars.sh has already been sourced, since it
# depends on FUCHSIA_DIR being defined correctly.

_METRICS_GA_PROPERTY_ID="UA-127897021-6"
_METRICS_ALLOWS_CUSTOM_REPORTING=( "test" )
# If args match the below, then track capture group 1
_METRICS_TRACK_REGEX=(
    "^run (fuchsia-pkg:\/\/[[:graph:]]*)"
    "^shell (run fuchsia-pkg:\/\/[[:graph:]]*)"
)
# We collect metrics when these operations happen without capturing all of
# their args.
_METRICS_TRACK_COMMAND_OPS=(
    "publish cache"
    "shell activity"
    "shell basename"
    "shell bssl"
    "shell bt-avdtp-tool"
    "shell bt-avrcp-controller"
    "shell bt-cli"
    "shell bt-hci-emulator"
    "shell bt-hci-tool"
    "shell bt-le-central"
    "shell bt-le-peripheral"
    "shell bt-pairing-tool"
    "shell bt-snoop-cli"
    "shell bugreport"
    "shell cal"
    "shell cat"
    "shell catapult_converter"
    "shell cksum"
    "shell cmp"
    "shell cols"
    "shell comm"
    "shell cowsay"
    "shell cp"
    "shell crashpad_database_util"
    "shell cs"
    "shell curl"
    "shell cut"
    "shell date"
    "shell dirname"
    "shell du"
    "shell echo"
    "shell ed"
    "shell env"
    "shell expand"
    "shell expr"
    "shell false"
    "shell far"
    "shell fdio_spawn_helper"
    "shell fdr"
    "shell find"
    "shell fold"
    "shell fuchsia_benchmarks"
    "shell gltf_export"
    "shell grep"
    "shell head"
    "shell hostname"
    "shell input"
    "shell iperf3"
    "shell iquery"
    "shell join"
    "shell limbo_client"
    "shell link"
    "shell locate"
    "shell log_listener"
    "shell ls"
    "shell md5sum"
    "shell mediasession_cli_tool"
    "shell mkdir"
    "shell mktemp"
    "shell mv"
    "shell net"
    "shell netdump"
    "shell nl"
    "shell od"
    "shell onet"
    "shell paste"
    "shell pathchk"
    "shell pkgctl"
    "shell pm"
    "shell present_view"
    "shell print_input"
    "shell printenv"
    "shell printf"
    "shell process_input_latency_trace"
    "shell pwd"
    "shell readlink"
    "shell rev"
    "shell rm"
    "shell rmdir"
    "shell run"
    "shell run_simplest_app_benchmark.sh"
    "shell run-test-suite"
    "shell scp"
    "shell screencap"
    "shell sed"
    "shell seq"
    "shell set_renderer_params"
    "shell sh"
    "shell sha1sum"
    "shell sha224sum"
    "shell sha256sum"
    "shell sha384sum"
    "shell sha512-224sum"
    "shell sha512-256sum"
    "shell sha512sum"
    "shell signal_generator"
    "shell sleep"
    "shell snapshot"
    "shell sort"
    "shell split"
    "shell sponge"
    "shell ssh"
    "shell ssh-keygen"
    "shell stash_ctl"
    "shell strings"
    "shell sync"
    "shell system-update-checker"
    "shell tail"
    "shell tar"
    "shell tee"
    "shell test"
    "shell tftp"
    "shell time"
    "shell touch"
    "shell tr"
    "shell trace"
    "shell true"
    "shell tsort"
    "shell tty"
    "shell uname"
    "shell unexpand"
    "shell uniq"
    "shell unlink"
    "shell update"
    "shell uudecode"
    "shell uuencode"
    "shell vim"
    "shell virtual_audio"
    "shell vol"
    "shell wav_recorder"
    "shell wc"
    "shell which"
    "shell whoami"
    "shell wlan"
    "shell xargs"
    "shell xinstall"
    "shell yes"
)

_METRICS_TRACK_UNKNOWN_OPS=( "shell" )

# These variables need to be global, but readonly (or declare -r) declares new
# variables as local when they are source'd inside a function.
# "declare -g -r" is the right way to handle it, but it is not supported in
# old versions of Bash, particularly in the one in MacOS. The alternative is to
# make them global first via the assignments above and marking they readonly
# later.
readonly _METRICS_GA_PROPERTY_ID _METRICS_ALLOWS_CUSTOM_REPORTING _METRICS_TRACK_REGEX _METRICS_TRACK_COMMAND_OPS _METRICS_TRACK_UNKNOWN_OPS

# To properly enable unit testing, METRICS_CONFIG is not read-only
METRICS_CONFIG="${FUCHSIA_DIR}/.fx-metrics-config"

_METRICS_DEBUG=0
_METRICS_DEBUG_LOG_FILE=""
_METRICS_USE_VALIDATION_SERVER=0

INIT_WARNING=$'Please opt in or out of fx metrics collection.\n'
INIT_WARNING+=$'You will receive this warning until an option is selected.\n'
INIT_WARNING+=$'To check what data we collect, run `fx metrics`\n'
INIT_WARNING+=$'To opt in or out, run `fx metrics <enable|disable>\n'

# Each Analytics batch call can send at most this many hits.
declare -r BATCH_SIZE=25
# Keep track of how many hits have accumulated.
hit_count=0
# Holds events for the current batch
events=()

function __is_in {
  local v="$1"
  shift
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "$v" ]]; then
      return 0
    fi
    shift
  done
  return 1
}

function __is_in_regex {
  local v="$1"
  shift
  while [[ $# -gt 0 ]]; do
    if [[ $v =~ $1 ]]; then
      return 0
    fi
    shift
  done
  return 1
}

function _read-other-tools-analytics-uuid {
  local base_dir
  case "${HOST_OS}" in
    linux)
      base_dir="${XDG_DATA_HOME}"
      if [[ -z "${base_dir}" ]]; then
        base_dir="${HOME}/.local/share"
      fi
      ;;
    mac)
      base_dir="${HOME}/Library/Application Support"
      ;;
  esac
  base_dir="${base_dir}/Fuchsia/metrics"
  if [[ "$(cat "${base_dir}/analytics-status" 2>/dev/null)" == 1 ]]; then
    echo "$(cat "${base_dir}/uuid" 2>/dev/null)"
  fi
}

function metrics-read-config {
  METRICS_UUID=""
  METRICS_ENABLED=0
  _METRICS_DEBUG_LOG_FILE=""
  if [[ ! -f "${METRICS_CONFIG}" ]]; then
    return 1
  fi
  source "${METRICS_CONFIG}"
  if [[ $METRICS_ENABLED == 1 && -z "$METRICS_UUID" ]]; then
    METRICS_ENABLED=0
    return 1
  fi
  OTHER_TOOLS_ANALYTICS_UUID="$(_read-other-tools-analytics-uuid)"
  return 0
}

function metrics-write-config {
  local enabled=$1
  if [[ "$enabled" -eq "1" ]]; then
    local uuid="$2"
    if [[ $# -gt 2 ]]; then
      local debug_logfile="$3"
    fi
  fi
  local -r tempfile="$(mktemp)"

  # Exit trap to clean up temp file
  trap "[[ -f \"${tempfile}\" ]] && rm -f \"${tempfile}\"" EXIT

  {
    echo "# Autogenerated config file for fx metrics. Run 'fx help metrics' for more information."
    echo "METRICS_ENABLED=${enabled}"
    echo "METRICS_UUID=\"${uuid}\""
    if [[ -n "${debug_logfile}" ]]; then
      echo "_METRICS_DEBUG_LOG_FILE=${debug_logfile}"
    fi
  } >> "${tempfile}"
  # Only rewrite the config file if content has changed
  if ! cmp --silent "${tempfile}" "${METRICS_CONFIG}" ; then
    mv -f "${tempfile}" "${METRICS_CONFIG}"
  fi
}

function metrics-read-and-validate {
  local hide_init_warning=$1
  if ! metrics-read-config; then
    if [[ $hide_init_warning -ne 1 ]]; then
      fx-warn "${INIT_WARNING}"
    fi
    return 1
  fi
  return 0
}

function metrics-set-debug-logfile {
  _METRICS_DEBUG_LOG_FILE="$1"
  return 0
}

function metrics-get-debug-logfile {
  if [[ -n "$_METRICS_DEBUG_LOG_FILE" ]]; then
    echo "$_METRICS_DEBUG_LOG_FILE"
  fi
}

function metrics-maybe-log {
  local filename="$(metrics-get-debug-logfile)"
  if [[ -n "$filename" ]]; then
    if [[ ! -f "$filename" && -w $(dirname "$filename") ]]; then
      touch "$filename"
    fi
    if [[ -w "$filename" ]]; then
      TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
      echo -n "${TIMESTAMP}:" >> "$filename"
      for i in "$@"; do
        if [[ "$i" =~ ^"--" ]]; then
          continue # Skip switches.
        fi
        # Space before $i is intentional.
        echo -n " $i" >> "$filename"
      done
      # Add a newline at the end.
      echo >> "$filename"
    fi
  fi
}

# Arguments:
#   - the name of the fx subcommand
#   - event action
#   - (optional) event label
function track-subcommand-custom-event {
  exec 1>/dev/null
  exec 2>/dev/null
  local subcommand="$1"
  local event_action="$2"
  shift 2
  local event_label="$*"

  # Only allow custom arguments to subcommands defined in # $_METRICS_ALLOWS_CUSTOM_REPORTING
  if ! __is_in "$subcommand" "${_METRICS_ALLOWS_CUSTOM_REPORTING[@]}"; then
    return 1
  fi

  # Limit to the first 100 characters
  # The Analytics API supports up to 500 bytes, but it is likely that
  # anything larger than 100 characters is an invalid execution and/or not
  # what we want to track.
  event_label=${event_label:0:100}

  local hide_init_warning=1
  metrics-read-and-validate $hide_init_warning
  if [[ $METRICS_ENABLED == 0 ]]; then
    return 0
  fi

  event_params=$(fx-command-run jq -c -n \
    --arg subcommand "${subcommand}" \
    --arg action "${event_action}" \
    --arg label "${event_label}" \
    '$ARGS.named')
  _add-to-analytics-batch "custom" "${event_params}"
  # Send any remaining hits.
  _send-analytics-batch
  return 0
}

# Arguments:
#   - the name of the fx subcommand
#   - args of the subcommand
function track-command-execution {
  exec 1>/dev/null
  exec 2>/dev/null
  local subcommand="$1"
  shift
  local args="$*"
  local subcommand_op;
  if [[ $# -gt 0 ]]; then
    local subcommand_arr=( $args )
    subcommand_op=${subcommand_arr[0]}
  fi

  local hide_init_warning=0
  if [[ "$subcommand" == "metrics" ]]; then
    hide_init_warning=1
  fi
  metrics-read-and-validate $hide_init_warning
  if [[ $METRICS_ENABLED == 0 ]]; then
    return 0
  fi

  if [[ "$subcommand" == "set" ]]; then
    # Add separate fx_set hits for packages
    _process-fx-set-command "$@"
  fi

  if __is_in_regex "${subcommand} ${args}" "${_METRICS_TRACK_REGEX[@]}"; then
    # Limit to the first 100 characters of arguments.
    # The GA4 API supports up to 100 characters for parameter values
    args="${BASH_REMATCH[1]:0:100}"
  elif [ -n "${subcommand_op}" ]; then
    if __is_in "${subcommand} ${subcommand_op}" \
        "${_METRICS_TRACK_COMMAND_OPS[@]}"; then
      # Track specific subcommand arguments (instead of all of them)
      args="${subcommand_op}"
    elif __is_in "${subcommand}" "${_METRICS_TRACK_UNKNOWN_OPS[@]}"; then
      # We care about the fact there was a subcommand_op, but we haven't
      # explicitly opted it into metrics collection.
      args="\$unknown_subcommand"
    else
      args=""
    fi
  else
    # Track no arguments
    args=""
  fi

  event_params=$(fx-command-run jq -c -n \
    --arg subcommand "${subcommand}" \
    --arg args "${args}" \
    '$ARGS.named')

  _add-to-analytics-batch "invoke" "${event_params}"
  # Send any remaining hits.
  _send-analytics-batch
  return 0
}

# Arguments:
#   - args of `fx set`
function _process-fx-set-command {
  while [[ $# -ne 0 ]]; do
    case $1 in
      --with)
        shift # remove "--with"
        _add-fx-set-hit "fx-with" "$1"
        ;;
      --with-base)
        shift # remove "--with-base"
        _add-fx-set-hit "fx-with-base" "$1"
        ;;
      *)
        ;;
    esac
    shift
  done
}

# Arguments:
#   - category name, either "fx-with" or "fx-with-base"
#   - package(s) following "--with" or "--with-base" switch
function _add-fx-set-hit {
  category="$1"
  packages="$2"
  # Packages argument can be a comma-separated list.
  IFS=',' read -ra packages_parts <<< "$packages"
  for p in "${packages_parts[@]}"; do
    event_params=$(fx-command-run jq -c -n \
      --arg with_type "${category}" \
      --arg package_name "${p}" \
      '$ARGS.named')

    _add-to-analytics-batch "set" "${event_params}"
  done
}

# Arguments:
#   - time taken to complete (milliseconds)
#   - exit status
#   - the name of the fx subcommand
#   - args of the subcommand
function track-command-finished {
  exec 1>/dev/null
  exec 2>/dev/null
  timing=$1
  exit_status=$2
  subcommand=$3
  is_remote=$4
  shift 4
  args="$*"

  metrics-read-config
  if [[ $METRICS_ENABLED == 0 ]]; then
    return 0
  fi

  # Only track arguments to the subcommands in $_METRICS_TRACK_ALL_ARGS
  if ! __is_in "$subcommand" "${_METRICS_TRACK_ALL_ARGS[@]}"; then
    args=""
  else
    # Limit to the first 100 characters of arguments.
    # The Analytics API supports up to 500 bytes, but it is likely that
    # anything larger than 100 characters is an invalid execution and/or not
    # what we want to track.
    args=${args:0:100}
  fi

  event_params=$(fx-command-run jq -c -n \
    --arg subcommand "${subcommand}" \
    --arg args "${args}" \
    --arg exit_status "${exit_status}" \
    --arg is_remote "${is_remote}" \
    --argjson timing "${timing}" \
    '$ARGS.named')

  _add-to-analytics-batch "finish" "${event_params}"


  # Send any remaining hits.
  _send-analytics-batch
  return 0
}

# Arguments:
#   - feature name
#   - 0 for enabled, 1 for disabled
function track-feature-status {
  if [[ "${HOST_OS}" == "mac" ]]; then
    return
  fi
  exec 1>/dev/null
  exec 2>/dev/null

  local feature=$1
  local is_disabled=$2
  local status

  if [[ "${is_disabled}" -eq 0 ]]; then
      status="enabled"
  else
      status="disabled"
  fi

  event_params=$(fx-command-run jq -c -n \
    --arg feature "${feature}" \
    --arg status "${status}" \
    '$ARGS.named')

  _add-to-analytics-batch "feature" "${event_params}"
}


# Add an analytics hit with the given args to the batch of hits. This will trigger
# sending a batch when the batch size limit is hit.
#
# Arguments:
#   - analytics arguments, e.g. "t=event" "ec=fx" etc.
function __add-to-analytics-batch {
  if [[ $# -eq 0 ]]; then
    return 0
  fi

  event_name=$1
  shift

  timestamp_micros=$("${PREBUILT_PYTHON3}" \
    -c 'import time; print(int(time.time() * 1000000))')
  if [[ $# -eq 0 ]]; then
    event=$(fx-command-run jq -c -n \
      --arg name "${event_name}" \
      --argjson timestamp_micros "${timestamp_micros}" \
      '$ARGS.named')
  else
    event=$(fx-command-run jq -c -n \
      --arg name "${event_name}" \
      --argjson params "$*" \
      --argjson timestamp_micros "${timestamp_micros}" \
      '$ARGS.named')
  fi
  events+=("$event")

  (( hit_count += 1 ))
  if ((hit_count == BATCH_SIZE)); then
    __send-analytics-batch
  fi
}

# The following list of allowed ninja persistent modes comes from
# https://fuchsia.googlesource.com/third_party/github.com/ninja-build/ninja/+/2005473679afc095025bd6db7d461590f8701e65/src/persistent_mode.cc#338
_ALLOWED_NINJA_PERSISTENT_MODE=( "" "0" "1" "on" "off" "client" "server" )
function _get_ninja_persistent_mode {
  if __is_in "${NINJA_PERSISTENT_MODE}" \
  "${_ALLOWED_NINJA_PERSISTENT_MODE[@]}"; then
    echo "${NINJA_PERSISTENT_MODE}"
  else
    echo unsupported
  fi
}

# Sends the current batch of hits to the Analytics server. As a side effect, clears
# the hit count and batch data.
function __send-analytics-batch {
  if [[ $hit_count -eq 0 ]]; then
    return 0
  fi

  # Construct the measuremnt data
  local user_properties="{\"os\":{\"value\":\"$(uname -s)\"},\
  \"arch\":{\"value\":\"$(uname -m)\"},\
  \"shell\":{\"value\":\"$(_app_name)\"},\
  \"shell_version\":{\"value\":\"$(_app_version)\"},\
  \"kernel_release\":{\"value\":\"$(uname -rs)\"},\
  \"ninja_persistent\":{\"value\":\"$(_get_ninja_persistent_mode)\"},\
  \"other_uuid\":{\"value\":\"${OTHER_TOOLS_ANALYTICS_UUID}\"}\
  }"
  local events_json=$(fx-command-run jq -n -c '$ARGS.positional' \
    --jsonargs "${events[@]}")
  local measurement=$(fx-command-run jq -n -c \
    --arg client_id "${METRICS_UUID}" \
    --argjson events "${events_json}" \
    --argjson user_properties "${user_properties}" \
    '$ARGS.named')

  local url_path="mp/collect"
  local url_parameters=$(printf "\x61\x70\x69\x5f\x73\x65\x63\x72\x65\x74=xjiXbh8eSGiExMthvAXd6w&measurement_id=G-L2YHSDD8ZF")
  local result=""
  if [[ $_METRICS_DEBUG == 1 && $_METRICS_USE_VALIDATION_SERVER == 1 ]]; then
    url_path="debug/mp/collect"
  fi
  if [[ $_METRICS_DEBUG == 1 && $_METRICS_USE_VALIDATION_SERVER == 0 ]]; then
    # if testing and not using the validation server, always return 202
    result="202"
  elif [[ $_METRICS_DEBUG == 0 || $_METRICS_USE_VALIDATION_SERVER == 1 ]]; then
    result=$(curl -s -o /dev/null -w "%{http_code}" -L -X POST \
      -H "Content-Type: application/json" \
      --data-raw "${measurement}" \
      "https://www.google-analytics.com/${url_path}?${url_parameters}")
  fi
  metrics-maybe-log "${measurement}" "RESULT=${result}"

  # Clear batch.
  hit_count=0
  events=()
}

# Metrics/analytics are processed asynchronously by the following function.
function _metrics-service {
  # redirect stdout and stderr away from parent process so that parent
  # stdout and stderr can be safely closed. Otherwise, dart will
  # wait for stdout and stderr being closed, defeating the purpose of
  # async analytics processing.
  exec 1>/dev/null
  exec 2>/dev/null
  metrics-read-config
  while read -r -d $'\0' args; do
        local IFS=$'\n'
        read -r -d $'\0' -a request <<< "$args"
        if [[ ${#request[@]} -eq 0 ]]; then
          __send-analytics-batch
        else
          __add-to-analytics-batch "${request[@]}"
        fi
  done <&0
  __send-analytics-batch
}

# Init metrics service by redirecting file descriptor 10 to the process
# substitution of metrics service.
function metrics-init {
  exec 10> >(_metrics-service)
}

# Calls __add-to-analytics-batch asynchronously, via metrics service
function _add-to-analytics-batch {
  if [[ $# -eq 0 ]]; then
    return 0
  fi

  local IFS=$'\n'
  printf "$*\0" >&10
}

# Calls __send-analytics-batch asynchronously, via metrics service
function _send-analytics-batch {
  printf "\0" >&10
}



function _os_data {
  if command -v uname >/dev/null 2>&1 ; then
    uname -rs
  else
    echo "Unknown"
  fi
}

function _app_name {
  if [[ -n "${BASH_VERSION}" ]]; then
    echo "bash"
  elif [[ -n "${ZSH_VERSION}" ]]; then
    echo "zsh"
  else
    echo "Unknown"
  fi
}

function _app_version {
  if [[ -n "${BASH_VERSION}" ]]; then
    echo "${BASH_VERSION}"
  elif [[ -n "${ZSH_VERSION}" ]]; then
    echo "${ZSH_VERSION}"
  else
    echo "Unknown"
  fi
}

# Args:
#   debug_log_file: string with a filename to save logs
#   use_validation_hit_server:
#          0 do not hit any Analytics server (for local tests)
#          1 use the Analytics validation Hit server (for integration tests)
#   config_file: string with a filename to save the config file. Defaults to
#          METRICS_CONFIG
function _enable_testing {
  _METRICS_DEBUG_LOG_FILE="$1"
  _METRICS_USE_VALIDATION_SERVER=$2
  if [[ $# -gt 2 ]]; then
    METRICS_CONFIG="$3"
  fi
  _METRICS_DEBUG=1
  METRICS_UUID="TEST"
}
