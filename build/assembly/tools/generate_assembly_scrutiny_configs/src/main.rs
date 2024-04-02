// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use assembly_config_schema::BuildType;
use assembly_util::{
    BlobfsCompiledPackageDestination, BootfsDestination, BootfsPackageDestination,
    PackageDestination,
};
use camino::Utf8PathBuf;
use strum::IntoEnumIterator;

#[derive(FromArgs)]
/// Produce the bootfs/packages allowlist for assembly-generated files.
struct Args {
    /// the path to the output packages allowlist.
    #[argh(option)]
    static_packages: Utf8PathBuf,

    /// the path to the output bootfs packages allowlist.
    #[argh(option)]
    bootfs_packages: Utf8PathBuf,

    /// the path to the output bootfs file allowlist.
    #[argh(option)]
    bootfs_files: Utf8PathBuf,

    /// build type of the product.
    #[argh(option)]
    build_type: BuildType,
}

/// Used to filter destinations by if they are assembly generated or not.
trait AssemblyGenerated {
    fn assembly_generated(&self) -> bool;
}

impl AssemblyGenerated for PackageDestination {
    /// Is this destination generated by assembly ? (true)
    /// Or provided to it? (false)
    fn assembly_generated(&self) -> bool {
        match self {
            // These are all provided to assembly:
            Self::FromAIB(_) | Self::FromBoard(_) | Self::FromProduct(_) | Self::ForTest => false,
            // Everything else is assembly-generated.
            _ => true,
        }
    }
}

/// This generates the static packages allowlist, based on the build-type of the
/// system being assembled.  Some assembly-generated packages are only created
/// in certain build-types.
fn get_static_packages_allowlist(build_type: &BuildType) -> Vec<String> {
    let mut static_packages: Vec<String> = PackageDestination::iter()
        .filter_map(|v|
            // This script only returns assembly-generated files.
            // Files from AIBs or the product are collected and merged in a separate process.
            if v.assembly_generated() {
                match (v, build_type) {
                    // Shell commands are not included on user builds
                    (PackageDestination::ShellCommands, BuildType::User) => None,
                    // But are included on all others.
                    (a @ PackageDestination::ShellCommands, _) => Some(a.to_string()),

                    // All other packages created by assembly are added in all
                    // builds.
                    (a @ _, _) => Some(a.to_string()),
                }
            } else {
                None
            })
        .collect();
    let mut compiled_packages: Vec<String> = BlobfsCompiledPackageDestination::iter()
        .filter_map(|v| match (v, build_type) {
            // Toolbox should not be included on user.
            (BlobfsCompiledPackageDestination::Toolbox, BuildType::User) => None,
            // But is included on all others.
            (a @ BlobfsCompiledPackageDestination::Toolbox, _) => Some(a.to_string()),

            // All other packages created by assembly are added in all
            // builds.
            (a @ _, _) => Some(a.to_string()),
        })
        .collect();
    static_packages.append(&mut compiled_packages);
    static_packages.sort();
    static_packages
}

impl AssemblyGenerated for BootfsPackageDestination {
    /// Is this destination generated by assembly ? (true)
    /// Or provided to it? (false)
    fn assembly_generated(&self) -> bool {
        match self {
            // Package in bootfs that come from AIB, Boards, or are for testing
            // assembly itself are not "assembly generated".
            Self::FromAIB(_) | Self::FromBoard(_) | Self::ForTest => false,
            // Everything else is assembly-generated.
            _ => true,
        }
    }
}

fn get_bootfs_packages_allowlist() -> Vec<String> {
    let mut bootfs_packages: Vec<String> = BootfsPackageDestination::iter()
    .filter_map(|v|
        // This script only returns assembly-generated files.
        // Files from AIBs are collected and merged in a separate process.
        if v.assembly_generated() {
            Some(v.to_string())
        } else {
            None
        }
    )
    .collect();
    bootfs_packages.sort();
    bootfs_packages
}

fn main() {
    let args: Args = argh::from_env();

    let static_packages = get_static_packages_allowlist(&args.build_type);
    std::fs::write(args.static_packages, static_packages.join("\n"))
        .expect("Writing packages allowlist");

    let bootfs_packages = get_bootfs_packages_allowlist();
    std::fs::write(args.bootfs_packages, bootfs_packages.join("\n"))
        .expect("Writing bootfs packages allowlist");

    let mut bootfs_files: Vec<String> = BootfsDestination::iter()
        .filter_map(|v| match v {
            // This script only returns assembly-generated files.
            // Files from AIBs are collected and merged in a separate process.
            BootfsDestination::FromAIB(_) | BootfsDestination::ForTest => None,
            a @ _ => Some(a.to_string()),
        })
        .collect();
    bootfs_files.sort();
    std::fs::write(args.bootfs_files, bootfs_files.join("\n")).expect("Writing bootfs allowlist");
}
