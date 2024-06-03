// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Result},
    async_trait::async_trait,
    camino::Utf8Path,
    chrono::{offset::Utc, DateTime},
    errors::ffx_bail,
    ffx_config::{environment::EnvironmentKind, EnvironmentContext},
    ffx_repository_packages_args::{
        ExtractArchiveSubCommand, ListSubCommand, PackagesCommand, PackagesSubCommand,
        ShowSubCommand,
    },
    fho::{FfxMain, FfxTool, MachineWriter, ToolIO},
    fuchsia_hash::Hash,
    fuchsia_hyper::new_https_client,
    fuchsia_pkg::PackageArchiveBuilder,
    fuchsia_repo::{
        repo_client::{PackageEntry, RepoClient},
        repository::{PmRepository, RepoProvider},
    },
    humansize::{file_size_opts, FileSize},
    pkg::repo::repo_spec_to_backend,
    prettytable::{cell, format::TableFormat, row, Row, Table},
    std::{
        fs::File,
        io::{BufWriter, Cursor, Read},
        time::{Duration, SystemTime},
    },
};

const MAX_HASH: usize = 11;
const REPO_PATH_RELATIVE_TO_BUILD_DIR: &str = "amber-files";

type PackagesWriter = MachineWriter<Vec<PackagesOutput>>;

#[derive(FfxTool)]
pub struct PackagesTool {
    #[command]
    cmd: PackagesCommand,
    context: EnvironmentContext,
}

fho::embedded_plugin!(PackagesTool);

#[async_trait(?Send)]
impl FfxMain for PackagesTool {
    type Writer = PackagesWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        packages_impl(self.cmd, self.context, writer).await?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
#[serde(untagged)]
pub enum PackagesOutput {
    Show(PackageEntry),
    List(RepositoryPackage),
}

#[derive(PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct RepositoryPackage {
    name: String,
    hash: String,
    size: Option<u64>,
    modified: Option<u64>,
    entries: Option<Vec<PackageEntry>>,
}

async fn packages_impl(
    cmd: PackagesCommand,
    context: EnvironmentContext,
    mut writer: PackagesWriter,
) -> Result<()> {
    match cmd.subcommand {
        PackagesSubCommand::List(subcmd) => list_impl(subcmd, context, None, &mut writer).await,
        PackagesSubCommand::Show(subcmd) => show_impl(subcmd, context, None, &mut writer).await,
        PackagesSubCommand::ExtractArchive(subcmd) => extract_archive_impl(subcmd, context).await,
    }
}

async fn show_impl(
    cmd: ShowSubCommand,
    context: EnvironmentContext,
    table_format: Option<TableFormat>,
    writer: &mut PackagesWriter,
) -> Result<()> {
    let repo = connect_to_repo(cmd.repository.clone(), context).await?;

    let Some(mut blobs) = repo
        .show_package(&cmd.package, cmd.include_subpackages)
        .await
        .with_context(|| format!("showing package {}", cmd.package))?
    else {
        ffx_bail!("repository does not contain package {}", cmd.package)
    };

    blobs.sort();

    if writer.is_machine() {
        let blobs = Vec::from_iter(blobs.into_iter().map(PackagesOutput::Show));
        writer.machine(&blobs).context("writing machine representation of blobs")?;
    } else {
        print_blob_table(&cmd, &blobs, table_format, writer).context("printing repository table")?
    }

    Ok(())
}

