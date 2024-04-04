// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::run_events::{RunEvent, SuiteEvents},
    anyhow::Error,
    fidl::endpoints::{create_proxy, create_request_stream, Proxy},
    fidl_fuchsia_io as fio, fidl_fuchsia_test_manager as ftest_manager, fuchsia_async as fasync,
    futures::{channel::mpsc, pin_mut, prelude::*, stream::FusedStream, StreamExt, TryStreamExt},
    tracing::warn,
};

const DEBUG_DATA_TIMEOUT_SECONDS: i64 = 15;
const EARLY_BOOT_DEBUG_DATA_PATH: &'static str = "/debugdata";

pub(crate) async fn send_kernel_debug_data(
    iterator: ftest_manager::DebugDataIteratorRequestStream,
) -> Result<(), Error> {
    tracing::info!("Serving kernel debug data");
    let directory = fuchsia_fs::directory::open_in_namespace(
        EARLY_BOOT_DEBUG_DATA_PATH,
        fuchsia_fs::OpenFlags::RIGHT_READABLE,
    )?;

    serve_iterator(EARLY_BOOT_DEBUG_DATA_PATH, directory, iterator).await
}

const ITERATOR_BATCH_SIZE: usize = 10;

async fn filter_map_filename(
    entry_result: Result<
        fuchsia_fs::directory::DirEntry,
        fuchsia_fs::directory::RecursiveEnumerateError,
    >,
    dir_path: &str,
) -> Option<String> {
    match entry_result {
        Ok(fuchsia_fs::directory::DirEntry { name, kind }) => match kind {
            fuchsia_fs::directory::DirentKind::File => Some(name),
            _ => None,
        },
        Err(e) => {
            warn!("Error reading directory in {}: {:?}", dir_path, e);
            None
        }
    }
}

async fn serve_file_over_socket(file: fio::FileProxy, socket: fuchsia_zircon::Socket) {
    let mut socket = fasync::Socket::from_socket(socket);

    // We keep a buffer of 4.8 MB while reading the file
    let num_bytes: u64 = 1024 * 48;
    let (mut sender, mut recv) = mpsc::channel(100);
    let _file_read_task = fasync::Task::spawn(async move {
        loop {
            let bytes = fuchsia_fs::file::read_num_bytes(&file, num_bytes).await.unwrap();
            let len = bytes.len();
            if let Err(_) = sender.send(bytes).await {
                // no recv, don't read rest of the file.
                break;
            }
            if len != usize::try_from(num_bytes).unwrap() {
                // done reading file
                break;
            }
        }
    });

    while let Some(bytes) = recv.next().await {
        if let Err(e) = socket.write_all(bytes.as_slice()).await {
            warn!("cannot serve file: {:?}", e);
            return;
        }
    }
}

pub(crate) async fn serve_directory(
    dir_path: &str,
    mut event_sender: mpsc::Sender<RunEvent>,
) -> Result<(), Error> {
    let directory =
        fuchsia_fs::directory::open_in_namespace(dir_path, fuchsia_fs::OpenFlags::RIGHT_READABLE)?;
    {
        let file_stream = fuchsia_fs::directory::readdir_recursive(
            &directory,
            Some(fasync::Duration::from_seconds(DEBUG_DATA_TIMEOUT_SECONDS)),
        )
        .filter_map(|entry| filter_map_filename(entry, dir_path));
        pin_mut!(file_stream);
        if file_stream.next().await.is_none() {
            // No files to serve.
            return Ok(());
        }

        drop(file_stream);
    }

    let (client, iterator) = create_request_stream::<ftest_manager::DebugDataIteratorMarker>()?;
    let _ = event_sender.send(RunEvent::debug_data(client).into()).await;
    event_sender.disconnect(); // No need to hold this open while we serve the iterator.

    serve_iterator(dir_path, directory, iterator).await
}

pub(crate) async fn serve_directory_for_suite(
    dir_path: &str,
    mut event_sender: mpsc::Sender<Result<SuiteEvents, ftest_manager::LaunchError>>,
) -> Result<(), Error> {
    let directory =
        fuchsia_fs::directory::open_in_namespace(dir_path, fuchsia_fs::OpenFlags::RIGHT_READABLE)?;
    {
        let file_stream = fuchsia_fs::directory::readdir_recursive(
            &directory,
            Some(fasync::Duration::from_seconds(DEBUG_DATA_TIMEOUT_SECONDS)),
        )
        .filter_map(|entry| filter_map_filename(entry, dir_path));
        pin_mut!(file_stream);
        if file_stream.next().await.is_none() {
            // No files to serve.
            return Ok(());
        }

        drop(file_stream);
    }

    let (client, iterator) = create_request_stream::<ftest_manager::DebugDataIteratorMarker>()?;
    let _ = event_sender.send(Ok(SuiteEvents::debug_data(client).into())).await;
    event_sender.disconnect(); // No need to hold this open while we serve the iterator.

    serve_iterator(dir_path, directory, iterator).await
}

