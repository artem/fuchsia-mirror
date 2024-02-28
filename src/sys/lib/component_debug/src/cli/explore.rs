// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{explore::*, query::get_cml_moniker_from_query},
    anyhow::Result,
    fidl_fuchsia_dash as fdash, fidl_fuchsia_sys2 as fsys,
};

pub async fn explore_cmd(
    query: String,
    ns_layout: DashNamespaceLayout,
    command: Option<String>,
    tools_urls: Vec<String>,
    dash_launcher: fdash::LauncherProxy,
    realm_query: fsys::RealmQueryProxy,
    stdout: socket_to_stdio::Stdout<'_>,
) -> Result<()> {
    let moniker = get_cml_moniker_from_query(&query, &realm_query).await?;
    println!("Moniker: {}", moniker);

    let (client, server) = fidl::Socket::create_stream();

    explore_over_socket(moniker, server, tools_urls, command, ns_layout, &dash_launcher).await?;

    #[allow(clippy::large_futures)]
    socket_to_stdio::connect_socket_to_stdio(client, stdout).await?;

    let exit_code = wait_for_shell_exit(&dash_launcher).await?;

    std::process::exit(exit_code);
}
