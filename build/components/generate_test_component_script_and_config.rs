// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_component_decl::Component;
use fidl_fuchsia_data as fdata;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::prelude::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(about = "Generate a bash wrapper script amd test config for test components.")]
struct Args {
    #[structopt(long, help = "The path to the host binary.")]
    bin_path: String,

    #[structopt(long, help = "The path to the test pilot.")]
    test_pilot: String,

    #[structopt(long, help = "Generated script path.")]
    script_output_filename: String,

    #[structopt(long, help = "Path to component manifest.")]
    component_manifest_path: String,

    #[structopt(long, help = "Path to partial test config.")]
    partial_test_config: String,

    #[structopt(long, help = "Path to test component specific config.")]
    test_component_config: String,

    #[structopt(long, help = "Generated test config path.")]
    test_config_output_filename: String,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, PartialOrd, Ord, Clone)]
pub struct TestTag {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
struct Execution {
    realm: Option<String>,

    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
struct TestConfig {
    #[serde(default)]
    tags: Vec<TestTag>,

    execution: Execution,

    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
struct TestComponentsJsonEntry {
    test_component: TestComponentEntry,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
struct TestComponentEntry {
    label: String,
    moniker: Option<String>,
}

fn generate_bash_script(args: &Args) -> Result<(), Error> {
    let mut file = File::create(&args.script_output_filename)?;
    file.write_all(b"#!/bin/bash\n")?;
    file.write_all(b"\n")?;
    file.write_all(
        format!(
            "{} --fuchsia_test_bin_path {} --fuchsia_test_configuration {}\n",
            args.test_pilot, args.bin_path, args.test_config_output_filename
        )
        .as_bytes(),
    )?;
    let mut perm = file.metadata()?.permissions();
    // Add the executable bit for user, group, and others
    perm.set_mode(0o751);
    file.set_permissions(perm).context("Error setting permissions on generated file")?;
    Ok(())
}

fn read_partial_config(file: &Path) -> Result<TestConfig, Error> {
    let mut buffer = String::new();
    File::open(&file)?.read_to_string(&mut buffer)?;
    let t: TestConfig = serde_json::from_str(&buffer)?;
    Ok(t)
}

fn read_test_components_json(file: &Path) -> Result<Vec<TestComponentsJsonEntry>, Error> {
    let mut buffer = String::new();
    File::open(&file)?.read_to_string(&mut buffer)?;
    let t: Vec<TestComponentsJsonEntry> = serde_json::from_str(&buffer)?;
    Ok(t)
}

fn read_cm(cm_path: &Path) -> Result<Component, Error> {
    let mut cm_contents = Vec::new();
    File::open(&cm_path)?.read_to_end(&mut cm_contents)?;
    let decl: Component = fidl::unpersist(&cm_contents)?;
    Ok(decl)
}

const TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY: &'static str =
    "fuchsia.test.deprecated-allowed-packages";

const REALM_TAG: &'static str = "realm";
const HERMETIC_TAG: &'static str = "hermetic";

fn is_test_hermetically_packaged(decl: &Component) -> bool {
    for facet in decl
        .facets
        .as_ref()
        .unwrap_or(&fdata::Dictionary::default())
        .entries
        .as_ref()
        .unwrap_or(&vec![])
    {
        if facet.key.eq(TEST_DEPRECATED_ALLOWED_PACKAGES_FACET_KEY) {
            if let Some(val) = &facet.value {
                match &**val {
                    fdata::DictionaryValue::StrVec(s) => {
                        return s.is_empty();
                    }
                    _ => {
                        // don't do anything, this tool will not validate facets.
                        return true;
                    }
                }
            }
        }
    }
    return true;
}

fn create_config(args: &Args) -> Result<(), Error> {
    let cm_path = Path::new(&args.component_manifest_path);
    let partial_test_config_path = Path::new(&args.partial_test_config);
    let test_component_config_path = Path::new(&args.test_component_config);
    assert!(cm_path.exists(), "{} should exist.", &args.component_manifest_path);
    assert!(partial_test_config_path.exists(), "{} should exist.", &args.partial_test_config);
    assert!(test_component_config_path.exists(), "{} should exist.", &args.test_component_config);

    let mut partial_test_config = read_partial_config(&partial_test_config_path)?;
    let test_components = read_test_components_json(&test_component_config_path)?;
    let mut hermetic = true;
    let mut realm_tag: String = "hermetic".to_string();

    if test_components.len() > 0 {
        assert!(test_components.len() == 1, "Multiple test components in config");
        if let Some(moniker) = &test_components[0].test_component.moniker {
            partial_test_config.execution.realm = Some(moniker.to_string());
            realm_tag = moniker.clone();
            hermetic = false;
        }
    }
    partial_test_config.tags.push(TestTag { key: REALM_TAG.to_string(), value: realm_tag });

    hermetic = hermetic && is_test_hermetically_packaged(&read_cm(&cm_path)?);

    partial_test_config
        .tags
        .push(TestTag { key: HERMETIC_TAG.to_string(), value: hermetic.to_string() });

    let test_config_json = serde_json::to_string_pretty(&partial_test_config)?;
    std::fs::write(&args.test_config_output_filename, test_config_json)?;

    Ok(())
}

fn main() -> Result<(), Error> {
    let args = Args::from_args();
    create_config(&args)?;
    generate_bash_script(&args)?;

    Ok(())
}
