// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

include!("../../zerocopy-derive/tests/util.rs");

extern crate zerocopy;

use zerocopy::transmute_ref;

fn main() {}

// `transmute_ref` requires that the source type implements `AsBytes`
const SRC_NOT_AS_BYTES: &AU16 = transmute_ref!(&NotZerocopy(AU16(0)));
