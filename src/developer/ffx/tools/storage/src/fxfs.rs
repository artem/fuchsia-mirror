// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    argh::{ArgsInfo, FromArgs},
    fho::{Error, Result, SimpleWriter},
    fidl_fuchsia_fxfs::DebugProxy,
};

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "compact",
    example = "ffx storage fxfs compact",
    description = "Forces a (blocking) compaction of all layer files."
)]
pub struct CompactSubCommand {}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "delete_profile",
    example = "ffx storage fxfs delete_profile",
    description = "Deletes a profile from a named unlocked volume."
)]
pub struct DeleteProfileSubCommand {
    #[argh(positional)]
    volume: String,
    #[argh(positional)]
    profile: String,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum FxfsSubCommand {
    Compact(CompactSubCommand),
    DeleteProfile(DeleteProfileSubCommand),
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "fxfs", description = "Interact with fxfs instances.")]
pub struct FxfsCommand {
    #[argh(subcommand)]
    subcommand: FxfsSubCommand,
}

pub async fn handle_cmd(
    cmd: FxfsCommand,
    _writer: SimpleWriter,
    fxfs_proxy: DebugProxy,
) -> Result<()> {
    match cmd.subcommand {
        FxfsSubCommand::Compact(_) => {
            fxfs_proxy
                .compact()
                .await
                .map_err(|e| Error::User(e.into()))?
                .map_err(|e| Error::ExitWithCode(e))?;
        }
        FxfsSubCommand::DeleteProfile(args) => {
            fxfs_proxy
                .delete_profile(&args.volume, &args.profile)
                .await
                .map_err(|e| Error::User(e.into()))?
                .map_err(|e| Error::ExitWithCode(e))?;
        }
    };
    Ok(())
}
