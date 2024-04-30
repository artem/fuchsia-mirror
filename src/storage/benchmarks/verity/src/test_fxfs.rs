// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    fidl::endpoints::Proxy,
    fidl_fuchsia_fxfs_test::{TestFxfsAdminRequest, TestFxfsAdminRequestStream},
    fidl_fuchsia_io as fio,
    fuchsia_component::server::ServiceFs,
    fuchsia_fs::directory::open_in_namespace,
    fuchsia_storage_benchmarks_lib::{block_devices::FvmVolumeFactory, filesystems::fxfs::Fxfs},
    futures::StreamExt,
    storage_benchmarks::{CacheClearableFilesystem, Filesystem, FilesystemConfig},
    vfs::{directory::helper::DirectlyMutable, remote::remote_dir},
};

enum IncomingRequest {
    TestAdmin(TestFxfsAdminRequestStream),
}

#[fuchsia::main]
async fn main() {
    let fvm_volume_factory = FvmVolumeFactory::new().await.unwrap();
    let mut fs = Fxfs::new(20 * 1024 * 1024).start_filesystem(&fvm_volume_factory).await;
    let mut svc = ServiceFs::new();
    let root_dir = open_in_namespace(
        &fs.benchmark_dir().to_string_lossy(),
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::RIGHT_READABLE,
    )
    .unwrap();
    let data_dir = vfs::pseudo_directory! {
        "fxfs_root_dir" => remote_dir(root_dir)
    };
    svc.add_entry_at("data", data_dir.clone());
    svc.dir("svc").add_fidl_service(IncomingRequest::TestAdmin);

    svc.take_and_serve_directory_handle().unwrap();
    'outer: while let Some(request) = svc.next().await {
        match request {
            IncomingRequest::TestAdmin(mut stream) => {
                while let Some(request) = stream.next().await {
                    match request.unwrap() {
                        TestFxfsAdminRequest::ClearCache { responder } => {
                            fs.clear_cache().await;
                            // The main component acquires a handle to /data that becomes invalid
                            // upon clearing the cache. Thus, clear_cache returns the new valid
                            // /data handle for the main component to use. The read_verified_file
                            // Starnix component does not have this issue because it requests a
                            // fresh /data handle when it gets launched. Therefore as long as /data
                            // is pointing to the new valid /data handle at that point, the Starnix
                            // component can safely use the /data handle it acquires in its
                            // namespace.
                            let root_dir_for_starnix = open_in_namespace(
                                &fs.benchmark_dir().to_string_lossy(),
                                fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::RIGHT_READABLE,
                            )
                            .unwrap();
                            let root_dir_for_main = fuchsia_fs::directory::clone_no_describe(
                                &root_dir_for_starnix,
                                Some(fio::OpenFlags::CLONE_SAME_RIGHTS),
                            )
                            .unwrap();

                            data_dir
                                .add_entry_may_overwrite(
                                    "fxfs_root_dir",
                                    remote_dir(root_dir_for_starnix),
                                    true,
                                )
                                .unwrap();
                            responder
                                .send(Ok(root_dir_for_main.into_client_end().unwrap()))
                                .unwrap();
                        }
                        TestFxfsAdminRequest::Shutdown { responder } => {
                            responder.send().unwrap();
                            break 'outer;
                        }
                    }
                }
            }
        }
    }
    fs.shutdown().await;
}
