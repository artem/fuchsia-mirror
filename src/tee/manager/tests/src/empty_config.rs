// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
#![allow(unused_imports)]
use anyhow::{Context, Error};
use fidl_fuchsia_io;
use fuchsia_component::client::connect_to_protocol_at_path;
use fuchsia_fs;

#[fuchsia::test]
async fn iterate_exposed_tas() -> Result<(), Error> {
    let ta_dir = connect_to_protocol_at_path::<fidl_fuchsia_io::DirectoryMarker>("/ta")
        .context("Failed to connect to ta directory")?;
    let entries = fuchsia_fs::directory::readdir(&ta_dir).await?;
    assert_eq!(entries, vec![]);
    Ok(())
}
