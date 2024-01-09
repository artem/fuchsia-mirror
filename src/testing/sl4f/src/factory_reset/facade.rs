// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common_utils::common::macros::{fx_err_and_bail, with_line};
use crate::common_utils::common::LazyProxy;
use anyhow::Error;
use fidl_fuchsia_recovery::{FactoryResetMarker, FactoryResetProxy};
use tracing::info;

/// Perform factory reset fidl operations.
///
/// Note this object is shared among all threads created by server.
///
#[derive(Debug)]
pub struct FactoryResetFacade {
    factory_reset_manager: LazyProxy<FactoryResetMarker>,
}

impl FactoryResetFacade {
    pub fn new() -> FactoryResetFacade {
        FactoryResetFacade { factory_reset_manager: Default::default() }
    }

    /// Returns the proxy provided on instantiation or establishes a new connection.
    fn factory_reset_manager(&self) -> Result<FactoryResetProxy, Error> {
        self.factory_reset_manager.get_or_connect()
    }

    /// Returns the pairing code from the FactoryDataManager proxy service.
    pub async fn factory_reset(&self) -> Result<(), Error> {
        let tag = "FactoryResetFacade::factory_reset";
        info!("Executing factory reset");
        match self.factory_reset_manager()?.reset().await {
            Ok(_) => Ok(()),
            Err(e) => fx_err_and_bail!(
                &with_line!(tag),
                format_err!("FIDL call failed with error: {}", e)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_recovery::FactoryResetRequest;
    use fuchsia_async as fasync;
    use futures::prelude::*;
    use lazy_static::lazy_static;

    lazy_static! {
        static ref RESULT: i32 = 0;
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_factory_reset() {
        let (proxy, mut stream) = create_proxy_and_stream::<FactoryResetMarker>().unwrap();
        let facade = FactoryResetFacade::new();
        facade.factory_reset_manager.set(proxy).unwrap();
        let facade_fut = async move {
            assert_eq!(facade.factory_reset().await.ok(), Some(()));
        };
        let stream_fut = async move {
            match stream.try_next().await {
                Ok(Some(FactoryResetRequest::Reset { responder })) => {
                    responder.send((*RESULT).clone()).unwrap();
                }
                err => panic!("Error in request handler: {:?}", err),
            }
        };
        future::join(facade_fut, stream_fut).await;
    }
}
