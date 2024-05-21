// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {anyhow::Context as _, fidl_fuchsia_io as fio, std::collections::HashSet, std::sync::Arc};

// Keeping the types as parameters instead of erasing them makes it easier to ensure that all of the
// RootDirs created by pkg-cache are using BootfsThenBlobfs.
pub(crate) type RootDir = package_directory::RootDir<BootfsThenBlobfs>;
pub(crate) type RootDirCache = package_directory::RootDirCache<BootfsThenBlobfs>;

/// An implementation of package_directory::NonMetaStorage that serves blobs from bootfs if they
/// are available and otherwise falls back to blobfs.
/// This saves memory because it allows deduplicating VMOs backed by files that are in both bootfs
/// and blobfs, such as ld.so.
/// Will fall back to only using blobfs if there is an error when initially listing the contents of
/// bootfs. This allows pkg-cache to treat the bootfs-blobs capability as optional.
#[derive(Clone, Debug)]
pub(crate) struct BootfsThenBlobfs(
    // Cloning BootfsThenBlobfs needs to be cheap b/c it is cloned each time a RootDir is created.
    Arc<Inner>,
);

#[derive(Debug)]
struct Inner {
    bootfs: fio::DirectoryProxy,
    bootfs_contents: HashSet<fuchsia_hash::Hash>,
    blobfs: blobfs::Client,
}

impl BootfsThenBlobfs {
    async fn new(bootfs: fio::DirectoryProxy, blobfs: blobfs::Client) -> anyhow::Result<Self> {
        let bootfs_contents = match fuchsia_fs::directory::readdir(&bootfs).await {
            Ok(entries) => entries
                .into_iter()
                .filter_map(|entry| {
                    if matches!(entry.kind, fuchsia_fs::directory::DirentKind::File) {
                        Some(entry.name)
                    } else {
                        None
                    }
                })
                .map(|name| {
                    name.parse::<fuchsia_hash::Hash>().context("invalid blob name in bootfs")
                })
                .collect::<Result<_, _>>()?,
            Err(e) => {
                tracing::warn!(
                    "error reading bootfs blobs directory, will treat as if empty {:#}",
                    anyhow::anyhow!(e)
                );
                HashSet::new()
            }
        };
        Ok(Self(Arc::new(Inner { bootfs, bootfs_contents, blobfs })))
    }
}

impl package_directory::NonMetaStorage for BootfsThenBlobfs {
    fn open(
        &self,
        blob: &fuchsia_hash::Hash,
        flags: fio::OpenFlags,
        scope: package_directory::ExecutionScope,
        server_end: fidl::endpoints::ServerEnd<fio::NodeMarker>,
    ) -> Result<(), fuchsia_fs::node::OpenError> {
        if self.0.bootfs_contents.contains(blob) {
            self.0
                .bootfs
                .open(flags, fio::ModeType::empty(), &blob.to_string(), server_end)
                .map_err(fuchsia_fs::node::OpenError::SendOpenRequest)
        } else {
            self.0
                .blobfs
                .open_blob_for_read(blob, flags, scope, server_end)
                .map_err(fuchsia_fs::node::OpenError::SendOpenRequest)
        }
    }

    #[cfg(feature = "supports_open2")]
    fn open2(
        &self,
        blob: &fuchsia_hash::Hash,
        protocols: fio::ConnectionProtocols,
        scope: package_directory::ExecutionScope,
        object_request: vfs::ObjectRequestRef<'_>,
    ) -> Result<(), fuchsia_zircon::Status> {
        if self.0.bootfs_contents.contains(blob) {
            self.0
                .bootfs
                .open2(&blob.to_string(), &protocols, object_request.take().into_channel())
                .map_err(|e| {
                    tracing::warn!(
                        "Error calling open2 on bootfs blobs dir for blob {blob}: {e:?}"
                    );
                    fuchsia_zircon::Status::INTERNAL
                })
        } else {
            self.0.blobfs.open2_blob_for_read(blob, protocols, scope, object_request)
        }
    }
}

/// Creates RootDirs that will preferentially use bootfs and then blobfs when opening blobs.
#[derive(Debug, Clone)]
pub(crate) struct RootDirFactory {
    bootfs_then_blobfs: BootfsThenBlobfs,
}

impl RootDirFactory {
    fn new(bootfs_then_blobfs: BootfsThenBlobfs) -> Self {
        Self { bootfs_then_blobfs }
    }

    /// Create a RootDir for `hash`.
    pub(crate) async fn create(
        &self,
        hash: fuchsia_hash::Hash,
    ) -> Result<RootDir, package_directory::Error> {
        package_directory::RootDir::new_raw(self.bootfs_then_blobfs.clone(), hash, None).await
    }
}

/// Create a RootDirFactory and a RootDirCache whose RootDirs will use first the supplied `bootfs`
/// and then fallback to the supplied `blobfs` when serving blobs.
pub(crate) async fn new(
    bootfs: fio::DirectoryProxy,
    blobfs: blobfs::Client,
) -> anyhow::Result<(RootDirFactory, RootDirCache)> {
    let bootfs_then_blobfs = BootfsThenBlobfs::new(bootfs, blobfs).await?;
    Ok((RootDirFactory::new(bootfs_then_blobfs.clone()), RootDirCache::new(bootfs_then_blobfs)))
}

/// Like calling `new` with an empty bootfs.
#[cfg(test)]
pub(crate) async fn new_test(blobfs: blobfs::Client) -> (RootDirFactory, RootDirCache) {
    let bootfs_dir = tempfile::tempdir().unwrap();
    let bootfs_proxy = fuchsia_fs::directory::open_in_namespace(
        bootfs_dir.path().to_str().unwrap(),
        fio::OpenFlags::RIGHT_READABLE,
    )
    .unwrap();
    new(bootfs_proxy, blobfs).await.unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    async fn bootfs_then_blobfs_treats_erroring_bootfs_as_empty() {
        let bootfs = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap().0;
        let bootfs_then_blobfs =
            BootfsThenBlobfs::new(bootfs, blobfs::Client::new_test().0).await.unwrap();
        assert_eq!(bootfs_then_blobfs.0.bootfs_contents, HashSet::new());
    }
}
