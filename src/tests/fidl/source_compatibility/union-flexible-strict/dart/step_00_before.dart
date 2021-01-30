// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'package:fidl_fidl_test_unionflexiblestrict/fidl_async.dart' as fidllib;

// [START contents]
void useUnion(fidllib.JsonValue value) {
  switch (value.$tag) {
    case fidllib.JsonValueTag.intValue:
      print('int value: ${value.intValue}');
      break;
    case fidllib.JsonValueTag.stringValue:
      print('string value: ${value.stringValue}');
      break;
    case fidllib.JsonValueTag.$unknown:
      print('unknown variant: ${value.$unknownData}');
      break;
  }
}

// [END contents]
