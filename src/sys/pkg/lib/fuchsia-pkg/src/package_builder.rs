// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        path_to_string::PathToStringExt, MetaContents, MetaPackage, MetaSubpackages,
        PackageBuildManifest, PackageManifest, RelativeTo, SubpackageEntry,
    },
    anyhow::{anyhow, bail, ensure, Context, Result},
    camino::Utf8PathBuf,
    fuchsia_merkle::Hash,
    fuchsia_url::RelativePackageUrl,
    std::{
        collections::BTreeMap,
        convert::{TryFrom, TryInto},
        fs::File,
        io::{BufReader, BufWriter, Cursor},
        path::{Path, PathBuf},
    },
    version_history::AbiRevision,
};

/// Paths which will be generated by `PackageBuilder` itself and which should not be manually added.
const RESERVED_PATHS: &[&str] = &[MetaContents::PATH, MetaPackage::PATH, ABI_REVISION_FILE_PATH];
pub const ABI_REVISION_FILE_PATH: &str = "meta/fuchsia.abi/abi-revision";

/// A builder for Fuchsia Packages
pub struct PackageBuilder {
    /// The name of the package being created.
    name: String,

    /// The abi_revision to embed in the package, if any.
    abi_revision: Option<AbiRevision>,

    /// The contents that are to be placed inside the FAR itself, not as
    /// separate blobs.
    far_contents: BTreeMap<String, String>,

    /// The contents that are to be attached to the package as external blobs.
    blobs: BTreeMap<String, String>,

    /// Optional path to serialize the PackageManifest to
    manifest_path: Option<Utf8PathBuf>,

    /// Optionally make the blob 'source_path's relative to the path the
    /// PackageManifest is serialized to.
    blob_sources_relative: RelativeTo,

    /// Optional (possibly different) name to publish the package under.
    /// This changes the name that's placed in the output package manifest.
    published_name: Option<String>,

    /// Optional package repository.
    repository: Option<String>,

    /// Metafile of subpackages.
    subpackages: BTreeMap<RelativePackageUrl, (Hash, PathBuf)>,
}

impl PackageBuilder {
    /// Create a new PackageBuilder.
    pub fn new(name: impl AsRef<str>) -> Self {
        PackageBuilder {
            name: name.as_ref().to_string(),
            abi_revision: None,
            far_contents: BTreeMap::default(),
            blobs: BTreeMap::default(),
            manifest_path: None,
            blob_sources_relative: RelativeTo::default(),
            published_name: None,
            repository: None,
            subpackages: BTreeMap::default(),
        }
    }

    /// Create a PackageBuilder from a PackageBuildManifest.
    pub fn from_package_build_manifest(manifest: &PackageBuildManifest) -> Result<Self> {
        // Read the package name from `meta/package`, or error out if it's missing.
        let meta_package = if let Some(path) = manifest.far_contents().get("meta/package") {
            let f = File::open(path).with_context(|| format!("opening {path}"))?;

            MetaPackage::deserialize(BufReader::new(f))?
        } else {
            return Err(anyhow!("package missing meta/package entry"));
        };

        ensure!(meta_package.variant().is_zero(), "package variant must be zero");

        let mut builder = PackageBuilder::new(meta_package.name());

        // Read the abi revision from `meta/fuchsia.abi/abi-revision`, or error out if it's missing.
        if let Some(path) = manifest.far_contents().get("meta/fuchsia.abi/abi-revision") {
            let abi_revision = std::fs::read(path).with_context(|| format!("reading {path}"))?;
            builder.abi_revision(AbiRevision::try_from(abi_revision.as_slice())?.into());
        }

        for (at_path, file) in manifest.external_contents() {
            builder
                .add_file_as_blob(at_path, file)
                .with_context(|| format!("adding file {at_path} as blob {file}"))?;
        }

        for (at_path, file) in manifest.far_contents() {
            // Ignore files that the package builder will automatically create.
            if at_path == "meta/package" || at_path == "meta/fuchsia.abi/abi-revision" {
                continue;
            }

            builder
                .add_file_to_far(at_path, file)
                .with_context(|| format!("adding file {at_path} to far {file}"))?;
        }

        Ok(builder)
    }

