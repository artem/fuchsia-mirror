#!/usr/bin/env bash
#
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Run this script whenever the $source_file has changed.
# TODO(fxbug.dev/73858): replace this script with a GN bindgen target when there
# is in-tree support.

set -euo pipefail

readonly target_file="$FUCHSIA_DIR/src/connectivity/lib/network-device/rust/src/session/buffer/sys.rs"
readonly source_file_within_tree="src/connectivity/lib/network-device/buffer_descriptor/buffer_descriptor.h"
readonly source_file="$FUCHSIA_DIR/$source_file_within_tree"

readonly copyright_line=$(grep -E "^// Copyright [0-9]+" "${source_file}" || \
  echo "// Copyright $(date +%Y) The Fuchsia Authors. All rights reserved.")

readonly RAW_LINES="${copyright_line}
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Generated by src/connectivity/lib/network-device/rust/scripts/bindgen.sh
// Run the above script whenever $source_file_within_tree
// has changed.
"

bindgen \
  --raw-line "${RAW_LINES}" \
  --no-layout-tests \
  --whitelist-type 'buffer_descriptor' \
  --whitelist-var 'NETWORK_DEVICE_DESCRIPTOR_VERSION' \
  --whitelist-var '__NETWORK_DEVICE_DESCRIPTOR_VERSION' \
  "${source_file}" | \
  grep -vF 'pub type __uint' > "${target_file}"
