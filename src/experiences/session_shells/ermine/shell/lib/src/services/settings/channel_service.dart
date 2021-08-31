// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:async';
import 'dart:ui';

import 'package:ermine/src/services/settings/task_service.dart';
import 'package:fidl/fidl.dart' show InterfaceHandle;
import 'package:fidl_fuchsia_update/fidl_async.dart';
import 'package:fidl_fuchsia_update_channelcontrol/fidl_async.dart';
import 'package:flutter/foundation.dart';
import 'package:fuchsia_logger/logger.dart';
import 'package:fuchsia_services/services.dart';

/// Defines a [TaskService] to control channel in QuickSettings.
class ChannelService extends TaskService {
  late final VoidCallback onChanged;
  late final ValueChanged<bool> onConnected;

  var _control = ChannelControlProxy();
  final _manager = ManagerProxy();

  late String _currentChannel;
  late List<String> _availableChannels;
  String _targetChannel = '';
  StreamSubscription? _targetSubscription;
  StreamSubscription? _checkSubscription;

  @override
  Future<void> start() async {
    Incoming.fromSvcPath().connectToService(_control);
    Incoming.fromSvcPath().connectToService(_manager);
    _availableChannels = await _control.getTargetList();

    final name = await _control.getCurrent();
    _currentChannel = _shortNames[name] ?? name;
    onChanged();
  }

  @override
  Future<void> stop() async {
    await _targetSubscription?.cancel();
    await _checkSubscription?.cancel();
    dispose();
  }

  String get currentChannel => _shortNames[_currentChannel] ?? _currentChannel;

  bool get optedIntoUpdates =>
      ((_shortNames[_currentChannel] ?? _currentChannel) != 'devhost') &&
      ((_shortNames[_currentChannel] ?? _currentChannel) != 'fuchsia.com');

  List<String> get channels => _availableChannels;

  String get targetChannel => _shortNames[_targetChannel] ?? _targetChannel;

  set targetChannel(String channel) {
    // If channel name was converted to a short name, convert back.
    var longNames = _shortNames.map((k, v) => MapEntry(v, k));
    _targetChannel = longNames[channel] ?? channel;
    _targetSubscription =
        setTargetChannel(_targetChannel).asStream().listen((_) {});
    onChanged();
  }

  Future<void> setTargetChannel(String channel) async {
    await _control.setTarget(channel);
    var currentTarget = await _control.getTarget();
    if (currentTarget != _targetChannel) {
      log.warning(
          'Failed to set target channel to $channel. Found target: $currentTarget');
    }
  }

  Future<void> checkForUpdates() async {
    _checkSubscription = () async {
      assert(_manager.ctrl.isBound);
      // User initiated the update check
      var initiator = Initiator.user;
      // If update check is already in progress, attach to that in-progress update
      var checkOptions = CheckOptions(
          initiator: initiator, allowAttachingToExistingUpdateCheck: true);
      // Create new monitor for update check
      var monitor = UpdateMonitor(_manager).getInterfaceHandle();
      // Check for updates
      try {
        return _manager.checkNow(checkOptions, monitor);
      } on Exception catch (e) {
        log.warning('Failed to check for updates: $e ${StackTrace.current}');
      }
    }()
        .asStream()
        .listen((_) {});
  }

  /// Returns the mapping of internal channel name to it's short name.
  static final _shortNames = <String, String>{
    '2gmrtg05aspff9bisjxsu46no.fuchsia-updates.googleusercontent.com': 'test',
    '4igty6t46noanfx782kp9ywyc.fuchsia-updates.googleusercontent.com':
        'dogfood',
    'b5cvjayvpm75pukjav4d4hurk.fuchsia-updates.googleusercontent.com': 'beta',
    '4x15snlqjzlsgunidd0q1hj8n.fuchsia-updates.googleusercontent.com': 'stable',
  };

  @override
  void dispose() {
    _control.ctrl.close();
    _control = ChannelControlProxy();
  }
}

class UpdateMonitor extends Monitor {
  final _binding = MonitorBinding();
  final ManagerProxy _manager;

  UpdateMonitor(this._manager);

  InterfaceHandle<Monitor> getInterfaceHandle() => _binding.wrap(this);

  @override
  Future<void> onState(State state) async {
    // TODO(fxb/79588): add state detection to trigger UI updates
    if (state.waitingForReboot != null) {
      try {
        await _manager.performPendingReboot();
      } on Exception catch (e) {
        log.warning(
            'Failed to perform pending reboot: $e ${StackTrace.current}');
      }
    }
  }
}
