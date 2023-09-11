// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context,
    fidl::endpoints::ClientEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{StreamExt, TryFutureExt, TryStreamExt},
    tracing::error,
};

enum IncomingService {
    PkgDir(fidl_test_pkgdir::PkgDirRequestStream),
}

#[fuchsia::main]
async fn main() -> anyhow::Result<()> {
    let use_fxblob = pkgdir_config::Config::take_from_startup_handle().use_fxblob;
    let builder = blobfs::Client::builder().readable();
    let blobfs = if use_fxblob { builder.use_reader() } else { builder }
        .build()
        .await
        .context("creating blobfs client")?;
    let mut service_fs = fuchsia_component::server::ServiceFs::new_local();
    service_fs.dir("svc").add_fidl_service(IncomingService::PkgDir);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;
    let () = service_fs
        .for_each_concurrent(None, |request| async {
            match request {
                IncomingService::PkgDir(stream) => {
                    serve_request_stream(stream, blobfs.clone())
                        .unwrap_or_else(|e| error!("failed to serve PkgDirRequest: {:#}", e))
                        .await
                }
            }
        })
        .await;
    Ok(())
}

async fn serve_request_stream(
    mut stream: fidl_test_pkgdir::PkgDirRequestStream,
    blobfs: blobfs::Client,
) -> anyhow::Result<()> {
    while let Some(request) =
        stream.try_next().await.context("failed to read request from FIDL stream")?
    {
        match request {
            fidl_test_pkgdir::PkgDirRequest::OpenPackageDirectory { meta_far, responder } => {
                responder
                    .send(open_package_directory(fuchsia_hash::Hash::from(meta_far), &blobfs).await)
                    .context("failed to send OpenPackageDirectory response")?;
            }
        }
    }
    Ok(())
}

async fn open_package_directory(
    meta_far: fuchsia_hash::Hash,
    blobfs: &blobfs::Client,
) -> Result<ClientEnd<fio::DirectoryMarker>, i32> {
    let (client, server) = fidl::endpoints::create_endpoints();
    let () = package_directory::serve(
        package_directory::ExecutionScope::new(),
        blobfs.clone(),
        meta_far,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        server,
    )
    .await
    .map_err(|_| zx::Status::INTERNAL.into_raw())?;
    Ok(client)
}
