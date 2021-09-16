// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::wlan::types::{ClientStatusResponse, MacRole, QueryIfaceResponse};
use anyhow::{Context as _, Error};
use fidl_fuchsia_wlan_device;
use fidl_fuchsia_wlan_device_service::{
    DeviceMonitorMarker, DeviceMonitorProxy, DeviceServiceMarker, DeviceServiceProxy,
};
use fidl_fuchsia_wlan_internal as fidl_internal;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon as zx;
use ieee80211::Ssid;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::convert::TryInto;
use wlan_common::scan::ScanResult;

// WlanFacade: proxies commands from sl4f test to proper fidl APIs
//
// This object is shared among all threads created by server.  The inner object is the facade
// itself.  Callers interact with a wrapped version of the facade that enforces read/write
// protection.
//
// Use: Create once per server instantiation.
#[derive(Debug)]
struct InnerWlanFacade {
    #[allow(unused)]
    scan_results: bool,
}

#[derive(Debug)]
pub struct WlanFacade {
    wlan_svc: DeviceServiceProxy,
    monitor_svc: DeviceMonitorProxy,
    #[allow(unused)]
    inner: RwLock<InnerWlanFacade>,
}

impl WlanFacade {
    pub fn new() -> Result<WlanFacade, Error> {
        let wlan_svc = connect_to_protocol::<DeviceServiceMarker>()?;
        let monitor_svc = connect_to_protocol::<DeviceMonitorMarker>()?;

        Ok(WlanFacade {
            wlan_svc,
            monitor_svc,
            inner: RwLock::new(InnerWlanFacade { scan_results: false }),
        })
    }

    /// Gets the list of wlan interface IDs.
    pub async fn get_iface_id_list(&self) -> Result<Vec<u16>, Error> {
        let wlan_iface_ids = wlan_service_util::get_iface_list(&self.wlan_svc)
            .await
            .context("Get Iface Id List: failed to get wlan iface list")?;
        Ok(wlan_iface_ids)
    }

    /// Gets the list of wlan interface IDs.
    pub async fn get_phy_id_list(&self) -> Result<Vec<u16>, Error> {
        let wlan_phy_ids = wlan_service_util::get_phy_list(&self.monitor_svc)
            .await
            .context("Get Phy Id List: failed to get wlan phy list")?;
        Ok(wlan_phy_ids)
    }

    pub async fn scan(&self) -> Result<Vec<String>, Error> {
        // get the first client interface
        let sme_proxy = wlan_service_util::client::get_first_sme(&self.wlan_svc)
            .await
            .context("Scan: failed to get client iface sme proxy")?;

        // start the scan
        let scan_result_list =
            wlan_service_util::client::passive_scan(&sme_proxy).await.context("Scan failed")?;

        // send the ssids back to the test
        let mut ssid_list = Vec::new();
        for scan_result in &scan_result_list {
            let scan_result: ScanResult = scan_result.clone().try_into()?;
            let ssid = String::from_utf8_lossy(&scan_result.bss_description.ssid).into_owned();
            ssid_list.push(ssid);
        }
        Ok(ssid_list)
    }

    pub async fn scan_for_bss_info(
        &self,
    ) -> Result<HashMap<Ssid, Vec<Box<fidl_internal::BssDescription>>>, Error> {
        // get the first client interface
        let sme_proxy = wlan_service_util::client::get_first_sme(&self.wlan_svc)
            .await
            .context("Scan: failed to get client iface sme proxy")?;

        // start the scan
        let mut scan_result_list =
            wlan_service_util::client::passive_scan(&sme_proxy).await.context("Scan failed")?;

        // send the bss descriptions back to the test
        let mut hashmap = HashMap::new();
        for scan_result in scan_result_list.drain(..) {
            let scan_result: ScanResult = scan_result.try_into()?;
            let entry = hashmap.entry(scan_result.bss_description.ssid.clone()).or_insert(vec![]);
            entry.push(Box::new(scan_result.bss_description.into()));
        }

        Ok(hashmap)
    }

    // TODO(fxbug.dev/68531): Require a BSS description and remove the optional scan.
    pub async fn connect(
        &self,
        target_ssid: Ssid,
        target_pwd: Vec<u8>,
        target_bss_desc: Option<Box<fidl_internal::BssDescription>>,
    ) -> Result<bool, Error> {
        let target_bss_desc = target_bss_desc.unwrap_or(
            self.scan_for_bss_info()
                .await?
                .remove(&target_ssid)
                .and_then(|bss_descs| bss_descs.into_iter().next())
                .context("No suitable BSS found")?,
        );
        // get the first client interface
        let sme_proxy = wlan_service_util::client::get_first_sme(&self.wlan_svc)
            .await
            .context("Connect: failed to get client iface sme proxy")?;

        wlan_service_util::client::connect(
            &sme_proxy,
            target_ssid,
            target_pwd,
            target_bss_desc.as_ref().clone(),
        )
        .await
    }

    /// Destroys a WLAN interface by input interface ID.
    ///
    /// # Arguments
    /// * `iface_id` - The u16 interface id.
    pub async fn destroy_iface(&self, iface_id: u16) -> Result<(), Error> {
        wlan_service_util::destroy_iface(&self.monitor_svc, iface_id)
            .await
            .context("Destroy: Failed to destroy iface")
    }

    pub async fn disconnect(&self) -> Result<(), Error> {
        wlan_service_util::client::disconnect_all(&self.wlan_svc)
            .await
            .context("Disconnect: Failed to disconnect ifaces")
    }

    pub async fn status(&self) -> Result<ClientStatusResponse, Error> {
        // get the first client interface
        let sme_proxy = wlan_service_util::client::get_first_sme(&self.wlan_svc)
            .await
            .context("Status: failed to get iface sme proxy")?;

        let rsp = sme_proxy.status().await.context("failed to get status from sme_proxy")?;

        Ok(ClientStatusResponse::from(rsp))
    }

    pub async fn query_iface(&self, iface_id: u16) -> Result<QueryIfaceResponse, Error> {
        let (status, iface_info) = self
            .wlan_svc
            .query_iface(iface_id)
            .await
            .context("Failed to query iface information")?;

        zx::ok(status)?;

        let iface_info = match iface_info {
            Some(iface_info) => iface_info,
            None => return Err(format_err!("no iface information for ID: {}", iface_id)),
        };
        let mac_role = match iface_info.role {
            fidl_fuchsia_wlan_device::MacRole::Client => MacRole::Client,
            fidl_fuchsia_wlan_device::MacRole::Ap => MacRole::Ap,
            fidl_fuchsia_wlan_device::MacRole::Mesh => MacRole::Mesh,
        };

        Ok(QueryIfaceResponse {
            role: mac_role,
            id: iface_info.id,
            phy_id: iface_info.phy_id,
            phy_assigned_id: iface_info.phy_assigned_id,
            sta_addr: iface_info.sta_addr,
        })
    }
}
