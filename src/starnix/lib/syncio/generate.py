#!/usr/bin/env -S python3 -B
# allow-non-vendored-python
#
# TODO(b/295039695): We intentionally use the host python3 here instead of
# fuchsia-vendored-python. This script calls out to cbindgen that is not part
# of the Fuchsia repo and must be installed on the local host.
#
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os, sys

sys.path.append(os.path.dirname(os.path.abspath(__file__)) + "/..")
from bindgen import Bindgen

bindgen = Bindgen()

bindgen.raw_lines = """
use zerocopy::{AsBytes, FromBytes, NoCell, FromZeros};
"""

bindgen.include_dirs = [
    "sdk/lib/zxio/include",
    "zircon/third_party/ulib/musl/include",
    "zircon/system/public",
]

bindgen.function_allowlist = ["zxio_.*"]
bindgen.var_allowlist = [
    "ZXIO_SHUTDOWN.*",
    "ZXIO_NODE_PROTOCOL.*",
    "ZXIO_SEEK_ORIGIN.*",
    "ZXIO_ALLOCATE.*",
    "ZXIO_CREATION_MODE.*",
    "E[A-Z]*",
    "AF_.*",
    "SO.*",
    "IP.*",
    "MSG_.*",
    "__bindgen_missing_.*",
]
bindgen.type_allowlist = [
    "cmsghdr.*",
    "in6_.*",
    "sockaddr.*",
    "timespec",
    "timeval",
]

bindgen.set_auto_derive_traits(
    [
        (r"cmsghdr", ["AsBytes, FromBytes", "FromZeros", "NoCell"]),
        (r"in6_pktinfo", ["AsBytes, FromBytes", "FromZeros", "NoCell"]),
        (r"in6_addr*", ["AsBytes, FromBytes", "FromZeros", "NoCell"]),
        (r"timespec", ["AsBytes, FromBytes", "FromZeros", "NoCell"]),
        (r"timeval", ["AsBytes, FromBytes", "FromZeros", "NoCell"]),
    ]
)

bindgen.set_replacements(
    [
        # Remove __bindgen_missing from the start of constants defined in missing_includes.h
        (r"const __bindgen_missing_([a-zA-Z_0-9]+)", "const \\1"),
    ]
)

bindgen.run(
    "src/starnix/lib/syncio/wrapper.h", "src/starnix/lib/syncio/src/zxio.rs"
)
