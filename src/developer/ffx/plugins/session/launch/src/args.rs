// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {argh::FromArgs, ffx_core::ffx_command};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "launch",
    description = "Launch a session",
    example = "To use the tiling session manager:

       $ fx set workstation_eng.x64 --with //src/session/examples/tiles-session

       $ ffx session launch fuchsia-pkg://fuchsia.com/tiles-session#meta/tiles-session.cm

This will launch the tiling session manager if a session is not already active. For a detailed explanation of sessions, see https://fuchsia.dev/fuchsia-src/concepts/session/introduction
"
)]
pub struct SessionLaunchCommand {
    #[argh(positional)]
    /// the component URL of a session.
    pub url: String,
}
