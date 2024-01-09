// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[derive(thiserror::Error, Debug)]
pub enum CpuManagerError {
    #[error("Error: {}", .0)]
    GenericError(anyhow::Error),

    #[error("Operation not supported")]
    Unsupported,

    #[error("Invalid argument")]
    InvalidArgument(String),
}

impl From<anyhow::Error> for CpuManagerError {
    fn from(e: anyhow::Error) -> Self {
        CpuManagerError::GenericError(e)
    }
}
