// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:ui';

import 'package:flutter/material.dart';

/// A collection [AlertModel] instances.
// TODO(fxb/73436): Provide different visibility options.
class AlertsModel extends ChangeNotifier {
  final _alerts = <AlertModel>[];
  final ValueNotifier<AlertModel> _currentAlert =
      ValueNotifier<AlertModel>(null);

  List<AlertModel> get alerts => _alerts;
  AlertModel get currentAlert => _currentAlert.value;

  void addAlert(AlertModel alert) {
    _alerts.add(alert);
    _currentAlert.value = _alerts.last;
    notifyListeners();
  }

  void removeAlert(AlertModel alert) {
    _alerts.remove(alert);
    _currentAlert.value = _alerts.isNotEmpty ? _alerts.last : null;
    notifyListeners();
  }
}

/// A model that holds the content of an alert message.
class AlertModel {
  final String header;
  final String title;
  final String description;
  final _actions = <ActionModel>[];
  final String _id;

  /// The model that holds a list of [AlertModel]s including this one.
  final AlertsModel alerts;

  List<ActionModel> get actions => _actions;
  String get id => _id;

  AlertModel({
    @required this.alerts,
    @required this.title,
    this.header = '',
    this.description = '',
    List<ActionModel> actions = const <ActionModel>[],
  })  : assert(title.isNotEmpty),
        assert(alerts != null),
        _id = '${header ?? 'alert'}_${DateTime.now().toString()}'
            '_${alerts.alerts.length}' {
    _actions.addAll(actions);
  }

  void close() {
    alerts.removeAlert(this);
  }
}

class ActionModel {
  final String name;
  final VoidCallback callback;

  ActionModel(this.name, this.callback);
}
