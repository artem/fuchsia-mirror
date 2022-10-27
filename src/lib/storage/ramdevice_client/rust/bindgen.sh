#!/bin/bash

# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
# This script generates Rust bindings for ramdevice_client.

# Determine paths for this script and its directory, and set $FUCHSIA_DIR.
readonly FULL_PATH="${BASH_SOURCE[0]}"
readonly SCRIPT_DIR="$(cd "$(dirname "${FULL_PATH}")" >/dev/null 2>&1 && pwd)"
source "${SCRIPT_DIR}/../../../../../tools/devshell/lib/vars.sh"

set -eu

cd "${SCRIPT_DIR}"

readonly RELPATH="${FULL_PATH#${FUCHSIA_DIR}/}"
readonly BINDGEN="${PREBUILT_RUST_BINDGEN_DIR}/bindgen"

# Generate annotations for the top of the generated source file.
readonly RAW_LINES="// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Generated by ${RELPATH} using $("${BINDGEN}" --version)

#![allow(dead_code)]
"

readonly OUTPUT="src/ramdevice_sys.rs"

"${BINDGEN}" \
    "${FUCHSIA_DIR}"/src/lib/storage/ramdevice_client/cpp/include/ramdevice-client/ramdisk.h \
    --disable-header-comment \
    --no-layout-tests \
    --raw-line "${RAW_LINES}" \
    --allowlist-function '(ramdisk_|wait_for_device).*' \
    --output "${OUTPUT}" \
    -- \
    -I "${FUCHSIA_DIR}"/zircon/system/public

fx format-code --files=${OUTPUT}
