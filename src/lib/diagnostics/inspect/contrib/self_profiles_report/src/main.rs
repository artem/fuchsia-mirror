// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context as _;
use argh::FromArgs;
use diagnostics_data::InspectData;
use self_profiles_report::SelfProfilesReport;
use std::{io::Write, path::PathBuf};

/// a small tool to render a human-readable report from inspect snapshots containing self-profiling
/// data.
#[derive(Debug, FromArgs)]
struct Options {
    /// path to the inspect snapshot in JSON.
    #[argh(positional)]
    snapshot: PathBuf,

    /// optional path to which to write text output
    #[argh(option)]
    output: Option<PathBuf>,
}

#[fuchsia::main]
async fn main() -> anyhow::Result<()> {
    let options: Options = argh::from_env();
    let raw_snapshot = std::fs::read(&options.snapshot).context("reading snapshot")?;
    let snapshot: Vec<InspectData> =
        serde_json::from_slice(&raw_snapshot).context("parsing snapshot as json")?;
    let reports = SelfProfilesReport::from_snapshot(&snapshot).context("analyzing snapshot")?;

    for report in reports {
        if let Some(output) = &options.output {
            let mut f = std::fs::File::create(output).context("creating output file")?;
            write!(&mut f, "{report}").context("writing report to file")?;
        } else {
            println!("{report}");
        }
    }

    Ok(())
}
