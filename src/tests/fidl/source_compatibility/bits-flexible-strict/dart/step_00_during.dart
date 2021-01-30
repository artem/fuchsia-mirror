// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'package:fidl_fidl_test_bitsflexiblestrict/fidl_async.dart' as fidllib;

// [START contents]
int useBits(fidllib.Flags bits) {
  if (bits.hasUnknownBits()) {
    return bits.getUnknownBits();
  }

  var result = fidllib.Flags.$none;
  if ((bits & fidllib.Flags.optionA).$value != 0) {
    result |= fidllib.Flags.$mask;
  }
  return result.$value;
}
// [END contents]
