// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::Proxy;
use fidl_fuchsia_developer_remotecontrol as rc;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_sys2 as sys2;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;

pub async fn toolbox_directory(
    remote_proxy: &rc::RemoteControlProxy,
    query: &sys2::RealmQueryProxy,
) -> Result<Arc<impl DirectoryEntry>> {
    let controller =
        rcs::root_lifecycle_controller(remote_proxy, std::time::Duration::from_secs(5)).await?;
    let moniker = moniker::Moniker::try_from("core/toolbox")?;
    component_debug::lifecycle::resolve_instance(&controller, &moniker).await?;
    let dir = component_debug::dirs::open_instance_dir_root_readable(
        &moniker,
        sys2::OpenDirType::NamespaceDir.into(),
        &query,
    )
    .await?;

    let (svc_dir, object) = fidl::endpoints::create_endpoints();
    let svc_dir =
        fio::DirectoryProxy::from_channel(fidl::AsyncChannel::from_channel(svc_dir.into_channel()));
    dir.open(fio::OpenFlags::RIGHT_READABLE, fio::ModeType::empty(), "svc", object)?;
    Ok(vfs::remote::remote_dir(svc_dir))
}
