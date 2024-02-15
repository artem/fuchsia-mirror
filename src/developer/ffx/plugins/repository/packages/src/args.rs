// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use std::path::PathBuf;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "package", description = "List the packages inside a repository")]
pub struct PackagesCommand {
    #[argh(subcommand)]
    pub subcommand: PackagesSubCommand,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum PackagesSubCommand {
    List(ListSubCommand),
    Show(ShowSubCommand),
    ExtractArchive(ExtractArchiveSubCommand),
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "list", description = "Inspect and manage package repositories")]
pub struct ListSubCommand {
    #[argh(option, short = 'r')]
    /// list packages from this repository.
    pub repository: Option<String>,

    /// if true, package hashes will be displayed in full (i.e. not truncated).
    #[argh(switch)]
    pub full_hash: bool,

    /// toggle whether components in each package will be fetched and shown in the output table
    #[argh(option, default = "true")]
    pub include_components: bool,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "show", description = "Inspect content of a package")]
pub struct ShowSubCommand {
    #[argh(option, short = 'r')]
    /// list package contents from this repository.
    pub repository: Option<String>,

    /// if true, package hashes will be displayed in full (i.e. not truncated).
    #[argh(switch)]
    pub full_hash: bool,

    /// if true, show contents of subpackages contained in the package.
    #[argh(option, default = "false")]
    pub include_subpackages: bool,

    #[argh(positional)]
    /// list this package's contents.
    pub package: String,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "extract-archive",
    description = "Extract a package archive from the repository"
)]
pub struct ExtractArchiveSubCommand {
    /// output path for the extracted archive.
    #[argh(option, short = 'o')]
    pub out: PathBuf,

    #[argh(option, short = 'r')]
    /// extract package from this repository.
    pub repository: Option<String>,

    #[argh(positional)]
    /// extract this package.
    pub package: String,
}