    /// Create a PackageBuilder from an existing manifest. Requires an out directory for temporarily
    /// unpacking `meta.far` contents.
    pub fn from_manifest(
        original_manifest: PackageManifest,
        outdir: impl AsRef<Path>,
    ) -> Result<Self> {
        // parse the existing manifest, copying everything over
        let mut abi_rev = None;
        let mut inner_name = None;
        let mut meta_blobs = BTreeMap::new();
        let mut blob_paths = BTreeMap::new();
        let mut subpackage_names = BTreeMap::new();
        for blob in original_manifest.blobs() {
            if blob.path == PackageManifest::META_FAR_BLOB_PATH {
                let meta_far_contents = std::fs::read(&blob.source_path)
                    .with_context(|| format!("reading {}", blob.source_path))?;
                let PackagedMetaFar { abi_revision, name, meta_contents, .. } =
                    PackagedMetaFar::parse(&meta_far_contents).context("parsing meta/")?;
                abi_rev = Some(abi_revision);
                inner_name = Some(name);
                meta_blobs = meta_contents;
            } else {
                blob_paths.insert(blob.path.clone(), blob.source_path.clone());
            }
        }
        for subpackage in original_manifest.subpackages() {
            subpackage_names.insert(
                subpackage.name.clone(),
                (subpackage.merkle, subpackage.manifest_path.clone()),
            );
        }
        let abi_rev = abi_rev.ok_or_else(|| anyhow!("did not find {}", ABI_REVISION_FILE_PATH))?;
        let inner_name = inner_name.ok_or_else(|| anyhow!("did not find {}", MetaPackage::PATH))?;

        let mut builder = PackageBuilder::new(inner_name);
        builder.abi_revision(*abi_rev);
        builder.published_name(original_manifest.name());
        if let Some(repository) = original_manifest.repository() {
            builder.repository(repository);
        }

        for (path, contents) in meta_blobs {
            builder
                .add_contents_to_far(&path, contents, &outdir)
                .with_context(|| format!("adding {path} to far"))?;
        }

        for (path, source_path) in blob_paths {
            builder
                .add_file_as_blob(&path, &source_path)
                .with_context(|| format!("adding {path}"))?;
        }

        for (name, (merkle, manifest_path)) in subpackage_names {
            builder
                .add_subpackage(
                    &name.parse().context("parsing subpackage name")?,
                    merkle,
                    manifest_path.into(),
                )
                .with_context(|| format!("adding {name}"))?;
        }

        Ok(builder)
    }

    /// Specify a path to write out the json package manifest to.
    pub fn manifest_path(&mut self, manifest_path: impl Into<Utf8PathBuf>) {
        self.manifest_path = Some(manifest_path.into())
    }

    pub fn manifest_blobs_relative_to(&mut self, relative_to: RelativeTo) {
        self.blob_sources_relative = relative_to
    }

    fn validate_ok_to_add_at_path(&self, at_path: impl AsRef<str>) -> Result<()> {
        let at_path = at_path.as_ref();
        if RESERVED_PATHS.contains(&at_path) {
            bail!("Cannot add '{}', it will be created by the PackageBuilder", at_path);
        }

        if self.far_contents.contains_key(at_path) {
            return Err(anyhow!(
                "Package '{}' already contains a file (in the far) at: '{}'",
                self.name,
                at_path
            ));
        }
        if self.blobs.contains_key(at_path) {
            return Err(anyhow!(
                "Package '{}' already contains a file (as a blob) at: '{}'",
                self.name,
                at_path
            ));
        }

        Ok(())
    }

    /// Add a file to the package's far.
    ///
    /// Errors
    ///
    /// Will return an error if the path for the file is already being used.
    /// Will return an error if any special package metadata paths are used.
    pub fn add_file_to_far(
        &mut self,
        at_path: impl AsRef<str>,
        file: impl AsRef<str>,
    ) -> Result<()> {
        let at_path = at_path.as_ref();
        let file = file.as_ref();
        self.validate_ok_to_add_at_path(at_path)?;

        self.far_contents.insert(at_path.to_string(), file.to_string());

        Ok(())
    }

    /// Add a file to the package as a blob itself.
    ///
    /// Errors
    ///
    /// Will return an error if the path for the file is already being used.
    /// Will return an error if any special package metadata paths are used.
    pub fn add_file_as_blob(
        &mut self,
        at_path: impl AsRef<str>,
        file: impl AsRef<str>,
    ) -> Result<()> {
        let at_path = at_path.as_ref();
        let file = file.as_ref();
        self.validate_ok_to_add_at_path(at_path)?;

        self.blobs.insert(at_path.to_string(), file.to_string());

        Ok(())
    }

