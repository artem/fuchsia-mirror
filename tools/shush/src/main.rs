// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod allow;
mod api;
mod bugspec;
mod fix;
mod issues;
mod lint;
mod mock;
mod owners;
mod rollout;
mod span;

use anyhow::{anyhow, Result};
use argh::FromArgs;

use std::{
    env,
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command,
};

use crate::api::Api;

#[derive(Debug, FromArgs)]
/// Silence rustc and clippy lints with allow attributes and autofixes
struct Args {
    #[argh(subcommand)]
    action: Action,
    /// don't modify source files
    #[argh(switch)]
    dryrun: bool,
    /// modify files even if there are local uncommitted changes
    #[argh(switch)]
    force: bool,
    /// lint (or category) to deal with e.g. clippy::needless_return
    #[argh(option)]
    lint: Vec<String>,
    /// the path to the rollout file
    #[argh(option)]
    rollout: Option<PathBuf>,
    /// path to the root dir of the fuchsia source tree
    #[argh(option)]
    fuchsia_dir: Option<PathBuf>,
    /// file containing json lints (uses stdin if not given)
    #[argh(positional)]
    lint_file: Option<PathBuf>,
    /// path to a binary for API calls
    #[argh(option)]
    api: Option<PathBuf>,
    /// mock API issue creation calls
    #[argh(switch)]
    mock: bool,
    /// print details of created issues to the command line
    #[argh(switch)]
    verbose: bool,
    /// print API calls to the command line
    #[argh(switch)]
    log_api: bool,
}

impl Args {
    pub fn try_get_filter(&self) -> Result<&[String]> {
        if self.lint.is_empty() {
            Err(anyhow!("Must filter on at least one lint or category with '--lint'"))
        } else {
            Ok(&self.lint)
        }
    }

    pub fn read_lints(&self) -> Box<dyn BufRead> {
        if let Some(ref f) = self.lint_file {
            Box::new(BufReader::new(File::open(f).unwrap()))
        } else {
            Box::new(BufReader::new(io::stdin()))
        }
    }

    pub fn api(&self) -> Result<Box<dyn Api>> {
        let api = self
            .api
            .as_ref()
            .map(|path| Box::new(bugspec::Bugspec::new(path.clone())) as Box<dyn Api>);

        if self.dryrun || self.mock {
            Ok(Box::new(mock::Mock::new(self.log_api, api)))
        } else {
            Ok(api.ok_or_else(|| anyhow!("--api is required when shush is not mocked"))?)
        }
    }

    pub fn rollout_path(&self) -> &Path {
        self.rollout.as_deref().unwrap_or(Path::new("./rollout.json~"))
    }

    pub fn change_to_fuchsia_root(&self) -> Result<PathBuf> {
        if let Some(fuchsia_dir) =
            self.fuchsia_dir.clone().or_else(|| env::var("FUCHSIA_DIR").ok().map(Into::into))
        {
            env::set_current_dir(&fuchsia_dir)?;
            Ok(fuchsia_dir)
        } else {
            Ok(std::env::current_dir()?.canonicalize()?)
        }
    }
}

#[derive(Debug, FromArgs)]
#[argh(subcommand)]
enum Action {
    Fix(Fix),
    Allow(Allow),
    Rollout(Rollout),
}

/// use rustfix to auto-fix the lints
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "fix")]
struct Fix {}

/// add allow attributes
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "allow")]
struct Allow {
    /// the tag to link to on codesearch
    #[argh(option)]
    codesearch_tag: Option<String>,
    /// path to an issue description template containing "INSERT_DETAILS_HERE"
    #[argh(option)]
    template: Option<PathBuf>,
    /// the issue to mark created issues as blocking
    #[argh(option)]
    blocking_issue: Option<String>,
    /// the maximum number of additional users to CC on created issues
    #[argh(option, default = "3")]
    max_cc_users: usize,
    /// the holding component to place newly-created bugs into (default "LanguagePlatforms>Rust")
    #[argh(option)]
    holding_component: Option<String>,
}

impl Allow {
    fn load_template(&self) -> Result<Option<String>> {
        Ok(self.template.as_ref().map(|path| fs::read_to_string(path)).transpose()?)
    }
}

/// roll out lints generated by allow
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "rollout")]
struct Rollout {}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let git_status =
        Command::new("jiri").args(["runp", "git", "status", "--porcelain"]).output()?;
    let clean_tree = git_status.status.success() && git_status.stdout.is_empty();
    if !(args.dryrun || args.force || clean_tree) {
        return Err(anyhow!(
            "The current directory is dirty, pass the --force flag or commit the local changes"
        ));
    }

    match args.action {
        Action::Fix(_) => fix::fix(&mut args.read_lints(), args.try_get_filter()?, args.dryrun),
        Action::Allow(ref allow_args) => {
            let mut api = args.api()?;
            let mut issue_template = issues::IssueTemplate::new(
                &args.lint,
                allow_args.codesearch_tag.as_deref(),
                allow_args.load_template()?,
                allow_args.blocking_issue.as_deref(),
                allow_args.max_cc_users,
            );

            let rollout_path = args.rollout_path();
            if rollout_path.exists() {
                return Err(anyhow!(
                    "The rollout path {} already exists, delete it or specify an alternate path.",
                    rollout_path.to_str().unwrap_or("<non-utf8 path>"),
                ));
            }

            allow::allow(
                &mut args.read_lints(),
                args.try_get_filter()?,
                &args.change_to_fuchsia_root()?,
                &mut *api,
                &mut issue_template,
                rollout_path,
                allow_args
                    .holding_component
                    .as_ref()
                    .map(String::as_str)
                    .unwrap_or("LanguagePlatforms>Rust"),
                args.dryrun,
                args.verbose,
            )
        }
        Action::Rollout(_) => {
            let mut api = args.api()?;
            let rollout_path = args.rollout_path();
            if !rollout_path.exists() {
                return Err(anyhow!(
                    "The rollout path {} does not exist, run shush allow to generate a rollout file.",
                    rollout_path.to_str().unwrap_or("<non-utf8 path>"),
                ));
            }

            rollout::rollout(&mut *api, rollout_path, args.verbose)
        }
    }
}
