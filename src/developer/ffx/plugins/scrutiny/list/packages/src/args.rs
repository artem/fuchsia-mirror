// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use std::path::PathBuf;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "packages",
    description = "Lists all the packages in the build",
    example = "To list all the packages in the build:

        $ffx scrutiny list packages \
            --product-bundle $(fx get-build-dir)/obj/build/images/fuchsia/product_bundle",
    note = "Lists all the packages in the build in a json format."
)]
pub struct ScrutinyPackagesCommand {
    /// path to a product bundle.
    #[argh(option)]
    pub product_bundle: PathBuf,
    /// build scrutiny model based on recovery-mode build artifacts.
    #[argh(switch)]
    pub recovery: bool,
}
