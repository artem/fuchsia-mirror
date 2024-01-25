// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42165807): Fix null safety and remove this language version.
// @dart=2.9

import 'dart:convert';

import 'package:image/image.dart';
import 'package:mockito/mockito.dart';
import 'package:sl4f/sl4f.dart';
import 'package:test/test.dart';

class MockSl4f extends Mock implements Sl4f {}

void main(List<String> args) {
  // This is mostly an integration test against the Image package.
  test('screenshot decodes to correct colors', () async {
    final color = Color.fromRgba(12, 34, 56, 78);

    final sl4f = MockSl4f();
    when(sl4f.request('scenic_facade.TakeScreenshot'))
        .thenAnswer((_) => Future.value({
              'info': {'pixel_format': 'Bgra8', 'width': 1, 'height': 1},
              'data': base64Encode([
                getBlue(color),
                getGreen(color),
                getRed(color),
                getAlpha(color)
              ])
            }));

    final Image image = await Scenic(sl4f).takeScreenshot();
    expect(image[0], color);
  });
}
