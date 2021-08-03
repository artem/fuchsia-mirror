// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'package:ermine/src/states/settings_state.dart';
import 'package:ermine/src/widgets/settings/setting_details.dart';
import 'package:flutter/material.dart';
import 'package:internationalization/strings.dart';

/// Defines a widget to list all channels in [SettingDetails] widget.
class ChannelSettings extends StatelessWidget {
  final SettingsState state;
  final ValueChanged<String> onChange;

  const ChannelSettings({required this.state, required this.onChange});

  @override
  Widget build(BuildContext context) {
    final channels = state.availableChannels.value;
    return SettingDetails(
      title: Strings.channel,
      onBack: state.showAllSettings,
      child: ListView.builder(
          itemCount: channels.length,
          itemBuilder: (context, index) {
            final channel = channels[index];
            return ListTile(
              title: Text(channel),
              subtitle: index == 0 ? Text(Strings.currentChannel) : null,
              onTap: () => onChange(channels[index]),
            );
          }),
    );
  }
}
