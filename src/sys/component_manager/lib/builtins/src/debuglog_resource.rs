// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    fidl_fuchsia_kernel as fkernel,
    fuchsia_zircon::{self as zx, HandleBased, Resource},
    futures::prelude::*,
    std::sync::Arc,
};

/// An implementation of fuchsia.kernel.DebuglogResource protocol.
pub struct DebuglogResource {
    resource: Resource,
}

impl DebuglogResource {
    /// `resource` must be the Debuglog resource.
    pub fn new(resource: Resource) -> Result<Arc<Self>, Error> {
        let resource_info = resource.info()?;
        if resource_info.kind != zx::sys::ZX_RSRC_KIND_SYSTEM
            || resource_info.base != zx::sys::ZX_RSRC_SYSTEM_DEBUGLOG_BASE
            || resource_info.size != 1
        {
            return Err(format_err!("Debuglog resource not available."));
        }
        Ok(Arc::new(Self { resource }))
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fkernel::DebuglogResourceRequestStream,
    ) -> Result<(), Error> {
        while let Some(fkernel::DebuglogResourceRequest::Get { responder }) =
            stream.try_next().await?
        {
            responder.send(self.resource.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, fidl_fuchsia_kernel as fkernel, fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol,
    };

    async fn get_debuglog_resource() -> Result<Resource, Error> {
        let debuglog_resource_provider = connect_to_protocol::<fkernel::DebuglogResourceMarker>()?;
        let debuglog_resource_handle = debuglog_resource_provider.get().await?;
        Ok(Resource::from(debuglog_resource_handle))
    }

    async fn serve_debuglog_resource() -> Result<fkernel::DebuglogResourceProxy, Error> {
        let debuglog_resource = get_debuglog_resource().await?;

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::DebuglogResourceMarker>()?;
        fasync::Task::local(
            DebuglogResource::new(debuglog_resource)
                .unwrap_or_else(|e| panic!("Error while creating debuglog resource service: {}", e))
                .serve(stream)
                .unwrap_or_else(|e| panic!("Error while serving debuglog resource service: {}", e)),
        )
        .detach();
        Ok(proxy)
    }

    #[fuchsia::test]
    async fn base_type_is_debuglog() -> Result<(), Error> {
        let debuglog_resource_provider = serve_debuglog_resource().await?;
        let debuglog_resource: Resource = debuglog_resource_provider.get().await?;
        let resource_info = debuglog_resource.info()?;
        assert_eq!(resource_info.kind, zx::sys::ZX_RSRC_KIND_SYSTEM);
        assert_eq!(resource_info.base, zx::sys::ZX_RSRC_SYSTEM_DEBUGLOG_BASE);
        assert_eq!(resource_info.size, 1);
        Ok(())
    }
}
