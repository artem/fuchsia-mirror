// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42165807): Fix null safety and remove this language version.
// @dart=2.9

import 'dart:io';

/// Wraps [Platform] for the sake of mocking env results in tests.
///
/// For now, this class only has a pass-thru to [Platform.environment]
/// since that's all that's currently needed, but in theory it may be nice to
/// use this to test all sorts of platform-related checks we might make.
///
/// Note that, for now, this is unnecessary if there is actually a way to mock
/// environment variables on a per-test basis, but after some research, I am
/// unaware of any such trick.
class EnvReader {
  static EnvReader shared = EnvReader();
  static Map<String, String> environment = Platform.environment;

  String getEnv(String variableName) {
    return environment[variableName];
  }
}
