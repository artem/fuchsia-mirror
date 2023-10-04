// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Result},
    fidl_fuchsia_sys2::StorageAdminProxy,
};

/// Delete the contents of all the storage of this component.
///
/// # Arguments
/// * `storage_admin`: The StorageAdminProxy
/// * `moniker`: The moniker for the target component
pub async fn delete_all(storage_admin: StorageAdminProxy, moniker: String) -> Result<()> {
    storage_admin
        .delete_component_storage(&moniker)
        .await?
        .map_err(|e| anyhow!("Could not delete storage contents of this component: {:?}", e))?;

    println!("Deleted storage contents of component");
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use {
        super::*,
        fidl::endpoints::{create_proxy_and_stream, Proxy},
        fidl_fuchsia_sys2 as fsys,
        futures::TryStreamExt,
    };

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_delete_all() -> Result<()> {
        let (storage_admin_proxy, mut stream) =
            create_proxy_and_stream::<<StorageAdminProxy as Proxy>::Protocol>()
                .expect("create proxy and stream failed");
        // Setup fake admin
        fuchsia_async::Task::local(async move {
            let request = stream.try_next().await;
            if let Ok(Some(fsys::StorageAdminRequest::DeleteComponentStorage {
                relative_moniker,
                responder,
                ..
            })) = request
            {
                if relative_moniker == "foo" {
                    responder.send(Ok(())).unwrap();
                } else {
                    panic!(
                        "couldn't parse string as moniker for storage admin protocol: {:?}",
                        relative_moniker
                    );
                }
            } else {
                panic!("did not get delete component storage request: {:?}", request)
            }
        })
        .detach();
        delete_all(storage_admin_proxy, "foo".to_string())
            .await
            .expect("delete component storage failed");
        Ok(())
    }
}
