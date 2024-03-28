// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::indexer::*,
    crate::resolved_driver::{DriverPackageType, ResolvedDriver},
    anyhow::Context,
    fidl_fuchsia_component_resolution as fresolution,
    std::{collections::HashSet, rc::Rc},
};

fn log_error(err: anyhow::Error) -> anyhow::Error {
    tracing::error!("{:#?}", err);
    err
}

pub async fn load_boot_drivers(
    boot_drivers: &Vec<String>,
    resolver: &fresolution::ResolverProxy,
    eager_drivers: &HashSet<url::Url>,
    disabled_drivers: &HashSet<url::Url>,
) -> Result<Vec<ResolvedDriver>, anyhow::Error> {
    let resolved_drivers = load_drivers(
        boot_drivers,
        &resolver,
        &eager_drivers,
        &disabled_drivers,
        DriverPackageType::Boot,
    )
    .await
    .context("Error loading boot packages")
    .map_err(log_error)?;
    Ok(resolved_drivers)
}

pub async fn load_base_drivers(
    indexer: Rc<Indexer>,
    base_drivers: &Vec<String>,
    resolver: &fresolution::ResolverProxy,
    eager_drivers: &HashSet<url::Url>,
    disabled_drivers: &HashSet<url::Url>,
) -> Result<(), anyhow::Error> {
    let resolved_drivers = load_drivers(
        &base_drivers,
        &resolver,
        &eager_drivers,
        &disabled_drivers,
        DriverPackageType::Base,
    )
    .await
    .context("Error loading base packages")
    .map_err(log_error)?;
    for resolved_driver in &resolved_drivers {
        let mut composite_node_spec_manager = indexer.composite_node_spec_manager.borrow_mut();
        composite_node_spec_manager.new_driver_available(resolved_driver.clone());
    }
    indexer.load_base_repo(resolved_drivers);
    Ok(())
}

pub async fn load_drivers(
    drivers: &Vec<String>,
    resolver: &fresolution::ResolverProxy,
    eager_drivers: &HashSet<url::Url>,
    disabled_drivers: &HashSet<url::Url>,
    package_type: DriverPackageType,
) -> Result<Vec<ResolvedDriver>, anyhow::Error> {
    let mut resolved_drivers = std::vec::Vec::new();
    for driver_url in drivers {
        let url = match url::Url::parse(&driver_url) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!("Found bad driver url: {}: error: {}", driver_url, e);
                continue;
            }
        };
        let resolve = ResolvedDriver::resolve(url, &resolver, package_type).await;
        if resolve.is_err() {
            continue;
        }

        let mut resolved_driver = resolve.unwrap();
        if disabled_drivers.contains(&resolved_driver.component_url) {
            tracing::info!("Skipping driver: {}", resolved_driver.component_url.to_string());
            continue;
        }
        tracing::info!("Found driver: {}", resolved_driver.component_url.to_string());
        if eager_drivers.contains(&resolved_driver.component_url) {
            resolved_driver.fallback = false;
        }
        resolved_drivers.push(resolved_driver);
    }
    Ok(resolved_drivers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolved_driver::load_driver;
    use fidl_fuchsia_component_decl as fdecl;
    use fidl_fuchsia_io as fio;

    #[fuchsia::test]
    async fn test_load_fallback_driver() {
        const DRIVER_URL: &str = "fuchsia-boot:///#meta/test-fallback-component.cm";
        let driver_url = url::Url::parse(&DRIVER_URL).unwrap();
        let pkg = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();
        let manifest = fuchsia_fs::directory::open_file(
            &pkg,
            "meta/test-fallback-component.cm",
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await
        .unwrap();
        let decl = fuchsia_fs::file::read_fidl::<fdecl::Component>(&manifest).await.unwrap();
        let fallback_driver = load_driver(
            driver_url,
            decl,
            fuchsia_pkg::PackageDirectory::from_proxy(pkg),
            DriverPackageType::Boot,
            None,
        )
        .await
        .expect("Fallback driver was not loaded");
        assert!(fallback_driver.fallback);
    }
}
