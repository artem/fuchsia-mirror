// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
// Allowing unused crate dependencies because there isn't a good way
// to conditionally add deps to GN targets based on API levels
#![allow(unused_crate_dependencies)]
mod client_cpp;
mod client_fidl;
mod client_rust;
mod common;
mod cvf;
mod cvm;
mod dump_values;
mod validate_package;

use anyhow::Error;
use argh::FromArgs;
use client_cpp::GenerateCppSource;
use client_fidl::GenerateFidlSource;
use client_rust::GenerateRustSource;
use cvf::GenerateValueFile;
#[cfg(fuchsia_api_level_at_least = "20")]
use cvm::GenerateValueManifest;
use dump_values::DumpValues;
use validate_package::ValidatePackage;

#[derive(FromArgs, PartialEq, Debug)]
/// Tool for compiling structured configuration artifacts
struct Command {
    #[argh(subcommand)]
    sub: Subcommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum Subcommand {
    GenerateValueFile(GenerateValueFile),
    #[cfg(fuchsia_api_level_at_least = "20")]
    GenerateValueManifest(GenerateValueManifest),
    GenerateFidlSource(GenerateFidlSource),
    GenerateRustSource(GenerateRustSource),
    GenerateCppSource(GenerateCppSource),
    ValidatePackage(ValidatePackage),
    DumpValues(DumpValues),
}

fn main() -> Result<(), Error> {
    let command = argh::from_env::<Command>();

    match command.sub {
        Subcommand::GenerateValueFile(cmd) => cmd.generate(),
        #[cfg(fuchsia_api_level_at_least = "20")]
        Subcommand::GenerateValueManifest(cmd) => cmd.generate(),
        Subcommand::GenerateFidlSource(cmd) => cmd.generate(),
        Subcommand::GenerateRustSource(cmd) => cmd.generate(),
        Subcommand::GenerateCppSource(cmd) => cmd.generate(),
        Subcommand::ValidatePackage(cmd) => cmd.validate(),
        Subcommand::DumpValues(cmd) => cmd.dump(),
    }
}
