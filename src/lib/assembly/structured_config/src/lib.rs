// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for working with structured configuration during the product assembly process.

use anyhow::{format_err, Context};
use assembly_named_file_map::NamedFileMap;
use assembly_util::{BootfsComponentForRepackage, BootfsDestination, FileEntry};
use assembly_validate_util::PkgNamespace;
use camino::Utf8PathBuf;
use cm_rust::{FidlIntoNative, NativeIntoFidl};
use fidl::{unpersist, Persistable};
use fuchsia_pkg::{PackageBuilder, PackageManifest, RelativeTo};
use std::{collections::BTreeMap, fmt::Debug};

pub struct BootfsRepackager<'f> {
    files: &'f mut NamedFileMap<BootfsDestination>,
    outdir: Utf8PathBuf,
}

impl<'f> BootfsRepackager<'f> {
    pub fn new(
        files: &'f mut NamedFileMap<BootfsDestination>,
        outdir: impl Into<Utf8PathBuf>,
    ) -> Self {
        Self { files, outdir: outdir.into() }
    }

    /// Check if the package we're building has a particular component manifest.
    pub fn has_component(&self, manifest_path: BootfsComponentForRepackage) -> bool {
        self.read_contents(manifest_path).is_ok()
    }

    /// Apply structured configuration values to this package, failing if the package already has
    /// a configuration value file for the component.
    pub fn set_component_config(
        &mut self,
        component: BootfsComponentForRepackage,
        values: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), RepackageError> {
        let manifest_bytes =
            self.read_contents(component.clone()).map_err(RepackageError::ReadManifest)?;
        let (config_bytes, _) = config_for_manifest(manifest_bytes, values)?;

        let path: BootfsDestination = component.config();
        let out_path = self.outdir.join("bootfs/repackaging/components").join(path.to_string());
        std::fs::create_dir_all(out_path.parent().unwrap())
            .context("creating outdir")
            .map_err(RepackageError::WriteValueFile)?;
        std::fs::write(&out_path, config_bytes)
            .with_context(|| format!("writing {}", out_path))
            .map_err(RepackageError::WriteValueFile)?;
        self.files
            .add_entry(FileEntry { source: out_path, destination: path })
            .map_err(RepackageError::WriteValueFile)
    }

    fn read_contents(&self, component: BootfsComponentForRepackage) -> anyhow::Result<Vec<u8>> {
        let path = component.manifest();
        let src_path = &self.files.get(&path).ok_or_else(|| format_err!("missing {path}"))?.source;
        Ok(std::fs::read(src_path).with_context(|| format!("reading {}", src_path))?)
    }
}

pub struct Repackager {
    builder: PackageBuilder,
    outdir: Utf8PathBuf,
}

impl Repackager {
    /// Read an existing package manifest for modification. The new manifest will be written to
    /// `outdir` along with any needed temporary files.
    pub fn new(
        original_manifest: PackageManifest,
        outdir: impl Into<Utf8PathBuf>,
    ) -> Result<Self, RepackageError> {
        let outdir = outdir.into();
        let builder = PackageBuilder::from_manifest(original_manifest, &outdir)
            .map_err(RepackageError::CreatePackageBuilder)?;
        Ok(Self { builder, outdir })
    }

    /// Apply structured configuration values to this package, failing if the package already has
    /// a configuration value file for the component.
    pub fn set_component_config(
        &mut self,
        manifest_path: &str,
        values: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), RepackageError> {
        let manifest_bytes = self
            .builder
            .read_contents_from_far(manifest_path)
            .map_err(RepackageError::ReadManifest)?;
        let (config_bytes, path) = config_for_manifest(manifest_bytes, values)?;
        let path = path.expect("Tried to set config for non CVF config path");
        self.builder
            .add_contents_to_far(path, config_bytes, self.outdir.as_std_path())
            .map_err(RepackageError::WriteValueFile)?;
        Ok(())
    }

    /// Build the modified package, returning a path to its new manifest.
    pub fn build(self) -> Result<Utf8PathBuf, RepackageError> {
        let Self { mut builder, outdir } = self;
        let manifest_path = outdir.join("package_manifest.json");
        builder.manifest_path(&manifest_path);
        builder.manifest_blobs_relative_to(RelativeTo::File);
        builder.build(&outdir, outdir.join("meta.far")).map_err(RepackageError::BuildPackage)?;
        Ok(manifest_path)
    }
}

/// Parse the component manifest and return the bytes for the new structured config value file.
fn config_for_manifest(
    manifest_bytes: Vec<u8>,
    values: BTreeMap<String, serde_json::Value>,
) -> Result<(Vec<u8>, Option<String>), RepackageError> {
    let manifest: cm_rust::ComponentDecl =
        read_and_validate_fidl(&manifest_bytes, cm_fidl_validator::validate)
            .map_err(RepackageError::ParseManifest)?;

    let Some(config_decl) = manifest.config else {
        return Err(RepackageError::MissingConfigDecl);
    };

    // create a value file
    let config_values =
        config_value_file::populate_value_file(&config_decl, values)?.native_into_fidl();

    let config_bytes = fidl::persist(&config_values).map_err(RepackageError::EncodeConfig)?;
    let path = match &config_decl.value_source {
        cm_rust::ConfigValueSource::PackagePath(path) => Some(path.to_string()),
        cm_rust::ConfigValueSource::Capabilities(_) => None,
    };
    Ok((config_bytes, path))
}

