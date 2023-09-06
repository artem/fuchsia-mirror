// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl_fuchsia_boot as fboot,
    fuchsia_zircon::{self as zx, DebugLog, DebugLogOpts, HandleBased, Resource},
    futures::prelude::*,
    std::sync::Arc,
};

/// An implementation of the `fuchsia.boot.ReadOnlyLog` protocol.
pub struct ReadOnlyLog {
    resource: Resource,
}

impl ReadOnlyLog {
    /// Create a service to provide a read-only version of the kernel log.
    /// Note that the root resource is passed in here, rather than a read-only log handle to be
    /// duplicated, because a fresh debuglog (LogDispatcher) object needs to be returned for each call.
    /// This is because LogDispatcher holds the implicit read location for reading from the log, so if a
    /// handle to the same object was duplicated, this would mistakenly share read location amongst all
    /// retrievals.
    pub fn new(resource: Resource) -> Arc<Self> {
        Arc::new(Self { resource })
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fboot::ReadOnlyLogRequestStream,
    ) -> Result<(), Error> {
        while let Some(fboot::ReadOnlyLogRequest::Get { responder }) = stream.try_next().await? {
            let debuglog = DebugLog::create(&self.resource, DebugLogOpts::READABLE)?;
            // If `READABLE` is set, then the rights for the debuglog include `READ`. However, we
            // must remove the `WRITE` right, as we want to provide a read-only debuglog.
            let readonly = debuglog
                .replace_handle(zx::Rights::BASIC | zx::Rights::READ | zx::Rights::SIGNAL)?;
            responder.send(readonly)?;
        }
        Ok(())
    }
}

/// An implementation of the `fuchsia.boot.WriteOnlyLog` protocol.
pub struct WriteOnlyLog {
    debuglog: DebugLog,
}

impl WriteOnlyLog {
    pub fn new(debuglog: DebugLog) -> Arc<Self> {
        Arc::new(Self { debuglog })
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fboot::WriteOnlyLogRequestStream,
    ) -> Result<(), Error> {
        while let Some(fboot::WriteOnlyLogRequest::Get { responder }) = stream.try_next().await? {
            responder.send(self.debuglog.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, fuchsia_async as fasync, fuchsia_component::client::connect_to_protocol,
        fuchsia_zircon::AsHandleRef,
    };

    async fn get_root_resource() -> Result<zx::Resource, Error> {
        let root_resource_provider = connect_to_protocol::<fboot::RootResourceMarker>()?;
        let root_resource_handle = root_resource_provider.get().await?;
        Ok(zx::Resource::from(root_resource_handle))
    }

    #[fuchsia::test]
    async fn has_correct_rights_for_read_only() -> Result<(), Error> {
        let resource = get_root_resource().await?;
        let read_only_log = ReadOnlyLog::new(resource);
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fboot::ReadOnlyLogMarker>()?;
        fasync::Task::local(
            read_only_log
                .serve(stream)
                .unwrap_or_else(|err| panic!("Error serving read-only log: {}", err)),
        )
        .detach();

        let read_only_log = proxy.get().await?;
        let info = zx::Handle::from(read_only_log).basic_info()?;
        assert_eq!(info.rights, zx::Rights::BASIC | zx::Rights::READ | zx::Rights::SIGNAL);
        Ok(())
    }

    #[fuchsia::test]
    async fn has_correct_rights_for_write_only() -> Result<(), Error> {
        // The kernel requires a valid `Resource` to be provided when creating a `Debuglog` that
        // can be read from, but not one that can be written to.  This may change in the future.
        let resource = Resource::from(zx::Handle::invalid());
        let write_only_log =
            WriteOnlyLog::new(zx::DebugLog::create(&resource, zx::DebugLogOpts::empty()).unwrap());
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fboot::WriteOnlyLogMarker>()?;
        fasync::Task::local(
            write_only_log
                .serve(stream)
                .unwrap_or_else(|err| panic!("Error serving write-only log: {}", err)),
        )
        .detach();

        let write_only_log = proxy.get().await?;
        let info = zx::Handle::from(write_only_log).basic_info()?;
        assert_eq!(info.rights, zx::Rights::BASIC | zx::Rights::WRITE | zx::Rights::SIGNAL);

        Ok(())
    }
}
