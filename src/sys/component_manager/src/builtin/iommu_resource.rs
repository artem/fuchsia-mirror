// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    fidl_fuchsia_kernel as fkernel,
    fuchsia_zircon::{self as zx, HandleBased, Resource},
    futures::prelude::*,
    std::sync::Arc,
};

/// An implementation of fuchsia.kernel.IommuResource protocol.
pub struct IommuResource {
    resource: Resource,
}

impl IommuResource {
    /// `resource` must be the Iommu resource.
    pub fn new(resource: Resource) -> Result<Arc<Self>, Error> {
        let resource_info = resource.info()?;
        if resource_info.kind != zx::sys::ZX_RSRC_KIND_SYSTEM
            || resource_info.base != zx::sys::ZX_RSRC_SYSTEM_IOMMU_BASE
            || resource_info.size != 1
        {
            return Err(format_err!("Iommu resource not available."));
        }
        Ok(Arc::new(Self { resource }))
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fkernel::IommuResourceRequestStream,
    ) -> Result<(), Error> {
        while let Some(fkernel::IommuResourceRequest::Get { responder }) = stream.try_next().await?
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

    async fn get_iommu_resource() -> Result<Resource, Error> {
        let iommu_resource_provider = connect_to_protocol::<fkernel::IommuResourceMarker>()?;
        let iommu_resource_handle = iommu_resource_provider.get().await?;
        Ok(Resource::from(iommu_resource_handle))
    }

    async fn serve_iommu_resource() -> Result<fkernel::IommuResourceProxy, Error> {
        let iommu_resource = get_iommu_resource().await?;

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::IommuResourceMarker>()?;
        fasync::Task::local(
            IommuResource::new(iommu_resource)
                .unwrap_or_else(|e| panic!("Error while creating iommu resource service: {}", e))
                .serve(stream)
                .unwrap_or_else(|e| panic!("Error while serving IOMMU resource service: {}", e)),
        )
        .detach();
        Ok(proxy)
    }

    #[fuchsia::test]
    async fn base_type_is_iommu() -> Result<(), Error> {
        let iommu_resource_provider = serve_iommu_resource().await?;
        let iommu_resource: Resource = iommu_resource_provider.get().await?;
        let resource_info = iommu_resource.info()?;
        assert_eq!(resource_info.kind, zx::sys::ZX_RSRC_KIND_SYSTEM);
        assert_eq!(resource_info.base, zx::sys::ZX_RSRC_SYSTEM_IOMMU_BASE);
        assert_eq!(resource_info.size, 1);
        Ok(())
    }
}
