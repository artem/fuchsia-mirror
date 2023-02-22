// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use component_debug::{
    cli::{show_cmd_print, show_cmd_serialized},
    realm::Instance,
};
use errors::FfxError;
use ffx_component::rcs::connect_to_realm_query;
use ffx_component_show_args::ComponentShowCommand;
use ffx_core::ffx_plugin;
use ffx_writer::Writer;
use fidl_fuchsia_developer_remotecontrol as rc;

#[ffx_plugin]
pub async fn cmd(
    rcs_proxy: rc::RemoteControlProxy,
    args: ComponentShowCommand,
    #[ffx(machine = Vec<Instance>)] writer: Writer,
) -> Result<()> {
    let realm_query = connect_to_realm_query(&rcs_proxy).await?;

    // All errors from component_debug library are user-visible.
    if writer.is_machine() {
        let output = show_cmd_serialized(args.query, realm_query)
            .await
            .map_err(|e| FfxError::Error(e, 1))?;
        writer.machine(&output)
    } else {
        show_cmd_print(args.query, realm_query, writer).await.map_err(|e| FfxError::Error(e, 1))?;
        Ok(())
    }
}
