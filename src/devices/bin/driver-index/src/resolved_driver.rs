// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context, Error},
    bind::interpreter::{
        decode_bind_rules::DecodedRules,
        match_bind::{match_bind, DeviceProperties, MatchBindData},
    },
    cm_rust::FidlIntoNative,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_resolution as fresolution,
    fidl_fuchsia_driver_framework as fdf, fidl_fuchsia_driver_index as fdi,
    fidl_fuchsia_pkg_ext::BlobId,
    fuchsia_pkg::{OpenRights, PackageDirectory},
    futures::TryFutureExt,
};

pub const DEFAULT_DEVICE_CATEGORY: &str = "misc";

// Cached drivers don't exist yet so we allow dead code.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum DriverPackageType {
    Boot = 0,
    Base = 1,
    Cached = 2,
    Universe = 3,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedDriver {
    pub component_url: url::Url,
    pub bind_rules: DecodedRules,
    pub bind_bytecode: Vec<u8>,
    pub colocate: bool,
    pub device_categories: Vec<fdf::DeviceCategory>,
    pub fallback: bool,
    pub package_type: DriverPackageType,
    pub package_hash: Option<BlobId>,
    pub is_dfv2: Option<bool>,
    pub disabled: bool,
}

impl std::fmt::Display for ResolvedDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.component_url)
    }
}

impl ResolvedDriver {
    pub async fn resolve(
        component_url: url::Url,
        resolver: &fresolution::ResolverProxy,
        package_type: DriverPackageType,
    ) -> Result<ResolvedDriver, fuchsia_zircon::Status> {
        let res = resolver
            .resolve(component_url.as_str())
            .map_err(|e| {
                tracing::warn!("Resolve call failed: {}", e);
                fuchsia_zircon::Status::INTERNAL
            })
            .await?;
        let resolved_component = res.map_err(|e| {
            tracing::warn!("{:?}: Failed to resolve driver: {:?}", component_url, e);
            map_resolve_err_to_zx_status(e)
        })?;
        let decl_data = resolved_component.decl.ok_or_else(|| {
            tracing::warn!("{}: Missing component decl", component_url);
            fuchsia_zircon::Status::NOT_FOUND
        })?;
        let decl_bytes = mem_util::bytes_from_data(&decl_data).map_err(|e| {
            tracing::warn!("{}: Failed to parse decl data into bytes: {}", component_url, e);
            fuchsia_zircon::Status::IO
        })?;
        let decl: fdecl::Component = fidl::unpersist(&decl_bytes[..]).map_err(|e| {
            tracing::warn!("{}: Failed to parse component decl: {}", component_url, e);
            fuchsia_zircon::Status::INVALID_ARGS
        })?;
        let package = resolved_component.package.and_then(|p| p.directory).ok_or_else(|| {
            tracing::warn!("{}: Missing package directory", component_url);
            fuchsia_zircon::Status::NOT_FOUND
        })?;
        let proxy = package.into_proxy().map_err(|e| {
            tracing::warn!("Failed to create package proxy: {:?}", e);
            fuchsia_zircon::Status::INTERNAL
        })?;
        let package_dir = PackageDirectory::from_proxy(proxy);
        let package_hash = package_dir.merkle_root().await.map_err(|e| {
            tracing::warn!("Failed to read package directory's hash: {}", e);
            fuchsia_zircon::Status::INTERNAL
        })?;
        load_driver(
            component_url,
            decl,
            package_dir,
            package_type,
            Some(BlobId::from(package_hash)),
        )
        .map_err(|e| {
            tracing::warn!("Could not load driver: {}", e);
            fuchsia_zircon::Status::INTERNAL
        })
        .await
    }

    pub fn matches(
        &self,
        properties: &DeviceProperties,
    ) -> Result<Option<fdi::MatchDriverResult>, bind::interpreter::common::BytecodeError> {
        if let DecodedRules::Normal(rules) = &self.bind_rules {
            let matches = match_bind(
                MatchBindData {
                    symbol_table: &rules.symbol_table,
                    instructions: &rules.instructions,
                },
                properties,
            )
            .map_err(|e| {
                tracing::error!("Driver {}: bind error: {}", self, e);
                e
            })?;

            if !matches {
                return Ok(None);
            }

            return Ok(Some(fdi::MatchDriverResult::Driver(self.create_driver_info(false))));
        }

        Ok(None)
    }

    pub fn create_driver_info(&self, full: bool) -> fdf::DriverInfo {
        fdf::DriverInfo {
            url: Some(self.component_url.clone().to_string()),
            colocate: Some(self.colocate),
            package_type: fdf::DriverPackageType::from_primitive(self.package_type as u8),
            is_fallback: Some(self.fallback),
            device_categories: Some(self.device_categories.clone()),
            bind_rules_bytecode: if full { Some(self.bind_bytecode.clone()) } else { None },
            driver_framework_version: match self.is_dfv2 {
                Some(true) => Some(2),
                Some(false) => Some(1),
                None => None,
            },
            is_disabled: Some(self.disabled),
            ..Default::default()
        }
    }
}

