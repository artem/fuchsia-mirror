// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    fidl_fuchsia_kernel as fkernel,
    fuchsia_zircon::{self as zx, HandleBased, Resource},
    futures::prelude::*,
    std::sync::Arc,
};

/// An implementation of fuchsia.kernel.HypervisorResource protocol.
pub struct HypervisorResource {
    resource: Resource,
}

impl HypervisorResource {
    /// `resource` must be the Hypervisor resource.
    pub fn new(resource: Resource) -> Result<Arc<Self>, Error> {
        let resource_info = resource.info()?;
        if resource_info.kind != zx::sys::ZX_RSRC_KIND_SYSTEM
            || resource_info.base != zx::sys::ZX_RSRC_SYSTEM_HYPERVISOR_BASE
            || resource_info.size != 1
        {
            return Err(format_err!("Hypervisor resource not available."));
        }
        Ok(Arc::new(Self { resource }))
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fkernel::HypervisorResourceRequestStream,
    ) -> Result<(), Error> {
        while let Some(fkernel::HypervisorResourceRequest::Get { responder }) =
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

    async fn get_hypervisor_resource() -> Result<Resource, Error> {
        let hypervisor_resource_provider =
            connect_to_protocol::<fkernel::HypervisorResourceMarker>()?;
        let hypervisor_resource_handle = hypervisor_resource_provider.get().await?;
        Ok(Resource::from(hypervisor_resource_handle))
    }

    async fn serve_hypervisor_resource() -> Result<fkernel::HypervisorResourceProxy, Error> {
        let hypervisor_resource = get_hypervisor_resource().await?;

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::HypervisorResourceMarker>()?;
        fasync::Task::local(
            HypervisorResource::new(hypervisor_resource)
                .unwrap_or_else(|e| {
                    panic!("Error while creating hypervisor resource service: {}", e)
                })
                .serve(stream)
                .unwrap_or_else(|e| {
                    panic!("Error while serving HYPERVISOR resource service: {}", e)
                }),
        )
        .detach();
        Ok(proxy)
    }

    #[fuchsia::test]
    async fn kind_type_is_hypervisor() -> Result<(), Error> {
        let hypervisor_resource_provider = serve_hypervisor_resource().await?;
        let hypervisor_resource: Resource = hypervisor_resource_provider.get().await?;
        let resource_info = hypervisor_resource.info()?;
        assert_eq!(resource_info.kind, zx::sys::ZX_RSRC_KIND_SYSTEM);
        assert_eq!(resource_info.base, zx::sys::ZX_RSRC_SYSTEM_HYPERVISOR_BASE);
        assert_eq!(resource_info.size, 1);
        Ok(())
    }
}
