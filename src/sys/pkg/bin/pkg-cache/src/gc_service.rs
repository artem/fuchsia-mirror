// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::base_packages::{BasePackages, CachePackages},
    crate::index::PackageIndex,
    anyhow::anyhow,
    anyhow::Context as _,
    fidl_fuchsia_space::{
        ErrorCode as SpaceErrorCode, ManagerRequest as SpaceManagerRequest,
        ManagerRequestStream as SpaceManagerRequestStream,
    },
    fidl_fuchsia_update::CommitStatusProviderProxy,
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::prelude::*,
    std::{collections::HashSet, sync::Arc},
    tracing::{error, info},
};

pub async fn serve(
    blobfs: blobfs::Client,
    base_packages: Arc<BasePackages>,
    cache_packages: Arc<CachePackages>,
    package_index: Arc<async_lock::RwLock<PackageIndex>>,
    open_packages: package_directory::RootDirCache<blobfs::Client>,
    protect_dynamic_packages: crate::DynamicProtection,
    protect_open_packages: crate::OpenProtection,
    commit_status_provider: CommitStatusProviderProxy,
    mut stream: SpaceManagerRequestStream,
) -> Result<(), anyhow::Error> {
    let event_pair = commit_status_provider
        .is_current_system_committed()
        .await
        .context("while getting event pair")?;

    while let Some(event) = stream.try_next().await? {
        let SpaceManagerRequest::Gc { responder } = event;
        responder.send(
            gc(
                &blobfs,
                base_packages.as_ref(),
                cache_packages.as_ref(),
                &package_index,
                &open_packages,
                protect_dynamic_packages,
                protect_open_packages,
                &event_pair,
            )
            .await,
        )?;
    }
    Ok(())
}

async fn gc(
    blobfs: &blobfs::Client,
    base_packages: &BasePackages,
    cache_packages: &CachePackages,
    package_index: &Arc<async_lock::RwLock<PackageIndex>>,
    open_packages: &package_directory::RootDirCache<blobfs::Client>,
    protect_dynamic_packages: crate::DynamicProtection,
    protect_open_packages: crate::OpenProtection,
    event_pair: &zx::EventPair,
) -> Result<(), SpaceErrorCode> {
    info!("performing gc");

    event_pair.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST).map_err(|e| {
        match e {
            zx::Status::TIMED_OUT => {
                info!("GC is blocked pending update.");
            }
            zx::Status::CANCELED => {
                info!("Commit handle is closed, likely because we are rebooting.");
            }
            other => {
                error!("Got unexpected status {:?} while waiting on handle.", other);
            }
        }
        SpaceErrorCode::PendingCommit
    })?;

    // The primary purpose of GC is to free space to enable an OTA, so we continue if possible
    // through any errors and delete as many blobs as we can (because OTA'ing may be the only way
    // to actually fix the errors).
    async move {
        // Determine all resident blobs before locking the package index to decrease the amount of
        // time the package index lock is held. Blobs written after this are implicitly protected.
        // This is for speed and not necessary for correctness. It would still be correct if the
        // list of resident blobs was determined any time after the package index lock is taken.
        // This error can not be ignored because we wouldn't know which blobs to delete.
        let mut eligible_blobs = blobfs.list_known_blobs().await?;

        // Lock the package index until we are done deleting blobs. During resolution, required
        // blobs are added to the index before their presence in blobfs is checked, so locking
        // the index until we are done deleting blobs guarantees we will never delete a blob
        // that resolution thinks it can skip fetching.
        let package_index = package_index.read().await;
        match protect_dynamic_packages {
            crate::DynamicProtection::Enabled => {
                let () = package_index.all_blobs().iter().for_each(|blob| {
                    eligible_blobs.remove(blob);
                });
            }
            crate::DynamicProtection::Disabled => {
                let () = package_index.all_blobs_ignoring_dynamic_index().iter().for_each(|blob| {
                    eligible_blobs.remove(blob);
                });
            }
        }

        let () = base_packages.list_blobs().iter().for_each(|blob| {
            eligible_blobs.remove(blob);
        });

        let () = cache_packages.list_blobs().iter().for_each(|blob| {
            eligible_blobs.remove(blob);
        });

        if matches!(protect_open_packages, crate::OpenProtection::Enabled) {
            let () = protect_open_blobs(&mut eligible_blobs, open_packages).await;
        }

        info!("Garbage collecting {} blobs...", eligible_blobs.len());
        let mut errors = 0;
        for (i, blob) in eligible_blobs.iter().enumerate() {
            let () = blobfs.delete_blob(blob).await.unwrap_or_else(|e| {
                error!(%blob, "Failed to delete blob: {:#}", anyhow!(e));
                errors += 1;
            });
            if (i + 1) % 100 == 0 {
                info!("{} blobs deleted...", i + 1 - errors);
            }
        }
        info!("Garbage collection done. Deleted {} blobs.", eligible_blobs.len() - errors);
        Ok(())
    }
    .await
    .map_err(|e: anyhow::Error| {
        error!("Failed to perform GC operation: {:#}", e);
        SpaceErrorCode::Internal
    })
}

async fn protect_open_blobs(
    eligible_blobs: &mut HashSet<fuchsia_hash::Hash>,
    open_packages: &package_directory::RootDirCache<blobfs::Client>,
) {
    let mut to_visit = open_packages.list();
    let mut enqueued = to_visit.iter().map(|pkg| *pkg.hash()).collect::<HashSet<_>>();
    while let Some(pkg) = to_visit.pop() {
        eligible_blobs.remove(pkg.hash());
        pkg.external_file_hashes().for_each(|h| {
            eligible_blobs.remove(h);
        });
        let subpackages = match pkg.subpackages().await {
            Ok(subpackages) => subpackages,
            Err(e) => {
                // Blobs necessary for OTA are already protected by the base index.
                error!(
                    "Could not determine subpackages of open package {}. \
                     The subpackages will NOT be protected from GC: {:#}",
                    pkg.hash(),
                    anyhow!(e)
                );
                continue;
            }
        };
        for h in subpackages.into_hashes_undeduplicated() {
            if enqueued.insert(h) {
                let sub = match open_packages.get_or_insert(h, None).await {
                    Ok(sub) => sub,
                    Err(e) => {
                        // Blobs necessary for OTA are already protected by the base index.
                        error!(
                            "Could not determine blobs of {h}, a subpackage of open package {}. \
                             The subpackage will NOT be protected from GC: {:#}",
                            pkg.hash(),
                            anyhow!(e)
                        );
                        continue;
                    }
                };
                to_visit.push(sub);
            }
        }
    }
}
