// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error, fidl_fuchsia_kernel as fkernel, fuchsia_runtime::job_default,
    fuchsia_zircon as zx, futures::TryStreamExt,
};

/// An implementation of the `fuchsia.kernel.RootJob` protocol.
pub struct RootJob;

impl RootJob {
    pub async fn serve(
        mut stream: fkernel::RootJobRequestStream,
        rights: zx::Rights,
    ) -> Result<(), Error> {
        let job = job_default();
        while let Some(fkernel::RootJobRequest::Get { responder }) = stream.try_next().await? {
            responder.send(job.duplicate(rights)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, fuchsia_async as fasync, fuchsia_zircon::AsHandleRef, futures::TryFutureExt};

    #[fuchsia::test]
    async fn has_correct_rights() -> Result<(), Error> {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<fkernel::RootJobMarker>()?;
        fasync::Task::local(
            RootJob::serve(stream, zx::Rights::TRANSFER)
                .unwrap_or_else(|err| panic!("Error serving root job: {}", err)),
        )
        .detach();

        let root_job = proxy.get().await?;
        let info = zx::Handle::from(root_job).basic_info()?;
        assert_eq!(info.rights, zx::Rights::TRANSFER);
        Ok(())
    }
}
