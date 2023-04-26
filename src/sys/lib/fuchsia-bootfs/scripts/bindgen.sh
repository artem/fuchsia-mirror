# Copyright 2019 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This script is for MANUAL USE ONLY.
# Do not include this script in any build processes.

read -r -d '' HEADER  << EOM
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Generated by garnet/public/rust/fuchsia-bootfs/scripts/bindgen.sh
EOM

source_file="${FUCHSIA_DIR}/sdk/lib/zbi-format/include/lib/zbi-format/internal/bootfs.h"
target_file="${FUCHSIA_DIR}/garnet/public/rust/fuchsia-bootfs/src/bootfs.rs"

bindgen \
  -o "${target_file}" \
    "${source_file}" \
  --with-derive-default \
  -- -I "${FUCHSIA_DIR}/zircon/system/public"

echo -e "${HEADER}\n\n$(cat ${target_file})" > ${target_file}
