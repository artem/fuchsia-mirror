// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::args::RepoPublishCommand,
    anyhow::{Context, Result},
    fuchsia_async as fasync,
    fuchsia_lockfile::Lockfile,
    fuchsia_repo::{
        package_manifest_watcher::{PackageManifestWatcher, PackageManifestWatcherBuilder},
        repo_builder::RepoBuilder,
        repo_client::RepoClient,
        repo_keys::RepoKeys,
        repository::{Error as RepoError, PmRepository},
    },
    futures::{FutureExt, StreamExt},
    std::{
        collections::BTreeSet,
        fs::File,
        io::{BufWriter, Write},
        path::Path,
        pin::Pin,
    },
    tracing::{error, warn},
    tuf::{metadata::RawSignedMetadata, Error as TufError},
};

/// Time in seconds after which the attempt to get a lock file is considered failed.
const LOCK_TIMEOUT_SEC: u64 = 2 * 60;

/// Time in milliseconds at which the filesystem update events are rated.
const WATCHER_NEXT_EVENT_RATE_MILLIS: u64 = 200;

/// Filename for lockfile for repository that's being processed by the command.
const REPOSITORY_LOCK_FILENAME: &str = ".repository.lock";

pub async fn cmd_repo_publish(cmd: RepoPublishCommand) -> Result<()> {
    if cmd.watch {
        repo_incremental_publish(&cmd, WATCHER_NEXT_EVENT_RATE_MILLIS).await
    } else {
        repo_publish(&cmd).await
    }
}

async fn repo_incremental_publish(cmd: &RepoPublishCommand, watcher_rate: u64) -> Result<()> {
    let mut watcher = PackageManifestWatcher::watch(
        PackageManifestWatcherBuilder::builder()
            .manifests(cmd.package_manifests.iter())
            .lists(cmd.package_list_manifests.iter())
            .build(),
    )?
    .peekable();
    loop {
        repo_publish(cmd).await.unwrap_or_else(|e| warn!("Repo publish error: {:?}", e));

        // TODO(fxb/117882): The current implementation of `PackageManifestWatcher` guarantees that
        // all buffered events can be processed, but future implementations might change this. It
        // should be considered to rewrite things such that the watcher only retains the last event
        // to avoid the risk of a bunch of changes being queued up to avoid unnecessary
        // re-publishes.
        // Wait for 200ms after last file change.
        let watcher_result = loop {
            let watcher_result = watcher.next().await;
            fuchsia_async::Timer::new(fuchsia_async::Duration::from_millis(watcher_rate)).await;
            // If the stream is empty, return the result.
            if Pin::new(&mut watcher).peek().now_or_never().is_none() {
                break watcher_result;
            } else {
                // Skip all but the last item on the stream.
                while Pin::new(&mut watcher).peek().now_or_never().is_some() {
                    let _ = watcher.next().await;
                }
            }
        };

        // Exit the loop if the notify watcher has shut down.
        if watcher_result.is_none() {
            break Ok(());
        }
    }
}

async fn lock_repository(lock_path: &Path) -> Result<Lockfile> {
    Ok(Lockfile::lock_for(lock_path, std::time::Duration::from_secs(LOCK_TIMEOUT_SEC))
        .await
        .map_err(|e| {
            error!(
                "Failed to aquire a lockfile. Check that {lockpath} doesn't exist and \
                 can be written to. Ownership information: {owner:#?}",
                lockpath = e.lock_path.display(),
                owner = e.owner
            );
            e
        })?)
}

async fn repo_publish(cmd: &RepoPublishCommand) -> Result<()> {
    if !cmd.repo_path.exists() {
        std::fs::create_dir(&cmd.repo_path).expect("creating repository parent dir");
    }
    let dir = cmd.repo_path.join("repository");
    if !dir.exists() {
        std::fs::create_dir(&dir).expect("creating repository dir");
    }
    let lock_path = dir.join(REPOSITORY_LOCK_FILENAME).into_std_path_buf();
    let lock_file = {
        let _log_warning_task = fasync::Task::local({
            let lock_path = lock_path.clone();
            async move {
                fasync::Timer::new(fasync::Duration::from_secs(30)).await;
                warn!("Obtaining a lock at {} not complete after 30s", &lock_path.display());
            }
        });
        lock_repository(&lock_path).await?
    };
    let publish_result = repo_publish_oneshot(cmd).await;
    lock_file.unlock()?;
    publish_result
}

