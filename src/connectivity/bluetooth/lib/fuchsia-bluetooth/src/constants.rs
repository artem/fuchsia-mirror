// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fuchsia_zircon::Duration;

pub const DEV_DIR: &str = "/dev";
pub const HOST_DEVICE_DIR: &str = "class/bt-host";
pub const HCI_DEVICE_DIR: &str = "class/bt-hci";

// Constants for creating bt-host component in bt-init and integration tests
pub const BT_HOST_COLLECTION: &str = "bt-host-collection";
pub const BT_HOST: &str = "bt-host";
// TODO(https://fxbug.dev/42085245): Eventually change to fuchsia-pkg://fuchsia.com/bt-host#meta/bt-host.cm
pub const BT_HOST_URL: &str = "bt-host-component#meta/bt-host.cm";

// Use a timeout of 4 minutes on integration tests.
//
// This time is expected to be:
//   a) sufficient to avoid flakes due to infra or resource contention, except in many standard
//      deviations of unlikeliness
//   b) short enough to still provide useful feedback in those cases where asynchronous operations
//      fail
//   c) short enough to fail before the overall infra-imposed test timeout (currently 5 minutes),
//      so that we can produce specific test-relevant information in the case of failure.
pub const INTEGRATION_TIMEOUT: Duration = Duration::from_minutes(4);
