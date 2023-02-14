#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for a component capable device."""

import abc

from honeydew.interfaces.affordances import component_base


# pylint: disable=too-few-public-methods
class ComponentCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    Component affordance."""

    @property
    @abc.abstractmethod
    def component(self) -> component_base.ComponentBase:
        """Returns a component affordance object.

        Returns:
            component_base.ComponentBase object
        """
