// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::inspect::container::InspectHandle,
    diagnostics_data::InspectHandleName,
    fidl::endpoints::{DiscoverableProtocolMarker, Proxy},
    fidl_fuchsia_inspect::{TreeMarker, TreeProxy},
    fidl_fuchsia_inspect_deprecated::{InspectMarker, InspectProxy},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::stream::StreamExt,
    pin_utils::pin_mut,
    std::collections::HashMap,
    tracing::error,
};

/// Mapping from a diagnostics filename to the underlying encoding of that
/// diagnostics data.
pub type DataMap = HashMap<Option<InspectHandleName>, InspectData>;

/// Data associated with a component.
/// This data is stored by data collectors and passed by the collectors to processors.
#[derive(Debug)]
pub enum InspectData {
    /// A VMO containing data associated with the event.
    Vmo(zx::Vmo),

    /// A file containing data associated with the event.
    ///
    /// Because we can't synchronously retrieve file contents like we can for VMOs, this holds
    /// the full file contents. Future changes should make streaming ingestion feasible.
    File(Vec<u8>),

    /// A connection to a Tree service.
    Tree(TreeProxy),

    /// A connection to the deprecated Inspect service.
    DeprecatedFidl(InspectProxy),
}

fn maybe_load_service<P: DiscoverableProtocolMarker>(
    dir_proxy: &fio::DirectoryProxy,
    entry: &fuchsia_fs::directory::DirEntry,
) -> Result<Option<P::Proxy>, anyhow::Error> {
    if entry.name.ends_with(P::PROTOCOL_NAME) {
        let (proxy, server) = fidl::endpoints::create_proxy::<P>()?;
        fdio::service_connect_at(
            dir_proxy.as_channel().as_ref(),
            &entry.name,
            server.into_channel(),
        )?;
        return Ok(Some(proxy));
    }
    Ok(None)
}

pub async fn populate_data_map(inspect_handles: &[InspectHandle]) -> DataMap {
    let mut data_map = DataMap::new();
    for inspect_handle in inspect_handles {
        match inspect_handle {
            InspectHandle::Directory(ref dir) => return populate_data_map_from_dir(dir).await,
            InspectHandle::Tree(proxy, ref name) => {
                data_map.insert(name.clone(), InspectData::Tree(proxy.clone()));
            }
        }
    }

    data_map
}

