// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/42165807): Fix null safety and remove this language version.
// @dart=2.9

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:path/path.dart' as p;

import 'package:status/status.dart';

/// The path, relative to a Fuchsia checkout, where we expect to find the fx
/// executable.
const fxLocation = '.jiri_root/bin/fx';

class GNStatusParser {
  final RegExp argsImportExtractor = RegExp('([^/]*)/([^/]*)\.gni');

  Map gnTitles = {
    'boards': 'Board',
    'products': 'Product',
    'universe_package_labels': [
      'Universe packages',
      '--with argument of `fx set`'
    ],
    'base_package_labels': [
      'Base packages',
      '--with-base argument of `fx set`'
    ],
    'cache_package_labels': [
      'Cache packages',
      '--with-cache argument of `fx set`'
    ],
    'host_labels': ['Host labels', '--with-host argument of `fx set`'],
  };

  List<Item> parseGn({ProcessResult processResult}) {
    if (processResult.exitCode != 0) {
      // Ideally, in the line below, we would `throw Exception(...)` and catch it
      // likewise up top, thought that is proving hard to format correctly in the test
      // ignore: only_throw_errors
      throw 'Unexpected error running fx gn: exit code ${processResult.exitCode}\n---- stderr output:\n${processResult.stderr}\n------';
    } else {
      String json = processResult.stdout;

      List<Map<String, dynamic>> argsTree =
          jsonDecode(json)['child'].cast<Map<String, dynamic>>().toList();

      return collectFromTreeParser(BasicGnParser(argsTree));
    }
  }

  List<Item> collectFromTreeParser(BasicGnParser parser) {
    List<Item> results = [];
    _addImportItems(parser, results);
    _addDirectItems(parser, results);
    _addCalculatedItems(parser, results);
    return results;
  }

  void _addDirectItems(BasicGnParser parser, List<Item> appendTo) {
    for (String key in parser.assignedVariables.keys) {
      dynamic value = parser.assignedVariables[key];
      var title = gnTitles[key];
      if (title != null && (value is! List || value.isNotEmpty)) {
        // ignore: avoid_init_to_null
        var notes = null;
        if (title is List<String>) {
          title = gnTitles[key][0];
          notes = gnTitles[key][1];
        }
        appendTo.add(Item(CategoryType.buildInfo, key, title, value, notes));
      }
    }
  }

  void _addImportItems(BasicGnParser parser, List<Item> appendTo) {
    for (String importClause in parser.imports) {
      Match m = argsImportExtractor.firstMatch(importClause);
      if (m != null) {
        var key = m.group(1);
        var title = gnTitles[key] ?? key;
        appendTo.add(
          Item(CategoryType.buildInfo, key, title, m.group(2), importClause),
        );
      }
    }
  }

  void _addCalculatedItems(BasicGnParser parser, List<Item> appendTo) {
    // goma
    bool isGomaEnabled = parser.assignedVariables['use_goma'] == 'true';
    String gomaDir = parser.assignedVariables['goma_dir'];
    appendTo.add(Item(CategoryType.buildInfo, 'goma', 'Goma',
        isGomaEnabled ? 'enabled' : 'disabled', gomaDir));

    // release
    bool isRelease = parser.assignedVariables['is_debug'] == 'false';
    appendTo.add(Item(CategoryType.buildInfo, 'release', 'Is release?',
        isRelease ? 'true' : 'false', '--release argument of `fx set`'));
  }
}

class GNStatusChecker {
  Future<ProcessResult> checkGn() async {
    EnvReader envReader = EnvReader.shared;

    /// Absolute path to the root of the Fuchsia checkout. Read from the
    /// environment variable.
    String fuchsiaDir = envReader.getEnv('FUCHSIA_DIR');

    /// Path to the fx executable
    String pathToFx = p.join(fuchsiaDir, fxLocation);

    /// Absolute path to the build directory. Read from the environment variable.
    String buildDir = envReader.getEnv('FUCHSIA_BUILD_DIR');

    /// Path to the args.gn
    String pathToArgs = '$buildDir/args.gn';

    return Process.run(
        pathToFx, ['gn', 'format', '--dump-tree=json', pathToArgs]);
  }
}

class ArgsGnCollector implements Collector {
  @override
  Future<List<Item>> collect({
    GNStatusChecker statusChecker,
    GNStatusParser statusParser,
  }) async {
    statusChecker ??= GNStatusChecker();
    statusParser ??= GNStatusParser();

    ProcessResult pr = await statusChecker.checkGn();
    return statusParser.parseGn(processResult: pr);
  }
}