    /// Write the contents to a file, and add that file as a blob at the given
    /// path within the package.
    pub fn add_contents_as_blob<C: AsRef<[u8]>>(
        &mut self,
        at_path: impl AsRef<str>,
        contents: C,
        gendir: impl AsRef<Path>,
    ) -> Result<()> {
        // Preflight that the file paths are valid before attempting to write.
        self.validate_ok_to_add_at_path(&at_path)?;
        let source_path = Self::write_contents_to_file(gendir, at_path.as_ref(), contents)?;
        self.add_file_as_blob(at_path, source_path.path_to_string()?)
    }

    /// Write the contents to a file, and add that file to the metafar at the
    /// given path within the package.
    pub fn add_contents_to_far<C: AsRef<[u8]>>(
        &mut self,
        at_path: impl AsRef<str>,
        contents: C,
        gendir: impl AsRef<Path>,
    ) -> Result<()> {
        // Preflight that the file paths are valid before attempting to write.
        self.validate_ok_to_add_at_path(&at_path)?;
        let source_path = Self::write_contents_to_file(gendir, at_path.as_ref(), contents)?;
        self.add_file_to_far(at_path, source_path.path_to_string()?)
    }

    /// Helper fn to write the contents to a file, creating the parent dirs as needed when doing so.
    fn write_contents_to_file<C: AsRef<[u8]>>(
        gendir: impl AsRef<Path>,
        file_path: impl AsRef<Path>,
        contents: C,
    ) -> Result<PathBuf> {
        let file_path = gendir.as_ref().join(file_path);
        if let Some(parent_dir) = file_path.parent() {
            std::fs::create_dir_all(parent_dir)
                .context(format!("creating parent directories for {}", file_path.display()))?;
        }
        std::fs::write(&file_path, contents)
            .context(format!("writing contents to file: {}", file_path.display()))?;
        Ok(file_path)
    }

    /// Helper fn to include a subpackage into this package.
    pub fn add_subpackage(
        &mut self,
        url: &RelativePackageUrl,
        package_hash: Hash,
        package_manifest_path: PathBuf,
    ) -> Result<()> {
        if self.subpackages.contains_key(url) {
            return Err(anyhow!("dupicate entry for {:?}", url));
        }
        self.subpackages.insert(url.clone(), (package_hash, package_manifest_path));
        Ok(())
    }

    /// Set the API Level that should be included in the package. This will return an error if there
    /// is no ABI revision that corresponds with this API Level.
    pub fn api_level(&mut self, api_level: u64) -> Result<()> {
        for v in version_history::VERSION_HISTORY {
            if v.api_level == api_level {
                self.abi_revision(v.abi_revision.0);
                return Ok(());
            }
        }

        Err(anyhow!("unknown API level {}", api_level))
    }

    /// Set the ABI Revision that should be included in the package.
    pub fn abi_revision(&mut self, abi_revision: u64) {
        self.abi_revision = Some(AbiRevision::new(abi_revision));
    }

    /// Set a different name for the package to be published by (and to be
    /// included in the generated PackageManifest), than the one embedded in the
    /// package itself.
    pub fn published_name(&mut self, published_name: impl AsRef<str>) {
        self.published_name = Some(published_name.as_ref().into());
    }

    /// Set a repository for the package to be included in the generated PackageManifest.
    pub fn repository(&mut self, repository: impl AsRef<str>) {
        self.repository = Some(repository.as_ref().into());
    }

    /// Read the contents of a file already added to the builder's meta.far.
    pub fn read_contents_from_far(&self, file_path: &str) -> Result<Vec<u8>> {
        if let Some(p) = self.far_contents.get(file_path) {
            std::fs::read(p).with_context(|| format!("reading {p}"))
        } else {
            bail!("couldn't find `{}` in package", file_path);
        }
    }

