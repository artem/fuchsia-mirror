// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This launches the binary specified in the arguments and takes the block device that is passed via
// the usual startup handle and makes it appear to the child process as an object that will work
// with POSIX I/O.  The object will exist in the child's namespace under /device/block.  At the time
// of writing, this is used to run the fsck-msdosfs and mkfs-msdosfs tools which use POSIX I/O to
// interact with block devices.

use {
    anyhow::Error,
    async_trait::async_trait,
    fidl::endpoints::{create_endpoints, ServerEnd},
    fidl_fuchsia_hardware_block as fhardware_block, fidl_fuchsia_io as fio,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    remote_block_device::{BlockClient as _, BufferSlice, MutableBufferSlice, RemoteBlockClient},
    std::sync::Arc,
    vfs::{
        attributes,
        common::rights_to_posix_mode_bits,
        directory::{
            entry::{DirectoryEntry, EntryInfo, OpenRequest},
            entry_container::Directory,
        },
        execution_scope::ExecutionScope,
        file::{FidlIoConnection, File, FileIo, FileLike, FileOptions, SyncMode},
        node::Node,
        path::Path,
        pseudo_directory, ObjectRequestRef,
    },
};

fn map_to_status(err: Error) -> zx::Status {
    if let Some(&status) = err.root_cause().downcast_ref::<zx::Status>() {
        status
    } else {
        // Print the internal error if we re-map it because we will lose any context after this.
        println!("Internal error: {:?}", err);
        zx::Status::INTERNAL
    }
}

struct BlockFile {
    block_client: RemoteBlockClient,
}

impl DirectoryEntry for BlockFile {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(0, fio::DirentType::File)
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.open_file(self)
    }
}

#[async_trait]
impl Node for BlockFile {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        let block_size = self.block_client.block_size();
        let block_count = self.block_client.block_count();
        let device_size = block_count.checked_mul(block_size.into()).unwrap();
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ true, /*x*/ false),
            id: 0,
            content_size: device_size,
            storage_size: device_size,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let block_size = self.block_client.block_size();
        let block_count = self.block_client.block_count();
        let device_size = block_count.checked_mul(block_size.into()).unwrap();
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
                content_size: device_size,
                storage_size: device_size,
                link_count: 1,
                id: 0,
            }
        ))
    }
}

impl File for BlockFile {
    fn writable(&self) -> bool {
        true
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn truncate(&self, _length: u64) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn get_backing_memory(&self, _flags: fio::VmoFlags) -> Result<zx::Vmo, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn get_size(&self) -> Result<u64, zx::Status> {
        let block_size = self.block_client.block_size();
        let block_count = self.block_client.block_count();
        Ok(block_count.checked_mul(block_size.into()).unwrap())
    }

    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), zx::Status> {
        self.block_client.flush().await.map_err(map_to_status)
    }
}

impl FileIo for BlockFile {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, zx::Status> {
        let () = self
            .block_client
            .read_at(MutableBufferSlice::Memory(buffer), offset)
            .await
            .map_err(map_to_status)?;
        Ok(buffer.len().try_into().unwrap())
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, zx::Status> {
        let () = self
            .block_client
            .write_at(BufferSlice::Memory(content), offset)
            .await
            .map_err(map_to_status)?;
        Ok(content.len().try_into().unwrap())
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

impl FileLike for BlockFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        FidlIoConnection::spawn(scope, self, options, object_request)
    }
}

// This launches the binary specified in the arguments and takes a block device and makes it appear
// to the child process as an object that will work with POSIX I/O.  The object will exist in the
// child's namespace under /device/block.  At the time of writing, this is used to run the
// fsck-msdosfs and mkfs-msdosfs tools which use POSIX I/O to interact with block devices.
pub async fn run(
    block_proxy: fhardware_block::BlockProxy,
    binary: &str,
    args: impl Iterator<Item = String>,
) -> Result<i64, Error> {
    let (client, server) = create_endpoints::<fio::DirectoryMarker>();

    let server_fut = {
        let block_client = RemoteBlockClient::new(block_proxy).await?;
        let dir = pseudo_directory! {
            "block" => Arc::new(BlockFile {
                block_client,
            }),
        };

        let scope = ExecutionScope::new();
        let () = dir.open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
            Path::dot(),
            ServerEnd::new(server.into_channel()),
        );
        async move { scope.wait().await }
    };

    let client_fut = {
        let mut builder = fdio::SpawnBuilder::new()
            .options(fdio::SpawnOptions::CLONE_ALL)
            .add_directory_to_namespace("/device", client)?
            .arg(binary)?;
        for arg in args {
            builder = builder.arg(arg)?;
        }
        builder = builder.arg("/device/block")?;
        let process = builder.spawn_from_path(binary, &fuchsia_runtime::job_default())?;

        async move {
            let _: zx::Signals =
                fasync::OnSignals::new(&process, zx::Signals::PROCESS_TERMINATED).await?;
            let info = process.info()?;
            Ok::<_, Error>(info.return_code)
        }
    };

    futures::future::join(server_fut, client_fut).await.1
}
