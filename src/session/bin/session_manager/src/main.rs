// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error, fidl_fuchsia_component as fcomponent,
    fuchsia_component::client::connect_to_protocol, fuchsia_component::server::ServiceFs,
    session_manager_config::Config, session_manager_lib::session_manager::SessionManager,
    tracing::info,
};

// If we find a file at this path, we won't automatically launch the session on
// startup, regardless of what the `autolaunch` structured config value says.
//
// TODO(https://fxbug.dev/42077029): Delete this mechanism once we have a proper
// replacement that relies only on structured config.
const DISABLE_AUTOLAUNCH_PATH: &str = "/data/session-manager/noautolaunch";

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    let realm = connect_to_protocol::<fcomponent::RealmMarker>()?;

    // Start the startup session, if any, and serve services exposed by session manager.
    let Config { session_url, autolaunch } = Config::take_from_startup_handle();
    let is_session_url_empty = session_url.is_empty();
    let mut session_manager =
        SessionManager::new(realm, inspector, (!is_session_url_empty).then_some(session_url));

    if is_session_url_empty {
        info!("Received an empty startup session URL. Waiting for a request.");
    } else if !autolaunch {
        info!("Startup session URL set, but autolaunch config option was false. Waiting for a request.");
    } else if std::path::Path::new(DISABLE_AUTOLAUNCH_PATH).exists() {
        info!(
            "Session autolaunch blocked by file at path {}. Waiting for a request.",
            DISABLE_AUTOLAUNCH_PATH
        );
    } else {
        // TODO(https://fxbug.dev/42146741): Using ? here causes errors to not be logged.
        session_manager.start_default_session().await.expect("failed to start session");
    }

    session_manager.serve(&mut fs).await.expect("failed to serve protocols");

    Ok(())
}