    /// Build the package, using the specified dir, returning the
    /// PackageManifest.
    ///
    /// If a path for the manifest was specified, the PackageManifest will also
    /// be written to there.
    ///
    /// The `gendir` param is assumed to be a path to folder which is only used
    /// by this package's creation, so this fn does not try to create paths
    /// within it that are unique across different packages.
    pub fn build(
        self,
        gendir: impl AsRef<Path>,
        metafar_path: impl AsRef<Path>,
    ) -> Result<PackageManifest> {
        let gendir = gendir.as_ref();
        let metafar_path = metafar_path.as_ref();

        let PackageBuilder {
            name,
            abi_revision,
            mut far_contents,
            blobs,
            manifest_path,
            blob_sources_relative,
            published_name,
            repository,
            subpackages,
        } = self;

        far_contents.insert(
            MetaPackage::PATH.to_string(),
            create_meta_package_file(gendir, &name)
                .with_context(|| format!("Writing the {} file", MetaPackage::PATH))?,
        );

        let abi_revision = abi_revision.unwrap_or(version_history::LATEST_VERSION.abi_revision);

        let abi_revision_file =
            Self::write_contents_to_file(gendir, ABI_REVISION_FILE_PATH, abi_revision.as_bytes())
                .with_context(|| format!("Writing the {ABI_REVISION_FILE_PATH} file"))?;

        far_contents.insert(
            ABI_REVISION_FILE_PATH.to_string(),
            abi_revision_file.path_to_string().with_context(|| {
                format!("Adding the {ABI_REVISION_FILE_PATH} file to the package")
            })?,
        );

        // Only add the subpackages file if we were configured with any subpackages.
        if !subpackages.is_empty() {
            far_contents.insert(
                MetaSubpackages::PATH.to_string(),
                create_meta_subpackages_file(gendir, subpackages.clone()).with_context(|| {
                    format!("Adding the {} file to the package", MetaSubpackages::PATH)
                })?,
            );
        }

        let package_build_manifest =
            PackageBuildManifest::from_external_and_far_contents(blobs, far_contents)
                .with_context(|| "creating creation manifest".to_string())?;

        let package_manifest = crate::build::build(
            &package_build_manifest,
            metafar_path,
            published_name.unwrap_or(name),
            subpackages
                .into_iter()
                .map(|(name, (merkle, package_manifest_path))| SubpackageEntry {
                    name,
                    merkle,
                    package_manifest_path,
                })
                .collect(),
            repository,
        )
        .with_context(|| format!("building package manifest {}", metafar_path.display()))?;

        Ok(if let Some(manifest_path) = manifest_path {
            if let RelativeTo::File = blob_sources_relative {
                let copy = package_manifest.clone();
                copy.write_with_relative_paths(&manifest_path).with_context(|| {
                    format!(
                        "Failed to create package manifest with relative paths at: {manifest_path}"
                    )
                })?;

                package_manifest
            } else {
                // Write the package manifest to a file.
                let package_manifest_file = std::fs::File::create(&manifest_path)
                    .context(format!("Failed to create package manifest: {manifest_path}"))?;

                serde_json::ser::to_writer(
                    BufWriter::new(package_manifest_file),
                    &package_manifest,
                )
                .with_context(|| format!("writing package manifest to {manifest_path}"))?;

                package_manifest
            }
        } else {
            package_manifest
        })
    }
}

/// Construct a meta/package file in `gendir`.
///
/// Returns the path that the file was created at.
fn create_meta_package_file(gendir: &Path, name: impl Into<String>) -> Result<String> {
    let package_name = name.into();
    let meta_package_path = gendir.join(MetaPackage::PATH);
    if let Some(parent_dir) = meta_package_path.parent() {
        std::fs::create_dir_all(parent_dir)?;
    }

    let file = std::fs::File::create(&meta_package_path)?;
    let meta_package = MetaPackage::from_name(package_name.try_into()?);
    meta_package.serialize(file)?;
    meta_package_path.path_to_string()
}

/// Results of parsing an existing meta.far for repackaging or testing purposes.
struct PackagedMetaFar {
    /// Package's name.
    name: String,

    /// Package's ABI revision.
    abi_revision: AbiRevision,

    /// Map of package paths to blob contents.
    meta_contents: BTreeMap<String, Vec<u8>>,
}

impl PackagedMetaFar {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let mut meta_far =
            fuchsia_archive::Utf8Reader::new(Cursor::new(bytes)).context("reading FAR")?;

        let mut abi_revision = None;
        let mut name = None;
        let mut meta_contents = BTreeMap::new();

        // collect paths separately, we need mutable access to reader for the bytes of each
        let meta_paths = meta_far.list().map(|e| e.path().to_owned()).collect::<Vec<_>>();

        // copy the contents of the meta.far, skipping files that PackageBuilder will write
        for path in meta_paths {
            let contents = meta_far.read_file(&path).with_context(|| format!("reading {path}"))?;

            if path == MetaContents::PATH {
                continue;
            } else if path == MetaPackage::PATH {
                ensure!(name.is_none(), "only one name per package");
                let mp = MetaPackage::deserialize(Cursor::new(&contents))
                    .context("deserializing meta/package")?;
                name = Some(mp.name().to_string());
            } else if path == ABI_REVISION_FILE_PATH {
                ensure!(abi_revision.is_none(), "only one abi revision per package");
                ensure!(contents.len() == 8, "ABI revision must be encoded as 8 bytes");
                abi_revision = Some(AbiRevision::try_from(contents.as_slice()).unwrap());
            } else {
                meta_contents.insert(path, contents);
            }
        }
        let abi_revision =
            abi_revision.ok_or_else(|| anyhow!("did not find {}", ABI_REVISION_FILE_PATH))?;
        let name = name.ok_or_else(|| anyhow!("did not find {}", MetaPackage::PATH))?;

