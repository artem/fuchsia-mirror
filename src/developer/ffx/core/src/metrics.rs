// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::build_info;
use analytics::{add_custom_event, init};
use anyhow::Result;
use std::collections::BTreeMap;

pub const GA_PROPERTY_ID: &str = "UA-127897021-9";

pub const FUCHSIA_DISCOVERY_LEGACY_ENV_VAR_NAME: &str = "FUCSHIA_DISABLED_legacy_discovery";

pub const ANALYTICS_LEGACY_DISCOVERY_CUSTOM_DIMENSION_KEY: &str = "cd4";

pub async fn init_metrics_svc() {
    let build_info = build_info();
    let build_version = build_info.build_version;
    init(String::from("ffx"), build_version, GA_PROPERTY_ID.to_string()).await;
}

fn get_launch_args() -> String {
    let args: Vec<String> = std::env::args().collect();
    // drop arg[0]: executable with hard path
    // TODO(fxb/71028): do we want to break out subcommands for analytics?
    let args_str = &args[1..].join(" ");
    format!("{}", &args_str)
}

fn legacy_discovery_env() -> String {
    let _one = "1".to_string();
    match std::env::var(FUCHSIA_DISCOVERY_LEGACY_ENV_VAR_NAME) {
        Ok(_one) => "true",
        _ => "false",
    }
    .to_string()
}

pub async fn add_fx_launch_event() -> Result<()> {
    let launch_args = get_launch_args();
    let mut custom_dimensions = BTreeMap::new();
    add_legacy_discovery_metrics(&mut custom_dimensions);
    add_custom_event(None, Some(&launch_args), None, custom_dimensions).await
}

pub fn add_legacy_discovery_metrics(custom_dimensions: &mut BTreeMap<&str, String>) {
    custom_dimensions
        .insert(ANALYTICS_LEGACY_DISCOVERY_CUSTOM_DIMENSION_KEY, legacy_discovery_env());
}
