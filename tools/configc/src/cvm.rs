// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(fuchsia_api_level_at_least = "20")]
use {
    crate::common::load_manifest,
    anyhow::anyhow,
    anyhow::{Context as _, Error},
    argh::FromArgs,
    cm_rust::NativeIntoFidl,
    fidl::persist,
    std::{collections::BTreeMap, fs, io::Write, path::PathBuf},
};

#[derive(FromArgs, PartialEq, Debug)]
/// Generates a Configuration Value Manifest (cvm) from a given manifest and JSON value file.
#[argh(subcommand, name = "cvm")]
#[cfg(fuchsia_api_level_at_least = "20")]
pub struct GenerateValueManifest {
    /// compiled manifest containing the config declaration
    #[argh(option)]
    cm: PathBuf,

    /// JSON5 file containing a single object with each config field as a top level key in an
    /// object.
    #[argh(option)]
    values: PathBuf,

    /// path to which to write configuration value file
    #[argh(option)]
    output: PathBuf,
}

#[cfg(fuchsia_api_level_at_least = "20")]
impl GenerateValueManifest {
    fn find_config_use(
        name: &str,
        component: &cm_rust::ComponentDecl,
    ) -> Option<cm_rust::UseConfigurationDecl> {
        let bounded_name = cm_types::Name::new(name).unwrap();
        for use_ in &component.uses {
            if let cm_rust::UseDecl::Config(config) = use_ {
                // This destructuring here will force us to update this logic when
                // dictionaries are added.
                let cm_rust::UseConfigurationDecl {
                    source_name: _,
                    source: _,
                    target_name,
                    availability: _,
                    type_: _,
                    default: _,
                } = config;
                if target_name == &bounded_name {
                    return Some(config.clone());
                }
            }
        }
        return None;
    }

    pub fn generate(self) -> Result<(), Error> {
        let component = load_manifest(&self.cm).context("loading component manifest")?;

        // load & parse the json file containing value defs
        let values_raw = fs::read_to_string(self.values).context("reading values JSON")?;
        let values: BTreeMap<String, serde_json::Value> =
            serde_json5::from_str(&values_raw).context("parsing values JSON")?;

        // combine the manifest and provided values
        let capabilities = values
            .iter()
            .map(|(name, value)| {
                let Some(config) = GenerateValueManifest::find_config_use(&name, &component) else {
                    return Err(anyhow!("Could not find use for {}", name));
                };
                let config_value =
                    config_value_file::field::config_value_from_json_value(value, &config.type_)?;
                Ok(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: config.source_name.clone(),
                    value: config_value,
                }))
            })
            .collect::<Result<Vec<cm_rust::CapabilityDecl>, _>>()?;
        let exposes: Vec<_> = capabilities
            .iter()
            .map(|cap| {
                let cm_rust::CapabilityDecl::Config(config) = cap else {
                    panic!("Bad capability somehow");
                };
                cm_rust::ExposeDecl::Config(cm_rust::ExposeConfigurationDecl {
                    source: cm_rust::ExposeSource::Self_,
                    source_name: config.name.clone(),
                    target: cm_rust::ExposeTarget::Parent,
                    target_name: config.name.clone(),
                    availability: cm_rust::Availability::Required,
                })
            })
            .collect();

        let new_component = cm_rust::ComponentDecl {
            capabilities: capabilities,
            exposes: exposes,
            ..Default::default()
        };
        let new_component = new_component.native_into_fidl();
        let encoded_output = persist(&new_component).context("encoding value file")?;

        // write result to value file output
        if let Some(parent) = self.output.parent() {
            // attempt to create all parent directories, ignore failures bc they might already exist
            std::fs::create_dir_all(parent).ok();
        }
        let mut out_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(self.output)
            .context("opening output file")?;
        out_file.write(&encoded_output).context("writing value file to output")?;

        Ok(())
    }
}
