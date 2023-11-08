#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains all Fuchsia Infra APIs used in Mobly Driver."""

# Defined in https://osscs.corp.google.com/fuchsia/fuchsia/+/main:tools/botanist/constants/constants.go.
BOT_ENV_TESTBED_CONFIG = "FUCHSIA_TESTBED_CONFIG"

# Defined in https://osscs.corp.google.com/fuchsia/fuchsia/+/main:tools/botanist/targets/target.go
FUCHSIA_DEVICE = "FuchsiaDevice"

# LINT.IfChange
TESTPARSER_PREAMBLE = "======== Mobly config content ========"

TESTPARSER_RESULT_HEADER = "[=====MOBLY RESULTS=====]"
# LINT.ThenChange(//tools/testing/testparser/moblytest.go)
