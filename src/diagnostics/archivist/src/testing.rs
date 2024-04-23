// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use lazy_static::lazy_static;
use moniker::ExtendedMoniker;
use std::sync::Arc;

lazy_static! {
    pub static ref TEST_IDENTITY: Arc<ComponentIdentity> = {
        Arc::new(ComponentIdentity::new(
            ExtendedMoniker::parse_str("./fake-test-env/test-component").unwrap(),
            "fuchsia-pkg://fuchsia.com/testing123#test-component.cm",
        ))
    };
}
