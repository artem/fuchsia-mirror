// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    argh::{ArgsInfo, FromArgs},
    ffx_core::ffx_command,
    ffx_profile_sub_command::SubCommand,
};

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "profile",
    description = "Profile run-time information from various subsystems"
)]
pub struct ProfileCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}
