#!/usr/bin/env bash

# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script updates the list of external dependencies in imports.go, which
# allows `go mod` to operate in that directory to update and vendor those
# dependencies.

set -euo pipefail

cd "$(dirname "$0")"

source ../../tools/devshell/lib/vars.sh

UPDATE=0
while getopts "u" flag
do
  case "${flag}" in
    u)
      UPDATE=1
      ;;
    *)
      echo "Invalid option: $flag"
      exit 1
      ;;
  esac
done

# Create various symlinks needed for the `go list` call below.
fx-command-run setup-go

function cleanup() {
  rm "$COBALT_DST"
  find "$TMP" -maxdepth 1 -mindepth 1 -exec mv {} . \;
}
trap cleanup EXIT

# Escape third_party/go.mod.
readonly COBALT_DST=$FUCHSIA_DIR/cobalt
ln -s "$FUCHSIA_DIR"/third_party/cobalt "$COBALT_DST"

# Move jiri-managed repositories out of the module.
TMP=$(mktemp -d)
if ignored=$(git check-ignore ./*); then
  echo "$ignored" | xargs --no-run-if-empty -I % mv % "$TMP"/%
fi

GOROOTBIN=$(fx-command-run go env GOROOT)/bin
GO=$GOROOTBIN/go
GOFMT=$GOROOTBIN/gofmt

IMPORTS=()
for dir in $FUCHSIA_DIR $FUCHSIA_DIR/cobalt $FUCHSIA_DIR/third_party/syzkaller/sys/syz-sysgen; do
  while IFS='' read -r line; do IMPORTS+=("$line"); done < <(cd "$dir" && git ls-files -- \
    '*.go' ':!third_party/golibs/vendor' |
    xargs dirname |
    sort | uniq |
    sed 's|^|./|' |
    xargs "$GO" list -tags deadlock_detection -mod=readonly -e -f \
      '{{join .Imports "\n"}}{{"\n"}}{{join .TestImports "\n"}}{{"\n"}}{{join .XTestImports "\n"}}' |
    grep -vF go.fuchsia.dev/fuchsia/ |
    # Apparently we generate these normally checked-in files?
    grep -vF 'go.chromium.org/luci' |
    grep -F . |
    sort | uniq |
    xargs "$GO" list -mod=readonly -e -f \
      '{{if not .Goroot}}_ "{{.ImportPath}}"{{end}}' |
    grep -vF github.com/google/syzkaller/ |
    sort | uniq)
done

IMPORTS_STR=$(
  IFS=$'\n'
  echo "${IMPORTS[*]}"
)

THIS_SCRIPT=$(realpath --relative-to="$FUCHSIA_DIR" "$(basename "$0")")
printf '// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Code generated by %s; DO NOT EDIT.

package imports

import (\n%s\n)' "//$THIS_SCRIPT" "$IMPORTS_STR" | $GOFMT -s >imports.go

if [ $UPDATE -eq 1 ]; then
  # The following two lines used to be in the opposite order, which incorrectly
  # caused `go get -u` to fetch gvisor from the default branch instead of the
  # go branch.
  $GO get -u gvisor.dev/gvisor@go
  $GO get -u
  # Starting with https://github.com/theupdateframework/go-tuf/commit/1e35084,
  # go-tuf/repo.go logs directly to stdout.
  #
  # TODO(https://github.com/theupdateframework/go-tuf/issues/376): Remove this.
  $GO get -u github.com/theupdateframework/go-tuf@fc0190d925a5f43942c51c5e519b8fce3b025610
else
  $GO get
fi

$GO mod tidy
$GO mod vendor

"$FUCHSIA_DIR/scripts/fuchsia-vendored-python" update_sources.py \
  --build-file='BUILD.gn' \
  --golibs-dir='.' > "${TMP}/BUILD.gn"
mv "${TMP}/BUILD.gn" 'BUILD.gn'
"${PREBUILT_GN}" format 'BUILD.gn'
