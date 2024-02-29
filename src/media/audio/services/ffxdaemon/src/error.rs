// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_audio_controller::Error;
use std::fmt;

// Wrapper for the Controller FIDL error type.

#[derive(Debug, Clone)]
pub struct ControllerError {
    pub inner: fidl_fuchsia_audio_controller::Error,
    pub msg: String,
}

impl ControllerError {
    pub fn new(inner: fidl_fuchsia_audio_controller::Error, msg: String) -> Self {
        ControllerError { inner, msg }
    }
}

impl fmt::Display for ControllerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.inner, self.msg)
    }
}

impl std::error::Error for ControllerError {}

impl From<fidl::Error> for ControllerError {
    fn from(source: fidl::Error) -> Self {
        Self { inner: Error::UnknownCanRetry, msg: format!("FIDL Error: {source}") }
    }
}

impl From<fuchsia_zircon_status::Status> for ControllerError {
    fn from(source: fuchsia_zircon_status::Status) -> Self {
        Self { inner: Error::UnknownCanRetry, msg: format!("Zx error: {source}") }
    }
}

impl From<anyhow::Error> for ControllerError {
    fn from(source: anyhow::Error) -> Self {
        Self { inner: Error::UnknownCanRetry, msg: format!("{source}") }
    }
}