/// Searches the directory specified by inspect_directory_proxy for
/// .inspect files and populates the `inspect_data_map` with the found VMOs.
async fn populate_data_map_from_dir(inspect_proxy: &fio::DirectoryProxy) -> DataMap {
    // TODO(https://fxbug.dev/42112326): Use a streaming and bounded readdir API when available to avoid
    // being hung.
    let entries = fuchsia_fs::directory::readdir_recursive(inspect_proxy, /* timeout= */ None)
        .filter_map(|result| {
            async move {
                // TODO(https://fxbug.dev/42126094): decide how to show directories that we failed to read.
                result.ok()
            }
        });
    let mut data_map = DataMap::new();
    pin_mut!(entries);
    // TODO(https://fxbug.dev/42138410) convert this async loop to a stream so we can carry backpressure
    while let Some(entry) = entries.next().await {
        // We are only currently interested in inspect VMO files (root.inspect) and
        // inspect services.
        if let Ok(Some(proxy)) = maybe_load_service::<TreeMarker>(inspect_proxy, &entry) {
            data_map
                .insert(Some(InspectHandleName::filename(entry.name)), InspectData::Tree(proxy));
            continue;
        }

        if let Ok(Some(proxy)) = maybe_load_service::<InspectMarker>(inspect_proxy, &entry) {
            data_map.insert(
                Some(InspectHandleName::filename(entry.name)),
                InspectData::DeprecatedFidl(proxy),
            );
            continue;
        }

        if !entry.name.ends_with(".inspect")
            || entry.kind != fuchsia_fs::directory::DirentKind::File
        {
            continue;
        }

        let file_proxy = match fuchsia_fs::directory::open_file_no_describe(
            inspect_proxy,
            &entry.name,
            fuchsia_fs::OpenFlags::RIGHT_READABLE,
        ) {
            Ok(proxy) => proxy,
            Err(_) => {
                // It should be ok to not be able to read a file. The file might be closed by the
                // time we get here.
                continue;
            }
        };

        // Obtain the backing vmo.
        let vmo = match file_proxy.get_backing_memory(fio::VmoFlags::READ).await {
            Ok(vmo) => vmo,
            Err(_) => {
                // It should be ok to not be able to read a file. The file might be closed by the
                // time we get here.
                continue;
            }
        };

        let data = match vmo.map_err(zx::Status::from_raw) {
            Ok(vmo) => InspectData::Vmo(vmo),
            Err(err) => {
                match err {
                    zx::Status::NOT_SUPPORTED => {}
                    err => {
                        error!(
                            file = %entry.name, ?err,
                            "unexpected error from GetBackingMemory",
                        )
                    }
                }
                match fuchsia_fs::file::read(&file_proxy).await {
                    Ok(contents) => InspectData::File(contents),
                    Err(_) => {
                        // It should be ok to not be able to read a file. The file might be closed
                        // by the time we get here.
                        continue;
                    }
                }
            }
        };
        data_map.insert(Some(InspectHandleName::filename(entry.name)), data);
    }

    data_map
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        diagnostics_assertions::assert_data_tree,
        fidl::endpoints::create_request_stream,
        fuchsia_async as fasync,
        fuchsia_component::server::ServiceFs,
        fuchsia_inspect::{reader, Inspector},
        fuchsia_zircon as zx,
        fuchsia_zircon::Peered,
        inspect_runtime::{service::spawn_tree_server_with_stream, TreeServerSendPreference},
    };

    fn get_vmo(text: &[u8]) -> zx::Vmo {
        let vmo = zx::Vmo::create(4096).unwrap();
        vmo.write(text, 0).unwrap();
        vmo
    }

    #[fuchsia::test]
    async fn populate_data_map_with_trees() {
        let insp1 = Inspector::default();
        let insp2 = Inspector::default();
        let insp3 = Inspector::default();

        insp1.root().record_int("one", 1);
        insp2.root().record_int("two", 2);
        insp3.root().record_int("three", 3);

        let (tree1, request_stream) = create_request_stream::<TreeMarker>().unwrap();
        spawn_tree_server_with_stream(insp1, TreeServerSendPreference::default(), request_stream)
            .detach();
        let (tree2, request_stream) = create_request_stream::<TreeMarker>().unwrap();
        spawn_tree_server_with_stream(insp2, TreeServerSendPreference::default(), request_stream)
            .detach();
        let (tree3, request_stream) = create_request_stream::<TreeMarker>().unwrap();
        spawn_tree_server_with_stream(insp3, TreeServerSendPreference::default(), request_stream)
            .detach();

        let name1 = Some(InspectHandleName::name("tree1"));
        let name2 = Some(InspectHandleName::name("tree2"));
        let name3 = None;

        let data = populate_data_map(&[
            InspectHandle::Tree(tree1.into_proxy().unwrap(), name1.clone()),
            InspectHandle::Tree(tree2.into_proxy().unwrap(), name2.clone()),
            InspectHandle::Tree(tree3.into_proxy().unwrap(), name3.clone()),
        ])
        .await;

        assert_eq!(data.len(), 3);

        assert_matches!(data.get(&name1), Some(InspectData::Tree(t)) => {
            let h = reader::read(t).await.unwrap();
            assert_data_tree!(h, root: {
                one: 1i64,
            });
        });
        assert_matches!(data.get(&name2), Some(InspectData::Tree(t)) => {
            let h = reader::read(t).await.unwrap();
            assert_data_tree!(h, root: {
                two: 2i64,
            });
        });
        assert_matches!(data.get(&name3), Some(InspectData::Tree(t)) => {
            let h = reader::read(t).await.unwrap();
            assert_data_tree!(h, root: {
                three: 3i64,
            });
        });
    }

    #[fuchsia::test]
    async fn inspect_data_collector() {
        let path = "/test-bindings/out";
        // Make a ServiceFs containing two files.
        // One is an inspect file, and one is not.
        let mut fs = ServiceFs::new();
        let vmo = get_vmo(b"test1");
        let vmo2 = get_vmo(b"test2");
        let vmo3 = get_vmo(b"test3");
        let vmo4 = get_vmo(b"test4");
        fs.dir("diagnostics").add_vmo_file_at("root.inspect", vmo);
        fs.dir("diagnostics").add_vmo_file_at("root_not_inspect", vmo2);
        fs.dir("diagnostics").dir("a").add_vmo_file_at("root.inspect", vmo3);
        fs.dir("diagnostics").dir("b").add_vmo_file_at("root.inspect", vmo4);
        // Create a connection to the ServiceFs.
        let (h0, h1) = fidl::endpoints::create_endpoints();
        fs.serve_connection(h1).unwrap();

        let ns = fdio::Namespace::installed().unwrap();
        ns.bind(path, h0).unwrap();

        fasync::Task::spawn(fs.collect()).detach();

        let (done0, done1) = zx::Channel::create();

        // Run the actual test in a separate thread so that it does not block on FS operations.
        // Use signalling on a zx::Channel to indicate that the test is done.
        std::thread::spawn(move || {
            let done = done1;
            let mut executor = fasync::LocalExecutor::new();

            executor.run_singlethreaded(async {
                let extra_data = collect(&format!("{path}/diagnostics")).await;
                assert_eq!(3, extra_data.len());

                let assert_extra_data = |path: &str, content: &[u8]| {
                    let extra = extra_data.get(&Some(InspectHandleName::filename(path)));
                    assert!(extra.is_some());

                    match extra.unwrap() {
                        InspectData::Vmo(vmo) => {
                            let mut buf = [0u8; 5];
                            vmo.read(&mut buf, 0).expect("reading vmo");
                            assert_eq!(content, &buf);
                        }
                        v => {
                            panic!("Expected Vmo, got {v:?}");
                        }
                    }
                };

                assert_extra_data("root.inspect", b"test1");
                assert_extra_data("a/root.inspect", b"test3");
                assert_extra_data("b/root.inspect", b"test4");

                done.signal_peer(zx::Signals::NONE, zx::Signals::USER_0).expect("signalling peer");
            });
        });

        fasync::OnSignals::new(&done0, zx::Signals::USER_0).await.unwrap();
        ns.unbind(path).unwrap();
    }

    async fn collect(path: &str) -> DataMap {
        let inspect_proxy =
            fuchsia_fs::directory::open_in_namespace(path, fuchsia_fs::OpenFlags::RIGHT_READABLE)
                .expect("Failed to open directory");
        populate_data_map(&[inspect_proxy.into()]).await
    }
}
