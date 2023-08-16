// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io;
use thiserror::Error;

/// Error encountered while executing test binary
#[derive(Debug, Error)]
pub enum TestRunError {
    #[error("Error launching test binary '{0:?}': {1:?}")]
    Spawn(std::ffi::OsString, io::Error),

    #[error("Error reading stdout: {0:?}")]
    StdoutRead(#[source] io::Error),

    #[error("Error reading stderr: {0:?}")]
    StderrRead(#[source] io::Error),

    #[error("Error writing stdout: {0:?}")]
    StdoutWrite(#[source] io::Error),

    #[error("Error writing stderr: {0:?}")]
    StderrWrite(#[source] io::Error),
}