fn print_blob_table(
    cmd: &ShowSubCommand,
    blobs: &[PackageEntry],
    table_format: Option<TableFormat>,
    writer: &mut PackagesWriter,
) -> Result<()> {
    let mut table = Table::new();
    let mut header = row!("NAME", "SIZE", "HASH", "MODIFIED");
    if cmd.include_subpackages {
        header.insert_cell(1, cell!("SUBPACKAGE"));
    }
    table.set_titles(header);
    if let Some(fmt) = table_format {
        table.set_format(fmt);
    }

    let mut rows = vec![];

    for blob in blobs {
        let mut row = row!(
            blob.path,
            blob.size
                .map(|s| s
                    .file_size(file_size_opts::CONVENTIONAL)
                    .unwrap_or_else(|_| format!("{}b", s)))
                .unwrap_or_else(|| "<unknown>".to_string()),
            format_hash(&blob.hash.map(|hash| hash.to_string()), cmd.full_hash),
            blob.modified
                .and_then(|m| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(m)))
                .map(|m| DateTime::<Utc>::from(m).to_rfc2822())
                .unwrap_or_else(String::new)
        );
        if cmd.include_subpackages {
            row.insert_cell(1, cell!(blob.subpackage.as_deref().unwrap_or("<root>")));
        }
        rows.push(row);
    }

    for row in rows.into_iter() {
        table.add_row(row);
    }
    table.print(writer)?;

    Ok(())
}

async fn list_impl(
    cmd: ListSubCommand,
    context: EnvironmentContext,
    table_format: Option<TableFormat>,
    writer: &mut PackagesWriter,
) -> Result<()> {
    let repo = connect_to_repo(cmd.repository.clone(), context).await?;

    let mut packages = vec![];
    for package in repo.list_packages().await? {
        let mut package = RepositoryPackage {
            name: package.name,
            hash: package.hash.to_string(),
            size: package.size,
            modified: package.modified,
            entries: None,
        };

        if cmd.include_components {
            package.entries = repo.show_package(&package.name, false).await?.map(|entries| {
                entries
                    .into_iter()
                    .filter(|entry| entry.subpackage.is_none())
                    .filter(|entry| entry.path.ends_with(".cm"))
                    .collect()
            });
        };

        packages.push(package);
    }

    packages.sort();

    if writer.is_machine() {
        let packages = Vec::from_iter(packages.into_iter().map(PackagesOutput::List));
        writer.machine(&packages).context("writing machine representation of packages")?;
    } else {
        print_package_table(&cmd, packages, table_format, writer)
            .context("printing repository table")?
    }

    Ok(())
}

fn print_package_table(
    cmd: &ListSubCommand,
    packages: Vec<RepositoryPackage>,
    table_format: Option<TableFormat>,
    writer: &mut PackagesWriter,
) -> Result<()> {
    let mut table = Table::new();
    let mut header = row!("NAME", "SIZE", "HASH", "MODIFIED");
    if cmd.include_components {
        header.add_cell(cell!("COMPONENTS"));
    }
    table.set_titles(header);
    if let Some(fmt) = table_format {
        table.set_format(fmt);
    }

    let mut rows = vec![];

    for pkg in packages {
        let mut row = row!(
            pkg.name,
            pkg.size
                .map(|s| s
                    .file_size(file_size_opts::CONVENTIONAL)
                    .unwrap_or_else(|_| format!("{}b", s)))
                .unwrap_or_else(|| "<unknown>".to_string()),
            format_hash(&Some(pkg.hash), cmd.full_hash),
            pkg.modified
                .and_then(|m| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(m)))
                .map(to_rfc2822)
                .unwrap_or_else(String::new)
        );
        if cmd.include_components {
            if let Some(entries) = pkg.entries {
                row.add_cell(cell!(entries
                    .into_iter()
                    .map(|entry| entry.path)
                    .collect::<Vec<_>>()
                    .join("\n")));
            }
        }
        rows.push(row);
    }

    rows.sort_by_key(|r: &Row| r.get_cell(0).unwrap().get_content());
    for row in rows.into_iter() {
        table.add_row(row);
    }
    table.print(writer)?;

    Ok(())
}

fn format_hash(hash_value: &Option<String>, full: bool) -> String {
    if let Some(value) = hash_value {
        if full {
            value.to_string()
        } else {
            value[..MAX_HASH].to_string()
        }
    } else {
        "<unknown>".to_string()
    }
}