#[derive(Debug, thiserror::Error)]
pub enum RepackageError {
    #[error("Couldn't read package manifest for modification.")]
    CreatePackageBuilder(#[source] anyhow::Error),
    #[error("Couldn't read component manifest.")]
    ReadManifest(#[source] anyhow::Error),
    #[error("Couldn't parse manifest.")]
    ParseManifest(#[source] PersistentFidlError),
    #[error("No config decl found in manifest.")]
    MissingConfigDecl,
    #[error("Couldn't compile a configuration value file.")]
    ValueFileCreation(
        #[source]
        #[from]
        config_value_file::FileError,
    ),
    #[error("Couldn't encode config values as persistent FIDL.")]
    EncodeConfig(#[source] fidl::Error),
    #[error("Couldn't write the config value file to the modified package.")]
    WriteValueFile(#[source] anyhow::Error),
    #[error("Couldn't build the modified package.")]
    BuildPackage(#[source] anyhow::Error),
}

/// The list of runners currently supported by structured config.
static SUPPORTED_RUNNERS: &[&str] =
    &["driver", "elf", "elf_test_runner", "elf_test_create_raw_processes_ambient_exec_runner"];

/// Validate a component manifest given access to the contents of its `/pkg` directory.
///
/// Ensures that if a component has a `config` stanza it can be paired with the specified
/// value file and that together they can produce a valid configuration for the component.
///
/// Also ensures that the component is using a runner which supports structured config.
pub fn validate_component(
    manifest_path: &str,
    reader: &mut impl PkgNamespace,
) -> Result<(), ValidationError> {
    // get the manifest and validate it
    let manifest_bytes =
        reader.read_file(manifest_path).map_err(ValidationError::ManifestMissing)?;
    let manifest: cm_rust::ComponentDecl =
        read_and_validate_fidl(&manifest_bytes, cm_fidl_validator::validate)
            .map_err(ValidationError::ParseManifest)?;

    let Some(config_decl) = manifest.config else {
        return Ok(());
    };

    // make sure the component has a runner that will deliver config before finding values
    let runner = manifest
        .program
        .as_ref()
        .ok_or(ValidationError::ProgramMissing)?
        .runner
        .as_ref()
        .ok_or(ValidationError::RunnerMissing)?
        .as_str();
    if !SUPPORTED_RUNNERS.contains(&runner) {
        return Err(ValidationError::UnsupportedRunner(runner.to_owned()));
    }

    let path = match &config_decl.value_source {
        cm_rust::ConfigValueSource::PackagePath(path) => path,
        cm_rust::ConfigValueSource::Capabilities(_) => return Ok(()),
    };
    let config_bytes = reader.read_file(&path).map_err(ValidationError::ConfigValuesMissing)?;

    // read and validate the config values
    let config_values: cm_rust::ConfigValuesData =
        read_and_validate_fidl(&config_bytes, cm_fidl_validator::validate_values_data)
            .map_err(ValidationError::ParseConfig)?;

    // we have config, make sure it's compatible with the manifest which references it
    config_encoder::ConfigFields::resolve(&config_decl, config_values, None)
        .map_err(ValidationError::ResolveConfig)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("Couldn't read manifest.")]
    ManifestMissing(#[source] assembly_validate_util::ReadError),
    #[error("Couldn't parse manifest.")]
    ParseManifest(#[source] PersistentFidlError),
    #[error("Couldn't find component's config values in package.")]
    ConfigValuesMissing(#[source] assembly_validate_util::ReadError),
    #[error("Couldn't parse config values.")]
    ParseConfig(#[source] PersistentFidlError),
    #[error("Couldn't resolve config.")]
    ResolveConfig(#[source] config_encoder::ResolutionError),
    #[error("Component manifest does not specify `program`.")]
    ProgramMissing,
    #[error("Component manifest does not specify `program.runner`.")]
    RunnerMissing,
    #[error("{:?} is not a supported runner. (allowed: {:?})", _0, SUPPORTED_RUNNERS)]
    UnsupportedRunner(String),
}

/// Parse bytes as a FIDL type, passing it to a `cm_fidl_validator` function before converting
/// it to the desired `cm_rust` type.
fn read_and_validate_fidl<Raw, Output, VF>(
    bytes: &[u8],
    validate: VF,
) -> Result<Output, PersistentFidlError>
where
    Raw: FidlIntoNative<Output> + Persistable,
    VF: Fn(&Raw) -> Result<(), cm_fidl_validator::error::ErrorList>,
{
    let raw: Raw = unpersist(&bytes).map_err(PersistentFidlError::Decode)?;
    validate(&raw).map_err(PersistentFidlError::Validate)?;
    Ok(raw.fidl_into_native())
}

#[derive(Debug, thiserror::Error)]
pub enum PersistentFidlError {
    #[error("Couldn't decode bytes.")]
    Decode(#[source] fidl::Error),
    #[error("Couldn't validate raw FIDL type.")]
    Validate(#[source] cm_fidl_validator::error::ErrorList),
}
