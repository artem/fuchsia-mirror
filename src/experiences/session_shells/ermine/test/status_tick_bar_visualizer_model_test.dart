// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// ignore_for_file: implementation_imports

import 'dart:math';

import 'package:ermine_library/src/widgets/status_tick_bar_visualizer.dart';
import 'package:test/test.dart';

void main() {
  test(
      'test to confirm StatusTickBarVisualizerModel contains non-breaking default values',
      () {
    StatusTickBarVisualizerModel testTickBarModel =
        StatusTickBarVisualizerModel();
    expect(testTickBarModel.barValue, 'loading...');
    expect(testTickBarModel.barFill, greaterThanOrEqualTo(0));
    expect(
        testTickBarModel.barFill, lessThanOrEqualTo(testTickBarModel.barMax));
    expect(testTickBarModel.barMax, greaterThanOrEqualTo(0));
    expect(testTickBarModel.barMax,
        greaterThanOrEqualTo(testTickBarModel.barFill));
    expect(testTickBarModel.tickMax, greaterThan(0));
    expect(testTickBarModel.barFirst, isNotNull);
  });

  test('test to confirm StatusTickBarVisualizerModel setters work properly',
      () {
    StatusTickBarVisualizerModel testTickBarModel =
        StatusTickBarVisualizerModel();
    // Check setter for barValue
    String initialBarValue = testTickBarModel.barValue;
    expect(testTickBarModel.barValue, initialBarValue);
    String testValue = 'testBarValue';
    testTickBarModel.barValue = testValue;
    expect(testTickBarModel.barValue, testValue);
    // Check setter for barFill
    double initialBarFill = testTickBarModel.barFill;
    expect(testTickBarModel.barFill, initialBarFill);
    double randomBarFill = Random().nextDouble() * 100;
    testTickBarModel.barFill = randomBarFill;
    expect(testTickBarModel.barFill, randomBarFill);
    // Check setter for barMax
    double initialBarMax = testTickBarModel.barMax;
    expect(testTickBarModel.barMax, initialBarMax);
    double randomBarMax = Random().nextDouble() * 100;
    testTickBarModel.barMax = randomBarMax;
    expect(testTickBarModel.barMax, randomBarMax);
  });
}