fn to_rfc2822(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc2822()
}

async fn connect_to_repo(
    repo_name: Option<String>,
    context: EnvironmentContext,
) -> Result<RepoClient<Box<dyn RepoProvider>>> {
    // If we specified a repository on the CLI, try to look up its repository spec.
    let repo_spec = if let Some(repo_name) = repo_name {
        if let Some(repo_spec) = pkg::config::get_repository(&repo_name)
            .await
            .with_context(|| format!("Finding repo spec for {repo_name}"))?
        {
            Some(repo_spec)
        } else {
            ffx_bail!("No configuration found for {repo_name}")
        }
    } else if let Some(repo_name) = pkg::config::get_default_repository().await? {
        // Otherwise, check if the default repository exists. Don't error out if
        // it doesn't.
        pkg::config::get_repository(&repo_name)
            .await
            .with_context(|| format!("Finding repo spec for {repo_name}"))?
    } else {
        None
    };

    // If we have a repository spec, create a backend for it. Otherwise we'll
    // see if we are in the fuchsia repository and try to use it.
    let backend = if let Some(repo_spec) = repo_spec {
        repo_spec_to_backend(&repo_spec, new_https_client())
            .with_context(|| format!("Creating a repo backend"))?
    } else if let EnvironmentKind::InTree { build_dir: Some(build_dir), .. } = context.env_kind() {
        let build_dir = Utf8Path::from_path(&build_dir)
            .with_context(|| format!("converting repo path to UTF-8 {:?}", build_dir))?;

        let backend =
            PmRepository::builder(build_dir.join(REPO_PATH_RELATIVE_TO_BUILD_DIR)).build();

        Box::new(backend)
    } else {
        ffx_bail!("No configuration found for the repository")
    };

    let mut repo = RepoClient::from_trusted_remote(backend)
        .await
        .with_context(|| format!("Connecting to repository"))?;

    // Make sure the repository is up to date.
    repo.update().await.with_context(|| format!("Updating repository"))?;

    Ok(repo)
}

