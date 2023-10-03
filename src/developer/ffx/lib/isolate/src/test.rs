// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Isolate;

#[derive(thiserror::Error, Debug)]
pub enum TestingError {
    #[error("Unexpected exit code. Expected {0}, got {1}")]
    UnexpectedExitCode(i32, i32),
    #[error("Error executing command: {0:?}")]
    ExecutionError(anyhow::Error),
    #[error("IO error {0:?}")]
    IoError(std::io::Error),
    #[error("Output did not match expected. Actual output: {0}")]
    MatchingError(String),
}

/// Struct defining a command line for executing as part of a test.
pub struct TestCommandLineInfo<'a> {
    /// args is the `ffx` command line arguments, not including `ffx`
    pub args: Vec<&'a str>,
    /// stdout_check and stderr_check are functions or closures to
    /// check the contents of stdout and stderr. This allows for
    /// flexibility of rigor, for example `|_| true` ignores stdout,
    /// and |s| s == "somevalue" performs an exact match.
    pub stdout_check: fn(&str) -> bool,
    pub stderr_check: fn(&str) -> bool,
    /// expected_exit_code is the exit code from the process.
    pub expected_exit_code: i32,
}

impl<'a> TestCommandLineInfo<'a> {
    pub fn new(
        args: Vec<&'a str>,
        stdout_check: fn(&str) -> bool,
        stderr_check: fn(&str) -> bool,
        expected_exit_code: i32,
    ) -> Self {
        TestCommandLineInfo {
            args,
            stdout_check: stdout_check,
            stderr_check: stderr_check,
            expected_exit_code,
        }
    }

    pub async fn run_command_lines(
        isolate: &Isolate,
        test_data: Vec<TestCommandLineInfo<'_>>,
    ) -> Result<(), TestingError> {
        for test in test_data {
            test.run_command_with_checks(isolate).await?;
        }
        Ok(())
    }

    async fn run_command_with_checks(&self, isolate: &Isolate) -> Result<String, TestingError> {
        let output = isolate.ffx(&self.args).await.map_err(|e| TestingError::ExecutionError(e))?;

        let actual_exit_code = output.status.code().expect("exit code");
        if actual_exit_code != self.expected_exit_code {
            return Err(TestingError::UnexpectedExitCode(
                self.expected_exit_code,
                actual_exit_code,
            ));
        }

        if !(self.stdout_check)(&output.stdout) {
            return Err(TestingError::MatchingError(format!(
                "stdout check failed for {:?}",
                self.args
            )));
        }

        if !(self.stderr_check)(&output.stderr) {
            return Err(TestingError::MatchingError(format!(
                "stderr check failed for {:?}",
                self.args
            )));
        }

        Ok(output.stdout)
    }
}