// Load the driver information from its resolved component's decl and package.
pub async fn load_driver(
    component_url: url::Url,
    component: fdecl::Component,
    package_dir: PackageDirectory,
    package_type: DriverPackageType,
    package_hash: Option<BlobId>,
) -> Result<ResolvedDriver, Error> {
    let component: cm_rust::ComponentDecl = component.fidl_into_native();

    let bind_path = get_rules_string_value(&component, "bind")
        .ok_or(anyhow!("{}: Missing bind path", component_url))?;
    let bind = package_dir.open_file(&bind_path, OpenRights::Read).await.with_context(|| {
        format!("{}: Failed to open bind file '{}'", component_url.as_str(), bind_path)
    })?;
    let bind = fuchsia_fs::file::read(&bind).await.with_context(|| {
        format!("{}: Failed to read bind file '{}'", component_url.as_str(), bind_path)
    })?;
    let bind_rules = DecodedRules::new(bind.clone())
        .with_context(|| format!("{}: Failed to parse bind", component_url.as_str()))?;

    let v1_driver_path = get_rules_string_value(&component, "compat");
    let fallback = get_rules_string_value(&component, "fallback");
    let fallback = match fallback {
        Some(s) => s == "true",
        None => false,
    };
    let colocate = get_rules_string_value(&component, "colocate");
    let colocate = match colocate {
        Some(s) => s == "true",
        None => false,
    };

    let device_categories = get_rules_device_categories_vec(&component).unwrap();
    let is_dfv2 = Some(v1_driver_path.is_none());

    Ok(ResolvedDriver {
        component_url: component_url,
        bind_rules: bind_rules,
        bind_bytecode: bind,
        colocate: colocate,
        device_categories: device_categories,
        fallback: fallback,
        package_type,
        package_hash,
        is_dfv2,
        disabled: false,
    })
}

fn get_rules_string_value(component: &cm_rust::ComponentDecl, key: &str) -> Option<String> {
    for entry in component.program.as_ref()?.info.entries.as_ref()? {
        if entry.key == key {
            match entry.value.as_ref()?.as_ref() {
                fidl_fuchsia_data::DictionaryValue::Str(s) => {
                    return Some(s.to_string());
                }
                _ => {
                    return None;
                }
            }
        }
    }
    return None;
}

fn get_rules_device_categories_vec(
    component: &cm_rust::ComponentDecl,
) -> Option<Vec<fdf::DeviceCategory>> {
    let default_val = Some(vec![fdf::DeviceCategory {
        category: Some(DEFAULT_DEVICE_CATEGORY.to_string()),
        subcategory: None,
        ..Default::default()
    }]);

    for entry in component.program.as_ref()?.info.entries.as_ref()? {
        if entry.key == "device_categories" {
            match entry.value.as_ref()?.as_ref() {
                fidl_fuchsia_data::DictionaryValue::ObjVec(dictionaries) => {
                    return Some(get_device_categories_from_component_data(dictionaries));
                }
                _ => {
                    return default_val;
                }
            }
        }
    }

    default_val
}

pub fn get_device_categories_from_component_data(
    dictionaries: &Vec<fidl_fuchsia_data::Dictionary>,
) -> Vec<fdf::DeviceCategory> {
    let mut categories = Vec::new();
    for dictionary in dictionaries {
        if let Some(entries) = &dictionary.entries {
            let category = get_dictionary_string_value(entries, "category");
            let subcategory = get_dictionary_string_value(entries, "subcategory");
            categories.push(fdf::DeviceCategory { category, subcategory, ..Default::default() });
        }
    }
    categories
}

fn get_dictionary_string_value(
    entries: &Vec<fidl_fuchsia_data::DictionaryEntry>,
    key: &str,
) -> Option<String> {
    for entry in entries {
        if entry.key == key {
            match entry.value.as_ref()?.as_ref() {
                fidl_fuchsia_data::DictionaryValue::Str(s) => {
                    return Some(s.clone());
                }
                _ => {
                    return None;
                }
            }
        }
    }

    None
}

fn map_resolve_err_to_zx_status(
    resolve_error: fresolution::ResolverError,
) -> fuchsia_zircon::Status {
    match resolve_error {
        fresolution::ResolverError::Internal => fuchsia_zircon::Status::INTERNAL,
        fresolution::ResolverError::NoSpace => fuchsia_zircon::Status::NO_SPACE,
        fresolution::ResolverError::Io => fuchsia_zircon::Status::IO,
        fresolution::ResolverError::NotSupported => fuchsia_zircon::Status::NOT_SUPPORTED,
        fresolution::ResolverError::ResourceUnavailable => fuchsia_zircon::Status::UNAVAILABLE,

        fresolution::ResolverError::PackageNotFound
        | fresolution::ResolverError::ManifestNotFound
        | fresolution::ResolverError::ConfigValuesNotFound
        | fresolution::ResolverError::AbiRevisionNotFound => fuchsia_zircon::Status::NOT_FOUND,

        fresolution::ResolverError::InvalidArgs
        | fresolution::ResolverError::InvalidAbiRevision
        | fresolution::ResolverError::InvalidManifest => fuchsia_zircon::Status::INVALID_ARGS,
    }
}