async fn extract_archive_impl(
    cmd: ExtractArchiveSubCommand,
    context: EnvironmentContext,
) -> Result<()> {
    let repo = connect_to_repo(cmd.repository.clone(), context).await?;

    let Some(entries) = repo
        .show_package(&cmd.package, true)
        .await
        .with_context(|| format!("showing package {}", cmd.package))?
    else {
        ffx_bail!("repository does not contain package {}", cmd.package)
    };

    let entry_is_meta_far =
        |entry: &&PackageEntry| entry.path == "meta.far" && entry.subpackage.is_none();

    let Some(meta_far_entry) = entries.iter().find(entry_is_meta_far) else {
        ffx_bail!("no meta.far entry in package {}", cmd.package)
    };

    let fetch_blob = |hash: Hash| {
        let repo = &repo;
        async move {
            let blob = repo
                .read_blob_decompressed(&hash)
                .await
                .with_context(|| format!("reading blob {hash}"))?;
            Ok::<(u64, Box<dyn Read>), anyhow::Error>((
                blob.len().try_into()?,
                Box::new(Cursor::new(blob)),
            ))
        }
    };

    let (size, content) =
        fetch_blob(meta_far_entry.hash.expect("meta.far entry should have hash")).await?;
    let mut archive_builder = PackageArchiveBuilder::with_meta_far(size, content);

    for entry in entries {
        if entry_is_meta_far(&&entry) {
            continue;
        }
        let Some(hash) = entry.hash else {
            // skip entries inside meta.far
            continue;
        };

        let (size, content) = fetch_blob(hash).await?;
        archive_builder.add_blob(hash, size, content);
    }

    let out_file = File::create(&cmd.out)
        .with_context(|| format!("creating package archive file {}", cmd.out.display()))?;
    archive_builder
        .build(BufWriter::new(out_file))
        .with_context(|| format!("writing package archive file {}", cmd.out.display()))?;
    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*,
        ffx_config::ConfigLevel,
        ffx_package_archive_utils::{read_file_entries, ArchiveEntry, FarArchiveReader},
        fho::TestBuffers,
        fuchsia_async as fasync,
        fuchsia_repo::test_utils,
        pretty_assertions::assert_eq,
        prettytable::format::FormatBuilder,
        std::path::Path,
    };

    const PKG1_HASH: &str = "2881455493b5870aaea36537d70a2adc635f516ac2092598f4b6056dabc6b25d";
    const PKG2_HASH: &str = "050907f009ff634f9aa57bff541fb9e9c2c62b587c23578e77637cda3bd69458";

    const PKG1_BIN_HASH: &str = "72e1e7a504f32edf4f23e7e8a3542c1d77d12541142261cfe272decfa75f542d";
    const PKG1_LIB_HASH: &str = "8a8a5f07f935a4e8e1fd1a1eda39da09bb2438ec0adfb149679ddd6e7e1fbb4f";

    async fn setup_repo(path: &Path) -> ffx_config::TestEnv {
        test_utils::make_pm_repo_dir(path).await;

        let env = ffx_config::test_init().await.unwrap();
        env.context
            .query("repository.repositories.devhost.path")
            .level(Some(ConfigLevel::User))
            .set(path.to_str().unwrap().into())
            .await
            .unwrap();

        env.context
            .query("repository.repositories.devhost.type")
            .level(Some(ConfigLevel::User))
            .set("pm".into())
            .await
            .unwrap();

        env
    }

    async fn run_impl(
        cmd: ListSubCommand,
        context: EnvironmentContext,
        writer: &mut PackagesWriter,
    ) {
        timeout::timeout(
            std::time::Duration::from_millis(1000),
            list_impl(cmd, context, Some(FormatBuilder::new().padding(1, 1).build()), writer),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn run_impl_for_show_command(
        cmd: ShowSubCommand,
        context: EnvironmentContext,
        writer: &mut PackagesWriter,
    ) {
        timeout::timeout(
            std::time::Duration::from_millis(1000),
            show_impl(cmd, context, Some(FormatBuilder::new().padding(1, 1).build()), writer),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_package_list_truncated_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let env = setup_repo(tmp.path()).await;

        let test_buffers = TestBuffers::default();
        let mut writer = PackagesWriter::new_test(None, &test_buffers);

        run_impl(
            ListSubCommand {
                repository: Some("devhost".to_string()),
                full_hash: false,
                include_components: false,
            },
            env.context.clone(),
            &mut writer,
        )
        .await;

        let blobs_path = tmp.path().join("repository/blobs/1");

        let pkg1_hash = &PKG1_HASH[..MAX_HASH];
        let pkg1_path = blobs_path.join(PKG1_HASH);
        let pkg1_modified = to_rfc2822(std::fs::metadata(pkg1_path).unwrap().modified().unwrap());

        let pkg2_hash = &PKG2_HASH[..MAX_HASH];
        let pkg2_path = blobs_path.join(PKG2_HASH);
        let pkg2_modified = to_rfc2822(std::fs::metadata(pkg2_path).unwrap().modified().unwrap());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(
            stdout,
            format!(
                " NAME        SIZE      HASH         MODIFIED \n \
                package1/0  24.03 KB  {pkg1_hash}  {pkg1_modified} \n \
                package2/0  24.03 KB  {pkg2_hash}  {pkg2_modified} \n",
            ),
        );

        assert_eq!(stderr, "");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_package_list_full_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let env = setup_repo(tmp.path()).await;

        let test_buffers = TestBuffers::default();
        let mut writer = PackagesWriter::new_test(None, &test_buffers);

        run_impl(
            ListSubCommand {
                repository: Some("devhost".to_string()),
                full_hash: true,
                include_components: false,
            },
            env.context.clone(),
            &mut writer,
        )
        .await;

        let blobs_path = tmp.path().join("repository/blobs/1");

        let pkg1_hash = &PKG1_HASH;
        let pkg1_path = blobs_path.join(PKG1_HASH);
        let pkg1_modified = to_rfc2822(std::fs::metadata(pkg1_path).unwrap().modified().unwrap());

        let pkg2_hash = &PKG2_HASH;
        let pkg2_path = blobs_path.join(PKG2_HASH);
        let pkg2_modified = to_rfc2822(std::fs::metadata(pkg2_path).unwrap().modified().unwrap());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(
            stdout,
            format!(
                " NAME        SIZE      HASH                                                              MODIFIED \n \
                package1/0  24.03 KB  {pkg1_hash}  {pkg1_modified} \n \
                package2/0  24.03 KB  {pkg2_hash}  {pkg2_modified} \n",
            ),
        );

        assert_eq!(stderr, "");
    }

    #[fasync::run_singlethreaded(test)]
    #[ignore = "TODO(https://fxbug.dev/344009079): Learn cause of whitespace difference and fix"]
    async fn test_package_list_including_components() {
        let tmp = tempfile::tempdir().unwrap();
        let env = setup_repo(tmp.path()).await;

        let test_buffers = TestBuffers::default();
        let mut writer = PackagesWriter::new_test(None, &test_buffers);

        run_impl(
            ListSubCommand {
                repository: Some("devhost".to_string()),
                full_hash: false,
                include_components: true,
            },
            env.context.clone(),
            &mut writer,
        )
        .await;

        let blobs_path = tmp.path().join("repository/blobs/1");

        let pkg1_hash = &PKG1_HASH[..MAX_HASH];
        let pkg1_path = blobs_path.join(PKG1_HASH);
        let pkg1_modified = to_rfc2822(std::fs::metadata(pkg1_path).unwrap().modified().unwrap());

        let pkg2_hash = &PKG2_HASH[..MAX_HASH];
        let pkg2_path = blobs_path.join(PKG2_HASH);
        let pkg2_modified = to_rfc2822(std::fs::metadata(pkg2_path).unwrap().modified().unwrap());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(
            stdout,
            format!(
                " NAME        SIZE      HASH         MODIFIED                         COMPONENTS \n \
                package1/0  24.03 KB  {pkg1_hash}  {pkg1_modified}  meta/package1.cm \n \
                package2/0  24.03 KB  {pkg2_hash}  {pkg2_modified}  meta/package2.cm \n",
            ),
        );

        assert_eq!(stderr, "");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_show_package_truncated_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let env = setup_repo(tmp.path()).await;

        let test_buffers = TestBuffers::default();
        let mut writer = PackagesWriter::new_test(None, &test_buffers);

        run_impl_for_show_command(
            ShowSubCommand {
                repository: Some("devhost".to_string()),
                full_hash: false,
                include_subpackages: false,
                package: "package1/0".to_string(),
            },
            env.context.clone(),
            &mut writer,
        )
        .await;

        let blobs_path = tmp.path().join("repository/blobs/1");

        let pkg1_hash = &PKG1_HASH[..MAX_HASH];
        let pkg1_path = blobs_path.join(PKG1_HASH);
        let pkg1_modified = to_rfc2822(std::fs::metadata(pkg1_path).unwrap().modified().unwrap());

        let pkg1_bin_hash = &PKG1_BIN_HASH[..MAX_HASH];
        let pkg1_bin_path = blobs_path.join(PKG1_BIN_HASH);
        let pkg1_bin_modified =
            to_rfc2822(std::fs::metadata(pkg1_bin_path).unwrap().modified().unwrap());

        let pkg1_lib_hash = &PKG1_LIB_HASH[..MAX_HASH];
        let pkg1_lib_path = blobs_path.join(PKG1_LIB_HASH);
        let pkg1_lib_modified =
            to_rfc2822(std::fs::metadata(pkg1_lib_path).unwrap().modified().unwrap());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(
            stdout,
            format!(
                " NAME                           SIZE   HASH         MODIFIED \n \
                  bin/package1                   15 B   {pkg1_bin_hash}  {pkg1_bin_modified} \n \
                  lib/package1                   12 B   {pkg1_lib_hash}  {pkg1_lib_modified} \n \
                  meta.far                       24 KB  {pkg1_hash}  {pkg1_modified} \n \
                  meta/contents                  156 B  <unknown>    {pkg1_modified} \n \
                  meta/fuchsia.abi/abi-revision  8 B    <unknown>    {pkg1_modified} \n \
                  meta/package                   33 B   <unknown>    {pkg1_modified} \n \
                  meta/package1.cm               11 B   <unknown>    {pkg1_modified} \n \
                  meta/package1.cmx              12 B   <unknown>    {pkg1_modified} \n"
            ),
        );

        assert_eq!(stderr, "");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_show_package_full_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let env = setup_repo(tmp.path()).await;

        let test_buffers = TestBuffers::default();
        let mut writer = PackagesWriter::new_test(None, &test_buffers);

        run_impl_for_show_command(
            ShowSubCommand {
                repository: Some("devhost".to_string()),
                full_hash: true,
                include_subpackages: false,
                package: "package1/0".to_string(),
            },
            env.context.clone(),
            &mut writer,
        )
        .await;

        let blobs_path = tmp.path().join("repository/blobs/1");

        let pkg1_hash = &PKG1_HASH;
        let pkg1_path = blobs_path.join(PKG1_HASH);
        let pkg1_modified = to_rfc2822(std::fs::metadata(pkg1_path).unwrap().modified().unwrap());

        let pkg1_bin_hash = &PKG1_BIN_HASH;
        let pkg1_bin_path = blobs_path.join(PKG1_BIN_HASH);
        let pkg1_bin_modified =
            to_rfc2822(std::fs::metadata(pkg1_bin_path).unwrap().modified().unwrap());

        let pkg1_lib_hash = &PKG1_LIB_HASH;
        let pkg1_lib_path = blobs_path.join(PKG1_LIB_HASH);
        let pkg1_lib_modified =
            to_rfc2822(std::fs::metadata(pkg1_lib_path).unwrap().modified().unwrap());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(
            stdout,
            format!(
                " NAME                           SIZE   HASH                                                              MODIFIED \n \
                  bin/package1                   15 B   {pkg1_bin_hash}  {pkg1_bin_modified} \n \
                  lib/package1                   12 B   {pkg1_lib_hash}  {pkg1_lib_modified} \n \
                  meta.far                       24 KB  {pkg1_hash}  {pkg1_modified} \n \
                  meta/contents                  156 B  <unknown>                                                         {pkg1_modified} \n \
                  meta/fuchsia.abi/abi-revision  8 B    <unknown>                                                         {pkg1_modified} \n \
                  meta/package                   33 B   <unknown>                                                         {pkg1_modified} \n \
                  meta/package1.cm               11 B   <unknown>                                                         {pkg1_modified} \n \
                  meta/package1.cmx              12 B   <unknown>                                                         {pkg1_modified} \n"
            ),
        );

        assert_eq!(stderr, "");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_show_package_with_subpackages() {
        let tmp = tempfile::tempdir().unwrap();
        let env = setup_repo(tmp.path()).await;

        let test_buffers = TestBuffers::default();
        let mut writer = PackagesWriter::new_test(None, &test_buffers);

        run_impl_for_show_command(
            ShowSubCommand {
                repository: Some("devhost".to_string()),
                full_hash: false,
                include_subpackages: true,
                package: "package1/0".to_string(),
            },
            env.context.clone(),
            &mut writer,
        )
        .await;

        let blobs_path = tmp.path().join("repository/blobs/1");

        let pkg1_hash = &PKG1_HASH[..MAX_HASH];
        let pkg1_path = blobs_path.join(PKG1_HASH);
        let pkg1_modified = to_rfc2822(std::fs::metadata(pkg1_path).unwrap().modified().unwrap());

        let pkg1_bin_hash = &PKG1_BIN_HASH[..MAX_HASH];
        let pkg1_bin_path = blobs_path.join(PKG1_BIN_HASH);
        let pkg1_bin_modified =
            to_rfc2822(std::fs::metadata(pkg1_bin_path).unwrap().modified().unwrap());

        let pkg1_lib_hash = &PKG1_LIB_HASH[..MAX_HASH];
        let pkg1_lib_path = blobs_path.join(PKG1_LIB_HASH);
        let pkg1_lib_modified =
            to_rfc2822(std::fs::metadata(pkg1_lib_path).unwrap().modified().unwrap());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(
            stdout,
            format!(
                " NAME                           SUBPACKAGE  SIZE   HASH         MODIFIED \n \
                  bin/package1                   <root>      15 B   {pkg1_bin_hash}  {pkg1_bin_modified} \n \
                  lib/package1                   <root>      12 B   {pkg1_lib_hash}  {pkg1_lib_modified} \n \
                  meta.far                       <root>      24 KB  {pkg1_hash}  {pkg1_modified} \n \
                  meta/contents                  <root>      156 B  <unknown>    {pkg1_modified} \n \
                  meta/fuchsia.abi/abi-revision  <root>      8 B    <unknown>    {pkg1_modified} \n \
                  meta/package                   <root>      33 B   <unknown>    {pkg1_modified} \n \
                  meta/package1.cm               <root>      11 B   <unknown>    {pkg1_modified} \n \
                  meta/package1.cmx              <root>      12 B   <unknown>    {pkg1_modified} \n"
            ),
        );

        assert_eq!(stderr, "");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_extract_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let env = setup_repo(&tmp.path().join("repo")).await;

        let archive_path = tmp.path().join("archive.far");
        extract_archive_impl(
            ExtractArchiveSubCommand {
                out: archive_path.clone(),
                repository: Some("devhost".to_string()),
                package: "package1/0".to_string(),
            },
            env.context.clone(),
        )
        .await
        .unwrap();

        let mut archive_reader = FarArchiveReader::new(&archive_path).unwrap();
        let mut entries = read_file_entries(&mut archive_reader).unwrap();
        entries.sort();

        assert_eq!(
            entries,
            vec![
                ArchiveEntry {
                    name: "bin/package1".to_string(),
                    path: "72e1e7a504f32edf4f23e7e8a3542c1d77d12541142261cfe272decfa75f542d"
                        .to_string(),
                    length: Some(15),
                },
                ArchiveEntry {
                    name: "lib/package1".to_string(),
                    path: "8a8a5f07f935a4e8e1fd1a1eda39da09bb2438ec0adfb149679ddd6e7e1fbb4f"
                        .to_string(),
                    length: Some(12),
                },
                ArchiveEntry {
                    name: "meta.far".to_string(),
                    path: "meta.far".to_string(),
                    length: Some(24576),
                },
                ArchiveEntry {
                    name: "meta/contents".to_string(),
                    path: "meta/contents".to_string(),
                    length: Some(156),
                },
                ArchiveEntry {
                    name: "meta/fuchsia.abi/abi-revision".to_string(),
                    path: "meta/fuchsia.abi/abi-revision".to_string(),
                    length: Some(8),
                },
                ArchiveEntry {
                    name: "meta/package".to_string(),
                    path: "meta/package".to_string(),
                    length: Some(33),
                },
                ArchiveEntry {
                    name: "meta/package1.cm".to_string(),
                    path: "meta/package1.cm".to_string(),
                    length: Some(11),
                },
                ArchiveEntry {
                    name: "meta/package1.cmx".to_string(),
                    path: "meta/package1.cmx".to_string(),
                    length: Some(12),
                },
            ]
        );
    }
}
