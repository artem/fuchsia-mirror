// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    argh::{ArgsInfo, FromArgs},
    ffx_core::ffx_command,
};

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "preflight",
    description = "Evaluate suitability for building and running Fuchsia"
)]
pub struct PreflightCommand {
    #[argh(switch)]
    /// outputs json instead of human-readable text.
    pub json: bool,
}