/// Serves the |DebugDataIterator| protocol by serving all the files contained under
/// |dir_path|.
///
/// The contents under |dir_path| are assumed to not change while the iterator is served.
pub(crate) async fn serve_iterator(
    dir_path: &str,
    directory: fio::DirectoryProxy,
    mut iterator: ftest_manager::DebugDataIteratorRequestStream,
) -> Result<(), Error> {
    let file_stream = fuchsia_fs::directory::readdir_recursive(
        &directory,
        Some(fasync::Duration::from_seconds(DEBUG_DATA_TIMEOUT_SECONDS)),
    )
    .filter_map(|entry| filter_map_filename(entry, dir_path));
    pin_mut!(file_stream);
    let mut file_stream = file_stream.fuse();

    let mut file_tasks = vec![];
    while let Some(request) = iterator.try_next().await? {
        let ftest_manager::DebugDataIteratorRequest::GetNext { responder } = request;
        let next_files = match file_stream.is_terminated() {
            true => vec![],
            false => file_stream.by_ref().take(ITERATOR_BATCH_SIZE).collect().await,
        };
        let debug_data = next_files
            .into_iter()
            .map(|file_name| {
                let (file, server) = create_proxy::<fio::NodeMarker>().unwrap();
                let file = fio::FileProxy::new(file.into_channel().unwrap());
                directory.open(
                    fuchsia_fs::OpenFlags::RIGHT_READABLE,
                    fio::ModeType::empty(),
                    &file_name,
                    server,
                )?;

                tracing::info!("Serving debug data file {}: {}", dir_path, file_name);
                let (client, server) = fuchsia_zircon::Socket::create_stream();
                let t = fasync::Task::spawn(serve_file_over_socket(file, server));
                file_tasks.push(t);
                Ok(ftest_manager::DebugData {
                    socket: Some(client.into()),
                    name: file_name.into(),
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let _ = responder.send(debug_data);
    }

    // make sure all tasks complete
    future::join_all(file_tasks).await;
    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::run_events::{RunEventPayload, SuiteEventPayload},
        fuchsia_async as fasync,
        std::collections::HashSet,
        tempfile::tempdir,
        test_diagnostics::collect_string_from_socket,
    };

    async fn serve_iterator_from_tmp(
        dir: &tempfile::TempDir,
    ) -> (Option<ftest_manager::DebugDataIteratorProxy>, fasync::Task<Result<(), Error>>) {
        let (send, mut recv) = mpsc::channel(0);
        let dir_path = dir.path().to_str().unwrap().to_string();
        let task = fasync::Task::local(async move { serve_directory(&dir_path, send).await });
        let proxy = recv.next().await.map(|event| {
            let RunEventPayload::DebugData(client) = event.into_payload();
            client.into_proxy().expect("into proxy")
        });
        (proxy, task)
    }

    #[fuchsia::test]
    async fn serve_iterator_empty_dir_returns_no_client() {
        let dir = tempdir().unwrap();
        let (client, task) = serve_iterator_from_tmp(&dir).await;
        assert!(client.is_none());
        task.await.expect("iterator server should not fail");
    }

    #[fuchsia::test]
    async fn serve_iterator_single_response() {
        let dir = tempdir().unwrap();
        fuchsia_fs::file::write_in_namespace(&dir.path().join("file").to_string_lossy(), "test")
            .await
            .expect("write to file");

        let (client, task) = serve_iterator_from_tmp(&dir).await;

        let proxy = client.expect("client to be returned");

        let mut values = proxy.get_next().await.expect("get next");
        assert_eq!(1usize, values.len());
        let ftest_manager::DebugData { name, socket, .. } = values.pop().unwrap();
        assert_eq!(Some("file".to_string()), name);
        let contents = collect_string_from_socket(socket.unwrap()).await.expect("read socket");
        assert_eq!("test", contents);

        let values = proxy.get_next().await.expect("get next");
        assert_eq!(values, vec![]);

        // Calling again is okay and should also return empty vector.
        let values = proxy.get_next().await.expect("get next");
        assert_eq!(values, vec![]);

        drop(proxy);
        task.await.expect("iterator server should not fail");
    }

    #[fuchsia::test]
    async fn serve_iterator_multiple_responses() {
        let num_files_served = ITERATOR_BATCH_SIZE * 2;

        let dir = tempdir().unwrap();
        for idx in 0..num_files_served {
            fuchsia_fs::file::write_in_namespace(
                &dir.path().join(format!("file-{:?}", idx)).to_string_lossy(),
                &format!("test-{:?}", idx),
            )
            .await
            .expect("write to file");
        }

        let (client, task) = serve_iterator_from_tmp(&dir).await;

        let proxy = client.expect("client to be returned");

        let mut all_files = vec![];
        loop {
            let mut next = proxy.get_next().await.expect("get next");
            if next.is_empty() {
                break;
            }
            all_files.append(&mut next);
        }

        let file_contents: HashSet<_> = futures::stream::iter(all_files)
            .then(|ftest_manager::DebugData { name, socket, .. }| async move {
                let contents =
                    collect_string_from_socket(socket.unwrap()).await.expect("read socket");
                (name.unwrap(), contents)
            })
            .collect()
            .await;

        let expected_files: HashSet<_> = (0..num_files_served)
            .map(|idx| (format!("file-{:?}", idx), format!("test-{:?}", idx)))
            .collect();

        assert_eq!(file_contents, expected_files);
        drop(proxy);
        task.await.expect("iterator server should not fail");
    }

    async fn serve_iterator_for_suite_from_tmp(
        dir: &tempfile::TempDir,
    ) -> (Option<ftest_manager::DebugDataIteratorProxy>, fasync::Task<Result<(), Error>>) {
        let (send, mut recv) = mpsc::channel(0);
        let dir_path = dir.path().to_str().unwrap().to_string();
        let task =
            fasync::Task::local(async move { serve_directory_for_suite(&dir_path, send).await });
        let proxy = recv.next().await.map(|event| {
            if let SuiteEventPayload::DebugData(client) = event.unwrap().into_payload() {
                Some(client.into_proxy().expect("into proxy"))
            } else {
                None // Event is not a DebugData
            }
            .unwrap()
        });
        (proxy, task)
    }

    #[fuchsia::test]
    async fn serve_iterator_for_suite_empty_dir_returns_no_client() {
        let dir = tempdir().unwrap();
        let (client, task) = serve_iterator_for_suite_from_tmp(&dir).await;
        assert!(client.is_none());
        task.await.expect("iterator server should not fail");
    }

    #[fuchsia::test]
    async fn serve_iterator_for_suite_single_response() {
        let dir = tempdir().unwrap();
        fuchsia_fs::file::write_in_namespace(&dir.path().join("file").to_string_lossy(), "test")
            .await
            .expect("write to file");

        let (client, task) = serve_iterator_for_suite_from_tmp(&dir).await;

        let proxy = client.expect("client to be returned");

        let mut values = proxy.get_next().await.expect("get next");
        assert_eq!(1usize, values.len());
        let ftest_manager::DebugData { name, socket, .. } = values.pop().unwrap();
        assert_eq!(Some("file".to_string()), name);
        let contents = collect_string_from_socket(socket.unwrap()).await.expect("read socket");
        assert_eq!("test", contents);

        let values = proxy.get_next().await.expect("get next");
        assert_eq!(values, vec![]);

        // Calling again is okay and should also return empty vector.
        let values = proxy.get_next().await.expect("get next");
        assert_eq!(values, vec![]);

        drop(proxy);
        task.await.expect("iterator server should not fail");
    }

    #[fuchsia::test]
    async fn serve_iterator_for_suite_multiple_responses() {
        let num_files_served = ITERATOR_BATCH_SIZE * 2;

        let dir = tempdir().unwrap();
        for idx in 0..num_files_served {
            fuchsia_fs::file::write_in_namespace(
                &dir.path().join(format!("file-{:?}", idx)).to_string_lossy(),
                &format!("test-{:?}", idx),
            )
            .await
            .expect("write to file");
        }

        let (client, task) = serve_iterator_from_tmp(&dir).await;

        let proxy = client.expect("client to be returned");

        let mut all_files = vec![];
        loop {
            let mut next = proxy.get_next().await.expect("get next");
            if next.is_empty() {
                break;
            }
            all_files.append(&mut next);
        }

        let file_contents: HashSet<_> = futures::stream::iter(all_files)
            .then(|ftest_manager::DebugData { name, socket, .. }| async move {
                let contents =
                    collect_string_from_socket(socket.unwrap()).await.expect("read socket");
                (name.unwrap(), contents)
            })
            .collect()
            .await;

        let expected_files: HashSet<_> = (0..num_files_served)
            .map(|idx| (format!("file-{:?}", idx), format!("test-{:?}", idx)))
            .collect();

        assert_eq!(file_contents, expected_files);
        drop(proxy);
        task.await.expect("iterator server should not fail");
    }
}
