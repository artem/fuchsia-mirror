// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use ffx_command::FfxContext;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_sys2::OpenDirType;
use std::time::{Duration, Instant};

pub const MONIKER: &str = "core/toolbox";

/// Connects to a protocol available in the namespace of the `toolbox` component.
/// If we fail to connect to the protocol in the namespace of the `toolbox` component, then we'll
/// attempt to connect to the protocol in the exposed directory of the component located at the
/// given `backup_moniker`.
pub async fn connect_with_timeout<P>(
    rcs_proxy: &RemoteControlProxy,
    backup_moniker: Option<impl AsRef<str>>,
    dur: Duration,
) -> Result<P::Proxy>
where
    P: DiscoverableProtocolMarker,
{
    let protocol_name = P::PROTOCOL_NAME;
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>()?;
    // time this so that we can use an appropriately shorter timeout for the attempt
    // to connect by the backup (if there is one)
    let start_time = Instant::now();
    let toolbox_res = crate::open_with_timeout_at(
        dur,
        MONIKER,
        OpenDirType::NamespaceDir,
        &format!("svc/{protocol_name}"),
        rcs_proxy,
        server_end.into_channel(),
    )
    .await;
    let toolbox_took = Instant::now() - start_time;
    // after doing these somewhat awkward lines, we know that toolbox_res is an
    // error and we have to either try the backup or return a useful error
    // message. This just avoids an indentation or having to break this out
    // into another single-use function. It's kind of a reverse `?`.
    let Some(backup) = backup_moniker.as_ref().map(|s| s.as_ref()) else {
        toolbox_res.with_user_message(|| toolbox_error_message(protocol_name))?;
        return Ok(proxy);
    };
    let Err(toolbox_err) = toolbox_res else {
        return Ok(proxy);
    };

    // try to connect to the moniker given instead, but don't double
    // up the timeout.
    let timeout = dur.saturating_sub(toolbox_took);
    let (proxy, server_end) = fidl::endpoints::create_proxy::<P>().unwrap();
    let moniker_res = crate::open_with_timeout::<P>(
        timeout,
        &backup,
        OpenDirType::ExposedDir,
        &rcs_proxy,
        server_end.into_channel(),
    )
    .await;

    // stack the errors together so we can see both of them in the log if
    // we want to and then provide an error message that indicates we tried
    // both and could find it at neither.
    moniker_res
        .bug_context(toolbox_err)
        .with_user_message(|| backup_error_message(protocol_name, &backup))?;
    Ok(proxy)
}

fn toolbox_error_message(protocol_name: &str) -> String {
    format!(
        "\
        Attempted to find protocol marker {protocol_name} at \
        '/core/toolbox', but it wasn't available. \n\n\
        Make sure the target is connected and otherwise functioning, \
        and that it is configured to provide capabilities over the \
        network to host tools.\
    "
    )
}

fn backup_error_message(protocol_name: &str, backup_name: &str) -> String {
    format!(
        "\
        Attempted to find protocol marker {protocol_name} at \
        '/core/toolbox' or '{backup_name}', but it wasn't available \
        at either of those monikers. \n\n\
        Make sure the target is connected and otherwise functioning, \
        and that it is configured to provide capabilities over the \
        network to host tools.\
    "
    )
}
