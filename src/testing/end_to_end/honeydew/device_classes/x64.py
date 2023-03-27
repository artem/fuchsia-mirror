#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""x64 (Fuchsia running on Intel NUC) device class."""

from honeydew.device_classes import fuchsia_device_base


class X64(fuchsia_device_base.FuchsiaDeviceBase):
    """X64 device class.

    Args:
        device_name: Device name returned by `ffx target list`.

        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

        device_ip_address: Device IP (V4|V6) address. If not provided, attempts
            to resolve automatically.
    """
