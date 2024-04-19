// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        additional_boot_args::AdditionalBootConfigCollection, static_pkgs::StaticPkgsCollection,
        zbi::Zbi,
    },
    anyhow::{anyhow, Context, Result},
    scrutiny::{model::controller::DataController, model::model::*},
    scrutiny_utils::{
        artifact::{ArtifactReader, FileArtifactReader},
        build_checks,
    },
    serde::{Deserialize, Serialize},
    serde_json::{json, value::Value},
    std::{collections::HashMap, fs::read_to_string, path::PathBuf, sync::Arc},
};

/// The input to the PreSigningController is the policy to validate against.
/// The data to validate will be implicitly collected by inclusion of plugins and accessed by
/// querying the DataModel for the relevant DataCollection.
#[derive(Deserialize, Serialize)]
pub struct PreSigningRequest {
    /// Path to policy file with contents deserializable into build_checks::BuildCheckSpec.
    pub policy_path: String,
    /// Path to directory containing golden files for policy to consume.
    pub golden_files_dir: String,
}

/// The output of the PreSigningController is a set of errors found if any of the checks fail.
#[derive(Deserialize, Serialize)]
pub struct PreSigningResponse {
    pub errors: Vec<build_checks::ValidationError>,
}

#[derive(Default)]
pub struct PreSigningController;

impl DataController for PreSigningController {
    fn query(&self, model: Arc<DataModel>, request: Value) -> Result<Value> {
        let pre_signing_request: PreSigningRequest =
            serde_json::from_value(request).context("Failed to parse PreSigningRequest")?;
        let policy_str = read_to_string(&pre_signing_request.policy_path).context(format!(
            "Failed to read policy file from {:?}",
            &pre_signing_request.policy_path
        ))?;
        let policy: build_checks::BuildCheckSpec = serde_json5::from_str(&policy_str).context(
            format!("Failed to parse policy file from {:?}", &pre_signing_request.policy_path),
        )?;

        let boot_config_model = model
            .get::<AdditionalBootConfigCollection>()
            .context("Failed to get AdditionalBootConfigCollection")?;
        if boot_config_model.errors.len() > 0 {
            return Err(anyhow!("Cannot validate additional boot args: AdditionalBootConfigCollector reported errors {:?}", boot_config_model.errors));
        }

        let boot_args_data = match boot_config_model.additional_boot_args.clone() {
            Some(data) => data,
            None => HashMap::new(),
        };

        let static_pkgs =
            model.get::<StaticPkgsCollection>().context("Failed to get StaticPkgsCollection")?;
        if static_pkgs.errors.len() > 0 {
            return Err(anyhow!("Cannot perform validations involving static packages: StaticPkgCollector reported errors {:?}", static_pkgs.errors));
        }

        let static_pkgs_map = static_pkgs.static_pkgs.clone().unwrap_or(HashMap::new());

        // Remove variant from package name and convert hash to string.
        // Build checks validation expects a map of package names to merkle hash strings.
        let static_pkgs_map: HashMap<String, String> = static_pkgs_map
            .into_iter()
            .map(|((name, _variant), hash)| (name.as_ref().to_string(), hash.to_string()))
            .collect();

        let zbi_data = model.get::<Zbi>().context("Failed to get ZbiCollection")?;
        let bootfs_files = &zbi_data.bootfs_files.bootfs_files;

        let mut blobs_artifact_reader: Box<dyn ArtifactReader> =
            Box::new(FileArtifactReader::new(&PathBuf::new(), &model.config().blobs_directory()));

        let validation_errors = build_checks::validate_build_checks(
            policy,
            boot_args_data,
            bootfs_files,
            static_pkgs_map,
            &mut blobs_artifact_reader,
            &pre_signing_request.golden_files_dir,
        )
        .context("Failed to run validation checks")?;

        Ok(json!(PreSigningResponse { errors: validation_errors }))
    }

    fn description(&self) -> String {
        "Runs a set of checks to verify builds for signing".to_string()
    }
}
