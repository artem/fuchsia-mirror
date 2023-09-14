// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;

#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "register",
    description = "Informs the driver manager that a new driver package is available. The driver manager will cache a copy of the driver",
    example = "To register a driver

    $ driver register 'fuchsia-pkg://fuchsia.com/example_driver#meta/example_driver.cm'",
    error_code(1, "Failed to connect to the driver registrar service")
)]
pub struct RegisterCommand {
    #[argh(positional, description = "component URL of the driver to be registered.")]
    pub url: String,

    /// if this exists, the user will be prompted for a component to select.
    #[argh(switch, short = 's', long = "select")]
    pub select: bool,
}