        Ok(Self { name, abi_revision, meta_contents })
    }
}

/// Construct a meta/fuchsia.pkg/subpackages file in `gendir`.
///
/// Returns the path that the file was created at.
fn create_meta_subpackages_file(
    gendir: &Path,
    subpackages: BTreeMap<RelativePackageUrl, (Hash, PathBuf)>,
) -> Result<String> {
    let meta_subpackages_path = gendir.join(MetaSubpackages::PATH);
    if let Some(parent_dir) = meta_subpackages_path.parent() {
        std::fs::create_dir_all(parent_dir)?;
    }

    let meta_subpackages = MetaSubpackages::from_iter(
        subpackages.into_iter().map(|(name, (merkle, _))| (name, merkle)),
    );
    let file = std::fs::File::create(&meta_subpackages_path)?;
    meta_subpackages.serialize(file)?;
    meta_subpackages_path.path_to_string()
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        camino::Utf8Path,
        fuchsia_merkle::MerkleTreeBuilder,
        tempfile::{NamedTempFile, TempDir},
    };

    #[test]
    fn test_create_meta_package_file() {
        let gen_dir = TempDir::new().unwrap();
        let name = "some_test_package";
        let meta_package_path = gen_dir.as_ref().join("meta/package");
        let created_path = create_meta_package_file(gen_dir.path(), name).unwrap();
        assert_eq!(created_path, meta_package_path.path_to_string().unwrap());

        let raw_contents = std::fs::read(meta_package_path).unwrap();
        let meta_package = MetaPackage::deserialize(std::io::Cursor::new(raw_contents)).unwrap();
        assert_eq!(meta_package.name().as_ref(), "some_test_package");
        assert!(meta_package.variant().is_zero());
    }

    #[test]
    fn test_builder() {
        let outdir = TempDir::new().unwrap();
        let metafar_path = outdir.path().join("meta.far");

        // Create a file to write to the package metafar
        let far_source_file_path = NamedTempFile::new_in(&outdir).unwrap();
        std::fs::write(&far_source_file_path, "some data for far").unwrap();

        // Create a file to include as a blob
        let blob_source_file_path = NamedTempFile::new_in(&outdir).unwrap();
        let blob_contents = "some data for blob";
        std::fs::write(&blob_source_file_path, blob_contents).unwrap();

        // Pre-calculate the blob's hash
        let mut merkle_builder = MerkleTreeBuilder::new();
        merkle_builder.write(blob_contents.as_bytes());
        let blob_hash = merkle_builder.finish().root();

        let subpackage_url = "subpackage0".parse::<RelativePackageUrl>().unwrap();
        let subpackage_hash = Hash::from([0; fuchsia_hash::HASH_SIZE]);
        let subpackage_package_manifest_path = "subpackages/package_manifest.json";

        // Create the builder
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder
            .add_file_as_blob("some/blob", blob_source_file_path.path().path_to_string().unwrap())
            .unwrap();
        builder
            .add_file_to_far(
                "meta/some/file",
                far_source_file_path.path().path_to_string().unwrap(),
            )
            .unwrap();
        builder
            .add_subpackage(
                &subpackage_url,
                subpackage_hash,
                subpackage_package_manifest_path.into(),
            )
            .unwrap();

        // Build the package
        let manifest = builder.build(&outdir, &metafar_path).unwrap();

        // Validate the returned manifest
        assert_eq!(manifest.name().as_ref(), "some_pkg_name");

        let (blobs, subpackages) = manifest.into_blobs_and_subpackages();

        // Validate that the blob has the correct hash and contents
        let blob_info = blobs.iter().find(|info| info.path == "some/blob").unwrap().clone();
        assert_eq!(blob_hash, blob_info.merkle);
        assert_eq!(blob_contents, std::fs::read_to_string(blob_info.source_path).unwrap());

        // Validate that the subpackage has the correct hash and manifest path
        let subpackage_info =
            subpackages.iter().find(|info| info.name == "subpackage0").unwrap().clone();
        assert_eq!(subpackage_hash, subpackage_info.merkle);
        assert_eq!(subpackage_package_manifest_path, subpackage_info.manifest_path);

        // Validate that the metafar contains the additional file in meta
        let mut metafar = std::fs::File::open(metafar_path).unwrap();
        let mut far_reader = fuchsia_archive::Utf8Reader::new(&mut metafar).unwrap();
        let far_file_data = far_reader.read_file("meta/some/file").unwrap();
        let far_file_data = std::str::from_utf8(far_file_data.as_slice()).unwrap();
        assert_eq!(far_file_data, "some data for far");

        // Validate that the abi_revision was written correctly
        let abi_revision_data = far_reader.read_file("meta/fuchsia.abi/abi-revision").unwrap();
        let abi_revision_data: [u8; 8] = abi_revision_data.try_into().unwrap();
        let abi_revision = u64::from_le_bytes(abi_revision_data);
        assert_eq!(abi_revision, version_history::LATEST_VERSION.abi_revision.0);
    }

    #[test]
    fn test_from_manifest() {
        let first_outdir = TempDir::new().unwrap();

        // Create an initial package with non-default outputs for generated files
        let inner_name = "some_pkg_name";
        let mut first_builder = PackageBuilder::new(inner_name);
        // Set a different published name
        let published_name = "some_other_pkg_name";
        first_builder.published_name(published_name);
        // Set a non-default ABI revision
        let fake_abi_revision = version_history::LATEST_VERSION.abi_revision.0 + 1;
        first_builder.abi_revision(fake_abi_revision);

        // Create a file to write to the package metafar
        let first_far_source_file_path = NamedTempFile::new_in(&first_outdir).unwrap();
        let first_far_contents = "some data for far";
        std::fs::write(&first_far_source_file_path, first_far_contents).unwrap();
        first_builder
            .add_file_to_far("meta/some/file", first_far_source_file_path.path().to_string_lossy())
            .unwrap();

        // Create a file to include as a blob
        let first_blob_source_file_path = NamedTempFile::new_in(&first_outdir).unwrap();
        let first_blob_contents = "some data for blob";
        std::fs::write(&first_blob_source_file_path, first_blob_contents).unwrap();
        first_builder
            .add_file_as_blob("some/blob", first_blob_source_file_path.path().to_string_lossy())
            .unwrap();

        let first_subpackage_url = "subpackage0".parse::<RelativePackageUrl>().unwrap();
        let first_subpackage_hash = Hash::from([0; fuchsia_hash::HASH_SIZE]);
        let first_subpackage_package_manifest_path = "subpackages/package_manifest.json";

        first_builder
            .add_subpackage(
                &first_subpackage_url,
                first_subpackage_hash,
                first_subpackage_package_manifest_path.into(),
            )
            .unwrap();

        // Build the package
        let first_manifest =
            first_builder.build(&first_outdir, first_outdir.path().join("meta.far")).unwrap();
        assert_eq!(
            first_manifest.blobs().len(),
            2,
            "package should have a meta.far and a single blob"
        );
        assert_eq!(
            first_manifest.subpackages().len(),
            1,
            "package should have a single subpackage"
        );
        let blob_info = first_manifest
            .blobs()
            .iter()
            .find(|blob_info| blob_info.path == PackageManifest::META_FAR_BLOB_PATH)
            .unwrap();
        let mut metafar = std::fs::File::open(&blob_info.source_path).unwrap();
        let far_reader = fuchsia_archive::Utf8Reader::new(&mut metafar).unwrap();
        let first_paths_in_far =
            far_reader.list().map(|e| e.path().to_string()).collect::<Vec<_>>();

        // Re-parse the package into a builder for further modification
        let second_outdir = TempDir::new().unwrap();
        let mut second_builder =
            PackageBuilder::from_manifest(first_manifest.clone(), second_outdir.path()).unwrap();

        // Create another file to write to the package metafar
        let second_far_source_file_path = NamedTempFile::new_in(&second_outdir).unwrap();
        let second_far_contents = "some more data for far";
        std::fs::write(&second_far_source_file_path, second_far_contents).unwrap();
        second_builder
            .add_file_to_far(
                "meta/some/other/file",
                second_far_source_file_path.path().to_string_lossy(),
            )
            .unwrap();

        // Create a file to include as a blob
        let second_blob_source_file_path = NamedTempFile::new_in(&second_outdir).unwrap();
        let second_blob_contents = "some more data for blobs";
        std::fs::write(&second_blob_source_file_path, second_blob_contents).unwrap();
        second_builder
            .add_file_as_blob(
                "some/other/blob",
                second_blob_source_file_path.path().to_string_lossy(),
            )
            .unwrap();

        // Write the package again after we've modified its contents
        let second_metafar_path = second_outdir.path().join("meta.far");
        let second_manifest = second_builder.build(&second_outdir, second_metafar_path).unwrap();
        assert_eq!(first_manifest.name(), second_manifest.name(), "package names must match");
        assert_eq!(
            second_manifest.blobs().len(),
            3,
            "package should have a meta.far and two blobs"
        );
        assert_eq!(
            second_manifest.subpackages().len(),
            1,
            "package should STILL have a single subpackage"
        );

        // Validate the contents of the package after re-writing
        for blob_info in second_manifest.blobs() {
            match &*blob_info.path {
                PackageManifest::META_FAR_BLOB_PATH => {
                    // Validate that the metafar contains the additional file in meta
                    let mut metafar = std::fs::File::open(&blob_info.source_path).unwrap();
                    let mut far_reader = fuchsia_archive::Utf8Reader::new(&mut metafar).unwrap();
                    let paths_in_far =
                        far_reader.list().map(|e| e.path().to_string()).collect::<Vec<_>>();
                    assert_eq!(
                        paths_in_far.len(),
                        first_paths_in_far.len() + 1,
                        "must have the original files and one added one"
                    );

                    for far_path in paths_in_far {
                        let far_bytes = far_reader.read_file(&far_path).unwrap();
                        match &*far_path {
                            MetaContents::PATH => (), // separate tests check this matches blobs
                            MetaPackage::PATH => {
                                let mp = MetaPackage::deserialize(Cursor::new(&far_bytes)).unwrap();
                                assert_eq!(mp.name().as_ref(), inner_name);
                            }
                            MetaSubpackages::PATH => {
                                let ms =
                                    MetaSubpackages::deserialize(Cursor::new(&far_bytes)).unwrap();
                                assert_eq!(ms.subpackages().len(), 1);
                                let (url, hash) = ms.subpackages().iter().next().unwrap();
                                assert_eq!(url, &first_subpackage_url);
                                assert_eq!(hash, &first_subpackage_hash);
                            }
                            ABI_REVISION_FILE_PATH => {
                                assert_eq!(far_bytes, fake_abi_revision.to_le_bytes());
                            }
                            "meta/some/file" => {
                                assert_eq!(far_bytes, first_far_contents.as_bytes());
                            }
                            "meta/some/other/file" => {
                                assert_eq!(far_bytes, second_far_contents.as_bytes());
                            }
                            other => panic!("unrecognized file in meta.far: {other}"),
                        }
                    }
                }
                "some/blob" => {
                    assert_eq!(
                        std::fs::read_to_string(&blob_info.source_path).unwrap(),
                        first_blob_contents,
                    );
                }
                "some/other/blob" => {
                    assert_eq!(
                        std::fs::read_to_string(&blob_info.source_path).unwrap(),
                        second_blob_contents,
                    )
                }
                other => panic!("unrecognized path in blobs `{other}`"),
            }
        }
    }

    #[test]
    fn test_build_rejects_meta_contents() {
        let mut builder = PackageBuilder::new("some_pkg_name");
        assert!(builder.add_file_to_far("meta/contents", "some/src/file").is_err());
        assert!(builder.add_file_as_blob("meta/contents", "some/src/file").is_err());
    }

    #[test]
    fn test_build_rejects_meta_package() {
        let mut builder = PackageBuilder::new("some_pkg_name");
        assert!(builder.add_file_to_far("meta/package", "some/src/file").is_err());
        assert!(builder.add_file_as_blob("meta/package", "some/src/file").is_err());
    }

    #[test]
    fn test_build_rejects_abi_revision() {
        let mut builder = PackageBuilder::new("some_pkg_name");
        assert!(builder.add_file_to_far("meta/fuchsia.abi/abi-revision", "some/src/file").is_err());
        assert!(builder
            .add_file_as_blob("meta/fuchsia.abi/abi-revision", "some/src/file")
            .is_err());
    }

    #[test]
    fn test_builder_rejects_path_in_far_when_existing_path_in_far() {
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.add_file_to_far("some/far/file", "some/src/file").unwrap();
        assert!(builder.add_file_to_far("some/far/file", "some/src/file").is_err());
    }

    #[test]
    fn test_builder_rejects_path_as_blob_when_existing_path_in_far() {
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.add_file_to_far("some/far/file", "some/src/file").unwrap();
        assert!(builder.add_file_as_blob("some/far/file", "some/src/file").is_err());
    }

    #[test]
    fn test_builder_rejects_path_in_far_when_existing_path_as_blob() {
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.add_file_as_blob("some/far/file", "some/src/file").unwrap();
        assert!(builder.add_file_to_far("some/far/file", "some/src/file").is_err());
    }

    #[test]
    fn test_builder_rejects_path_in_blob_when_existing_path_as_blob() {
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.add_file_as_blob("some/far/file", "some/src/file").unwrap();
        assert!(builder.add_file_as_blob("some/far/file", "some/src/file").is_err());
    }

    #[test]
    fn test_builder_makes_file_relative_manifests_when_asked() {
        let tmp = TempDir::new().unwrap();
        let outdir = Utf8Path::from_path(tmp.path()).unwrap();

        let metafar_path = outdir.join("meta.far");
        let manifest_path = outdir.join("package_manifest.json");

        // Create a file to write to the package metafar
        let far_source_file_path = NamedTempFile::new_in(outdir).unwrap();
        std::fs::write(&far_source_file_path, "some data for far").unwrap();

        // Create a file to include as a blob
        let blob_source_file_path = outdir.join("contents/data_file");
        std::fs::create_dir_all(blob_source_file_path.parent().unwrap()).unwrap();
        let blob_contents = "some data for blob";
        std::fs::write(&blob_source_file_path, blob_contents).unwrap();

        // Create the builder
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.add_file_as_blob("some/blob", &blob_source_file_path).unwrap();
        builder
            .add_file_to_far(
                "meta/some/file",
                far_source_file_path.path().path_to_string().unwrap(),
            )
            .unwrap();

        // set it to write a manifest, with file-relative paths.
        builder.manifest_path(manifest_path);
        builder.manifest_blobs_relative_to(RelativeTo::File);

        // Build the package
        let manifest = builder.build(outdir, metafar_path).unwrap();

        // Ensure that the loaded manifest has paths still relative to the working directory, even
        // though serialized paths should be relative to the manifest itself.
        manifest
            .blobs()
            .iter()
            .find(|b| b.source_path == blob_source_file_path)
            .expect("The manifest should have paths relative to the working directory");

        // The written manifest is tested in [crate::package_manifest::host_tests]
    }

    #[test]
    fn test_builder_add_subpackages() {
        let outdir = TempDir::new().unwrap();
        let metafar_path = outdir.path().join("meta.far");

        let mut builder = PackageBuilder::new("some_pkg_name");

        let pkg1_url = "pkg1".parse::<RelativePackageUrl>().unwrap();
        let pkg1_hash = Hash::from([0; fuchsia_hash::HASH_SIZE]);
        let pkg1_package_manifest_path = outdir.path().join("path1/package_manifest.json");

        let pkg2_url = "pkg2".parse::<RelativePackageUrl>().unwrap();
        let pkg2_hash = Hash::from([1; fuchsia_hash::HASH_SIZE]);
        let pkg2_package_manifest_path = outdir.path().join("path2/package_manifest.json");

        builder.add_subpackage(&pkg1_url, pkg1_hash, pkg1_package_manifest_path).unwrap();
        builder.add_subpackage(&pkg2_url, pkg2_hash, pkg2_package_manifest_path).unwrap();

        // Build the package.
        builder.build(&outdir, &metafar_path).unwrap();

        // Validate that the metafar contains the subpackages.
        let mut metafar = std::fs::File::open(metafar_path).unwrap();
        let mut far_reader = fuchsia_archive::Utf8Reader::new(&mut metafar).unwrap();
        let far_file_data = far_reader.read_file(MetaSubpackages::PATH).unwrap();

        assert_eq!(
            MetaSubpackages::deserialize(Cursor::new(&far_file_data)).unwrap(),
            MetaSubpackages::from_iter([(pkg1_url, pkg1_hash), (pkg2_url, pkg2_hash)])
        );
    }

    #[test]
    fn test_builder_rejects_subpackages_collisions() {
        let url = "pkg".parse::<RelativePackageUrl>().unwrap();
        let hash1 = Hash::from([0; fuchsia_hash::HASH_SIZE]);
        let package_manifest_path1 = PathBuf::from("path1/package_manifest.json");
        let hash2 = Hash::from([0; fuchsia_hash::HASH_SIZE]);
        let package_manifest_path2 = PathBuf::from("path2/package_manifest.json");

        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.add_subpackage(&url, hash1, package_manifest_path1).unwrap();
        assert!(builder.add_subpackage(&url, hash2, package_manifest_path2).is_err());
    }
}
