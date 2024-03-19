// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::api::{Api, Component, ComponentId, CreateIssue, IssueId, UpdateIssue};

pub struct Bugspec {
    path: PathBuf,
    log_api: bool,
}

impl Bugspec {
    pub fn new(path: PathBuf, log_api: bool) -> Self {
        Self { path, log_api }
    }
}

impl Api for Bugspec {
    fn create_issue(&mut self, request: CreateIssue) -> Result<IssueId> {
        if self.log_api {
            println!("[bugspec] Creating new issue");
        }

        let mut child = Command::new(&self.path)
            .arg("create")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.to_bugspec().as_bytes())?;
        drop(stdin);

        let output = child.wait_with_output()?;

        if self.log_api {
            println!("[bugspec] Successfully created issue");
        }

        let response = core::str::from_utf8(&output.stdout)?;
        let id = response
            .strip_prefix("Created issue http://b/")
            .and_then(|r| r.strip_suffix('\n'))
            .context(format!("Unexpected response from bugspec API: '{}'", response))?;

        Ok(IssueId::new(id.parse()?))
    }

    fn update_issue(&mut self, request: UpdateIssue) -> Result<()> {
        let mut child = Command::new(&self.path)
            .arg("edit")
            .arg(&format!("{}", request.id))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.to_bugspec().as_bytes())?;
        drop(stdin);

        child.wait()?;

        Ok(())
    }

    fn list_components(&mut self) -> Result<Vec<Component>> {
        const FUCHSIA_COMPONENT_ID: &str = "1360843";

        let output =
            Command::new(&self.path).args(&["list-components", FUCHSIA_COMPONENT_ID]).output()?;

        let text = String::from_utf8(output.stdout)?;

        let mut results = Vec::new();
        for line in text.lines() {
            let (id, path) =
                line.split_once('\t').context("expected component to have id and path")?;
            results.push(Component {
                id: ComponentId::new(
                    id.parse()
                        .context(format!("while parsing id `{id}` for component `{path}`"))?,
                ),
                path: path.split(" > ").skip(2).map(str::to_string).collect(),
            });
        }

        Ok(results)
    }
}
