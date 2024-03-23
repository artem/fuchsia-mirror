# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

def _empty_impl(_ctx):
    return []

empty = rule(
    implementation = _empty_impl,
    doc = "Just an empty rule",
)
