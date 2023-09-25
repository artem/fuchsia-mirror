// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use ffx_package_sub_command::SubCommand;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "package", description = "Create and publish Fuchsia packages")]
pub struct PackageCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}