async fn repo_publish_oneshot(cmd: &RepoPublishCommand) -> Result<()> {
    let repo = PmRepository::builder(cmd.repo_path.clone()).copy_mode(cmd.copy_mode).build();

    let mut deps = BTreeSet::new();

    // Load the signing metadata keys if from a file if specified.
    let repo_signing_keys = if let Some(path) = &cmd.signing_keys {
        if !path.exists() {
            anyhow::bail!("--signing-keys path {} does not exist", path);
        }

        let keys = RepoKeys::from_dir(path.as_std_path())?;
        deps.insert(path.clone());

        Some(keys)
    } else {
        None
    };

    // Load the trusted metadata keys. If they weren't passed in a trusted keys file, try to read
    // the keys from the repository.
    let repo_trusted_keys = if let Some(path) = &cmd.trusted_keys {
        if !path.exists() {
            anyhow::bail!("--trusted-keys path {} does not exist", path);
        }

        RepoKeys::from_dir(path.as_std_path())?
    } else {
        repo.repo_keys()?
    };

    // Try to connect to the repository. This should succeed if we have at least some root metadata
    // in the repository. If none exists, we'll create a new repository.
    let client = if let Some(trusted_root_path) = &cmd.trusted_root {
        let buf = async_fs::read(&trusted_root_path)
            .await
            .with_context(|| format!("reading trusted root {trusted_root_path}"))?;

        let trusted_root = RawSignedMetadata::new(buf);

        Some(RepoClient::from_trusted_root(&trusted_root, &repo).await?)
    } else {
        match RepoClient::from_trusted_remote(&repo).await {
            Ok(mut client) => {
                // Make sure our client has the latest metadata. It's okay if this fails with missing
                // metadata since we'll create it when we publish to the repository.
                match client.update().await {
                    Ok(_) | Err(RepoError::Tuf(TufError::MetadataNotFound { .. })) => {}
                    Err(err) => {
                        return Err(err.into());
                    }
                }

                Some(client)
            }
            Err(RepoError::Tuf(TufError::MetadataNotFound { .. })) => None,
            Err(err) => {
                return Err(err.into());
            }
        }
    };

    let mut repo_builder = if let Some(client) = &client {
        RepoBuilder::from_database(&repo, &repo_trusted_keys, client.database())
    } else {
        RepoBuilder::create(&repo, &repo_trusted_keys)
    };

    if let Some(repo_signing_keys) = &repo_signing_keys {
        repo_builder = repo_builder.signing_repo_keys(repo_signing_keys);
    }

    repo_builder = repo_builder
        .current_time(cmd.metadata_current_time)
        .time_versioning(cmd.time_versioning)
        .inherit_from_trusted_targets(!cmd.clean);

    repo_builder = if cmd.refresh_root {
        repo_builder.refresh_metadata(true)
    } else {
        repo_builder.refresh_non_root_metadata(true)
    };

    // Publish all the packages.
    deps.extend(
        repo_builder
            .add_packages(cmd.package_manifests.iter().cloned())
            .await?
            .add_package_lists(cmd.package_list_manifests.iter().cloned())
            .await?
            .add_package_archives(cmd.package_archives.iter().cloned())
            .await?
            .commit()
            .await?,
    );

    if let Some(depfile_path) = &cmd.depfile {
        let timestamp_path = cmd.repo_path.join("repository").join("timestamp.json");

        let file =
            File::create(depfile_path).with_context(|| format!("creating {depfile_path}"))?;

        let mut file = BufWriter::new(file);

        write!(file, "{timestamp_path}:")?;

        for path in deps {
            // Spaces are separators, so spaces in filenames must be escaped.
            let path = path.as_str().replace(' ', "\\ ");

            write!(file, " {path}")?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        camino::{Utf8Path, Utf8PathBuf},
        chrono::{TimeZone, Utc},
        fuchsia_async as fasync,
        fuchsia_pkg::{PackageManifest, PackageManifestList},
        fuchsia_repo::{repository::CopyMode, test_utils},
        tuf::metadata::Metadata as _,
    };

    struct TestEnv {
        _tmp: tempfile::TempDir,
        cmd: RepoPublishCommand,
        repo_path: Utf8PathBuf,
        manifests: Vec<PackageManifest>,
        manifest_paths: Vec<Utf8PathBuf>,
        list_paths: Vec<Utf8PathBuf>,
        depfile_path: Utf8PathBuf,
    }

    impl TestEnv {
        fn new() -> Self {
            let tempdir = tempfile::tempdir().unwrap();
            let root = Utf8Path::from_path(tempdir.path()).unwrap();

            let repo_path = root.join("repo");
            test_utils::make_empty_pm_repo_dir(&repo_path);

            // Build some packages to publish.
            let mut manifests = vec![];
            let mut manifest_paths = vec![];
            for name in (1..=5).map(|i| format!("package{}", i)) {
                let (pkg_manifest, pkg_manifest_path) = create_manifest(&name, root);
                manifests.push(pkg_manifest);
                manifest_paths.push(pkg_manifest_path);
            }

            let list_names = (1..=2).map(|i| format!("list{}.json", i)).collect::<Vec<_>>();
            let list_paths = list_names.iter().map(|name| root.join(name)).collect::<Vec<_>>();
            // Bundle up package3, package4, and package5 into package list manifests.
            let list_parent = list_paths[0].parent().unwrap();
            let pkg_list1_manifest = PackageManifestList::from_iter(
                [&manifest_paths[2], &manifest_paths[3]]
                    .iter()
                    .map(|path| path.strip_prefix(list_parent).unwrap())
                    .map(Into::into),
            );
            pkg_list1_manifest.to_writer(File::create(&list_paths[0]).unwrap()).unwrap();

            let list_parent = list_paths[1].parent().unwrap();
            let pkg_list2_manifest = PackageManifestList::from_iter(
                [&manifest_paths[4]]
                    .iter()
                    .map(|path| path.strip_prefix(list_parent).unwrap())
                    .map(Into::into),
            );
            pkg_list2_manifest.to_writer(File::create(&list_paths[1]).unwrap()).unwrap();

            let depfile_path = root.join("deps");

            let cmd = RepoPublishCommand {
                watch: false,
                signing_keys: None,
                trusted_keys: None,
                trusted_root: None,
                package_archives: vec![],
                package_manifests: manifest_paths[0..2].to_vec(),
                package_list_manifests: list_paths.clone(),
                metadata_current_time: Utc::now(),
                time_versioning: false,
                refresh_root: false,
                clean: false,
                depfile: Some(depfile_path.clone()),
                copy_mode: CopyMode::Copy,
                repo_path: repo_path.to_path_buf(),
            };

            TestEnv {
                _tmp: tempdir,
                cmd,
                repo_path,
                manifests,
                list_paths,
                depfile_path,
                manifest_paths,
            }
        }

        // takes paths to manifests - lists and packages
        fn validate_manifest_blobs(&self, expected_deps: BTreeSet<Utf8PathBuf>) {
            let mut expected_deps = expected_deps;
            let blob_repo_path = self.repo_path.join("repository").join("blobs");

            for package_manifest in &self.manifests {
                for blob in package_manifest.blobs() {
                    expected_deps.insert(blob.source_path.clone().into());
                    let blob_path = blob_repo_path.join(blob.merkle.to_string());
                    assert_eq!(
                        std::fs::read(&blob.source_path).unwrap(),
                        std::fs::read(blob_path).unwrap(),
                    );
                }
            }

            assert_eq!(
                std::fs::read_to_string(&self.depfile_path).unwrap(),
                format!(
                    "{}: {}",
                    self.repo_path.join("repository").join("timestamp.json"),
                    expected_deps.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(" "),
                )
            );
        }
    }

    fn create_manifest(name: &str, root: &Utf8Path) -> (PackageManifest, Utf8PathBuf) {
        let pkg_build_path = root.join(name);
        let pkg_manifest_path = root.join(format!("{}.json", name));

        let (_, pkg_manifest) =
            test_utils::make_package_manifest(name, pkg_build_path.as_std_path(), Vec::new());
        serde_json::to_writer(File::create(&pkg_manifest_path).unwrap(), &pkg_manifest).unwrap();
        (pkg_manifest, pkg_manifest_path)
    }

    fn update_file(path: &Utf8PathBuf, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn update_manifest(path: &Utf8PathBuf, manifest: &PackageManifest) {
        let file = std::fs::OpenOptions::new().write(true).truncate(true).open(path).unwrap();
        serde_json::to_writer(file, manifest).unwrap();
    }

    // Waits for the repo to be unlocked.
    async fn ensure_repo_unlocked(repo_path: &Utf8Path) {
        fasync::Timer::new(fasync::Duration::from_millis(100)).await;
        lock_repository(repo_path.as_std_path()).await.unwrap().unlock().unwrap();
    }

    #[fuchsia::test]
    async fn test_repository_should_error_with_no_keys_if_it_does_not_exist() {
        let tempdir = tempfile::tempdir().unwrap();
        let repo_path = Utf8Path::from_path(tempdir.path()).unwrap();

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Err(_));
    }

    #[fuchsia::test]
    async fn test_repository_should_create_repo_with_keys_if_it_does_not_exist() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tempdir.path()).unwrap();

        let repo_keys_path = root.join("keys");
        let repo_path = root.join("repo");

        test_utils::make_repo_keys_dir(&repo_keys_path);

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: Some(repo_keys_path),
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        let repo = PmRepository::new(repo_path);
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 1);
        assert_eq!(repo_client.database().trusted_targets().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_snapshot().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_timestamp().map(|m| m.version()), Some(1));
    }

    #[fuchsia::test]
    async fn test_repository_should_create_repo_if_only_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        // First create a repository.
        let full_repo_path = root.join("full");
        let full_metadata_repo_path = full_repo_path.join("repository");
        test_utils::make_pm_repo_dir(full_repo_path.as_std_path()).await;

        // Then create a repository, which only has the keys and root metadata in it.
        let test_repo_path = root.join("test");
        let test_metadata_repo_path = test_repo_path.join("repository");
        std::fs::create_dir_all(&test_metadata_repo_path).unwrap();

        std::fs::rename(full_repo_path.join("keys"), test_repo_path.join("keys")).unwrap();

        std::fs::copy(
            full_metadata_repo_path.join("root.json"),
            test_metadata_repo_path.join("1.root.json"),
        )
        .unwrap();

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: test_repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        let repo = PmRepository::new(test_repo_path);
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 1);
        assert_eq!(repo_client.database().trusted_targets().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_snapshot().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_timestamp().map(|m| m.version()), Some(1));
    }

    #[fuchsia::test]
    async fn test_publish_nothing_to_empty_pm_repo() {
        let tempdir = tempfile::tempdir().unwrap();
        let repo_path = Utf8Path::from_path(tempdir.path()).unwrap();

        test_utils::make_empty_pm_repo_dir(repo_path);

        // Connect to the repo before we run the command to make sure we generate new metadata.
        let repo = PmRepository::new(repo_path.to_path_buf());
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 1);
        assert_eq!(repo_client.database().trusted_targets().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_snapshot().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_timestamp().map(|m| m.version()), Some(1));

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        // Even though we didn't add any packages, we still should have refreshed the metadata.
        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 1);
        assert_eq!(repo_client.database().trusted_targets().map(|m| m.version()), Some(2));
        assert_eq!(repo_client.database().trusted_snapshot().map(|m| m.version()), Some(2));
        assert_eq!(repo_client.database().trusted_timestamp().map(|m| m.version()), Some(2));
    }

    #[fuchsia::test]
    async fn test_publish_refreshes_root_metadata() {
        let tempdir = tempfile::tempdir().unwrap();
        let repo_path = Utf8Path::from_path(tempdir.path()).unwrap();

        test_utils::make_empty_pm_repo_dir(repo_path);

        // Connect to the repo before we run the command to make sure we generate new metadata.
        let repo = PmRepository::new(repo_path.to_path_buf());
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 1);
        assert_eq!(repo_client.database().trusted_targets().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_snapshot().map(|m| m.version()), Some(1));
        assert_eq!(repo_client.database().trusted_timestamp().map(|m| m.version()), Some(1));

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: true,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        // Even though we didn't add any packages, we still should have refreshed the metadata.
        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 2);
        assert_eq!(repo_client.database().trusted_targets().map(|m| m.version()), Some(2));
        assert_eq!(repo_client.database().trusted_snapshot().map(|m| m.version()), Some(2));
        assert_eq!(repo_client.database().trusted_timestamp().map(|m| m.version()), Some(2));
    }

    #[fuchsia::test]
    async fn test_keys_path() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tempdir.path()).unwrap();
        let repo_path = root.join("repository");
        let keys_path = root.join("keys");

        test_utils::make_empty_pm_repo_dir(&repo_path);

        // Move the keys directory out of the repository. We should error out since we can't find
        // the keys.
        std::fs::rename(repo_path.join("keys"), &keys_path).unwrap();

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Err(_));

        // Explicitly specifying the keys path should work though.
        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: Some(keys_path),
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));
    }

    #[fuchsia::test]
    async fn test_time_versioning() {
        let tempdir = tempfile::tempdir().unwrap();
        let repo_path = Utf8Path::from_path(tempdir.path()).unwrap();

        // Time versioning uses the unix timestamp of the current time. Note that the TUF spec does
        // not allow `0` for a version, so tuf::RepoBuilder will fall back to normal versioning if
        // we have a unix timestamp of 0, so we'll use a non-zero time.
        let time_version = 100u32;
        let now = Utc.timestamp(time_version as i64, 0);

        test_utils::make_empty_pm_repo_dir(repo_path);

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: now,
            time_versioning: true,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        // The metadata we generated should use the current time for a version.
        let repo = PmRepository::new(repo_path.to_path_buf());
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update_with_start_time(&now).await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 1);
        assert_eq!(
            repo_client.database().trusted_targets().map(|m| m.version()).unwrap(),
            time_version,
        );
        assert_eq!(
            repo_client.database().trusted_snapshot().map(|m| m.version()).unwrap(),
            time_version,
        );
        assert_eq!(
            repo_client.database().trusted_timestamp().map(|m| m.version()).unwrap(),
            time_version,
        );
    }

    #[fuchsia::test]
    async fn test_publish_archives() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tempdir.path()).unwrap();

        let repo_path = root.join("repo");
        test_utils::make_empty_pm_repo_dir(&repo_path);

        // Build some packages to publish.
        let mut archives = vec![];
        for name in ["package1", "package2", "package3", "package4", "package5"] {
            let pkg_build_path = root.join(name);
            std::fs::create_dir(pkg_build_path.clone()).expect("create package directory");

            let archive_path =
                test_utils::make_package_archive(name, pkg_build_path.as_std_path()).await;

            archives.push(archive_path);
        }
        let depfile_path = root.join("deps");

        // Publish the packages.
        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: archives,
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: Some(depfile_path.clone()),
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };

        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        let repo = PmRepository::new(repo_path.to_path_buf());
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));

        let pkg1_archive_path = root.join("package1").join("package1.far");
        let pkg2_archive_path = root.join("package2").join("package2.far");
        let pkg3_archive_path = root.join("package3").join("package3.far");
        let pkg4_archive_path = root.join("package4").join("package4.far");
        let pkg5_archive_path = root.join("package5").join("package5.far");

        let expected_deps = BTreeSet::from([
            pkg1_archive_path,
            pkg2_archive_path,
            pkg3_archive_path,
            pkg4_archive_path,
            pkg5_archive_path,
        ]);

        let depfile_str = std::fs::read_to_string(&depfile_path).unwrap();
        for arch_path in expected_deps {
            assert!(depfile_str.contains(&arch_path.to_string()))
        }
    }

    #[fuchsia::test]
    async fn test_publish_packages() {
        let env = TestEnv::new();

        // Publish the packages.
        assert_matches!(repo_publish(&env.cmd).await, Ok(()));

        let repo = PmRepository::new(env.repo_path.to_path_buf());
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));

        assert_eq!(repo_client.database().trusted_root().version(), 1);
        assert_eq!(repo_client.database().trusted_targets().map(|m| m.version()), Some(2));
        assert_eq!(repo_client.database().trusted_snapshot().map(|m| m.version()), Some(2));
        assert_eq!(repo_client.database().trusted_timestamp().map(|m| m.version()), Some(2));

        let expected_deps =
            BTreeSet::from_iter(env.manifest_paths.iter().chain(env.list_paths.iter()).cloned());
        env.validate_manifest_blobs(expected_deps);
    }

    #[fuchsia::test]
    async fn test_publish_packages_should_support_cleaning() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tempdir.path()).unwrap();

        // Create a repository that contains packages package1 and package2.
        let repo_path = root.join("repo");
        test_utils::make_pm_repo_dir(repo_path.as_std_path()).await;

        // Publish package3 without cleaning enabled. This should preserve the old packages.
        let pkg3_manifest_path = root.join("package3.json");
        let (_, pkg3_manifest) = test_utils::make_package_manifest(
            "package3",
            root.join("pkg3").as_std_path(),
            Vec::new(),
        );
        serde_json::to_writer(File::create(&pkg3_manifest_path).unwrap(), &pkg3_manifest).unwrap();

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![pkg3_manifest_path],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: false,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };
        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        let repo = PmRepository::new(repo_path.to_path_buf());
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        let trusted_targets = repo_client.database().trusted_targets().unwrap();
        assert!(trusted_targets.targets().get("package1/0").is_some());
        assert!(trusted_targets.targets().get("package2/0").is_some());
        assert!(trusted_targets.targets().get("package3/0").is_some());

        // Publish package4 with cleaning should clean out the old packages.
        let pkg4_manifest_path = root.join("package4.json");
        let (_, pkg4_manifest) = test_utils::make_package_manifest(
            "package4",
            root.join("pkg4").as_std_path(),
            Vec::new(),
        );
        serde_json::to_writer(File::create(&pkg4_manifest_path).unwrap(), &pkg4_manifest).unwrap();

        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: None,
            package_archives: vec![],
            package_manifests: vec![pkg4_manifest_path],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: true,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: repo_path.to_path_buf(),
        };
        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        let repo = PmRepository::new(repo_path.to_path_buf());
        let mut repo_client = RepoClient::from_trusted_remote(repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        let trusted_targets = repo_client.database().trusted_targets().unwrap();
        assert!(trusted_targets.targets().get("package1/0").is_none());
        assert!(trusted_targets.targets().get("package2/0").is_none());
        assert!(trusted_targets.targets().get("package3/0").is_none());
        assert!(trusted_targets.targets().get("package4/0").is_some());
    }

    #[fuchsia::test]
    async fn test_trusted_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        // Create a simple repository.
        let repo = test_utils::make_pm_repository(root).await;

        // Refresh all the metadata using 1.root.json.
        let cmd = RepoPublishCommand {
            watch: false,
            signing_keys: None,
            trusted_keys: None,
            trusted_root: Some(root.join("repository").join("1.root.json")),
            package_archives: vec![],
            package_manifests: vec![],
            package_list_manifests: vec![],
            metadata_current_time: Utc::now(),
            time_versioning: false,
            refresh_root: false,
            clean: true,
            depfile: None,
            copy_mode: CopyMode::Copy,
            repo_path: root.to_path_buf(),
        };
        assert_matches!(cmd_repo_publish(cmd).await, Ok(()));

        // Make sure we can update a client with 1.root.json metadata.
        let buf = async_fs::read(root.join("repository").join("1.root.json")).await.unwrap();
        let trusted_root = RawSignedMetadata::new(buf);
        let mut repo_client = RepoClient::from_trusted_root(&trusted_root, &repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_root().version(), 1);
    }

    #[fuchsia::test]
    async fn test_concurrent_publish() {
        let env = TestEnv::new();
        assert_matches!(
            futures::join!(repo_publish(&env.cmd), repo_publish(&env.cmd)),
            (Ok(()), Ok(()))
        );
    }

    #[fuchsia::test]
    async fn test_watch_republishes_on_package_change() {
        let mut env = TestEnv::new();
        env.cmd.watch = true;
        let mut publish_fut = Box::pin(repo_incremental_publish(&env.cmd, 10)).fuse();

        futures::select! {
            r = publish_fut => panic!("Incremental publishing exited early: {:?}", r),
            _ = ensure_repo_unlocked(&env.repo_path).fuse() => {},
        }

        // Make changes to the watched manifest.
        let (_, manifest) =
            test_utils::make_package_manifest("foobar", env.repo_path.as_std_path(), Vec::new());
        update_manifest(&env.manifest_paths[4], &manifest);

        futures::select! {
            r = publish_fut => panic!("Incremental publishing exited early: {:?}", r),
            _ = ensure_repo_unlocked(&env.repo_path).fuse() => {},
        }

        let expected_deps =
            BTreeSet::from_iter(env.manifest_paths.iter().chain(env.list_paths.iter()).cloned());
        env.validate_manifest_blobs(expected_deps);
    }

    #[fuchsia::test]
    async fn test_watch_unlocks_repository_on_error() {
        let mut env = TestEnv::new();
        env.cmd.watch = true;
        let mut publish_fut = Box::pin(repo_incremental_publish(&env.cmd, 10)).fuse();

        futures::select! {
            r = publish_fut => panic!("Incremental publishing exited early: {:?}", r),
            _ = ensure_repo_unlocked(&env.repo_path).fuse() => {},
        }

        // Make changes to the watched manifests.
        update_file(&env.manifest_paths[4], br#"invalid content"#);

        futures::select! {
            r = publish_fut => panic!("Incremental publishing exited early: {:?}", r),
            _ = ensure_repo_unlocked(&env.repo_path).fuse() => {},
        }

        // Test will timeout if the repository is not unlocked because of the error.
    }
}
