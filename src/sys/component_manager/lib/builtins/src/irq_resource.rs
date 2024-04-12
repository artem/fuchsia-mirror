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

/// An implementation of fuchsia.kernel.IrqResource protocol.
pub struct IrqResource {
    resource: Resource,
}

impl IrqResource {
    /// `resource` must be the IRQ resource.
    pub fn new(resource: Resource) -> Arc<Self> {
        Arc::new(Self { resource })
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fkernel::IrqResourceRequestStream,
    ) -> Result<(), Error> {
        if self.resource.info()?.kind != zx::sys::ZX_RSRC_KIND_IRQ {
            return Err(format_err!("invalid handle kind, expected IRQ"));
        }
        while let Some(fkernel::IrqResourceRequest::Get { responder }) = stream.try_next().await? {
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

    async fn get_irq_resource() -> Result<Resource, Error> {
        let irq_resource_provider = connect_to_protocol::<fkernel::IrqResourceMarker>()?;
        let irq_resource_handle = irq_resource_provider.get().await?;
        Ok(Resource::from(irq_resource_handle))
    }

    async fn serve_irq_resource() -> Result<fkernel::IrqResourceProxy, Error> {
        let irq_resource = get_irq_resource().await?;

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::IrqResourceMarker>()?;
        fasync::Task::local(
            IrqResource::new(irq_resource)
                .serve(stream)
                .unwrap_or_else(|e| panic!("Error while serving IRQ resource service: {}", e)),
        )
        .detach();
        Ok(proxy)
    }

    #[fuchsia::test]
    async fn fail_with_no_irq_resource() -> Result<(), Error> {
        let (_, stream) = fidl::endpoints::create_proxy_and_stream::<fkernel::IrqResourceMarker>()?;
        IrqResource::new(Resource::from(zx::Handle::invalid()))
            .serve(stream)
            .await
            .expect_err("should fail to serve stream with an invalid resource");
        Ok(())
    }

    #[fuchsia::test]
    async fn kind_type_is_irq() -> Result<(), Error> {
        let irq_resource_provider = serve_irq_resource().await?;
        let irq_resource: Resource = irq_resource_provider.get().await?;
        let resource_info = irq_resource.info()?;
        assert_eq!(resource_info.kind, zx::sys::ZX_RSRC_KIND_IRQ);
        assert_eq!(resource_info.base, 0);
        assert_eq!(resource_info.size, 0);
        Ok(())
    }
}
