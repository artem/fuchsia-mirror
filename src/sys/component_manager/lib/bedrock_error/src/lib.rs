// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    fmt::{Debug, Display},
    sync::Arc,
};

use fuchsia_zircon_status as zx;
use thiserror::Error;

/// The error type returned by bedrock operations.
#[derive(Debug, Error, Clone)]
pub enum BedrockError {
    #[error("could not route: {0}")]
    RoutingError(Arc<dyn Explain>),

    #[error("could not transition lifecycle: {0}")]
    LifecycleError(Arc<dyn Explain>),

    #[error("could not open: {0}")]
    OpenError(Arc<dyn Explain>),
}

/// All detailed error objects must implement the [`Explain`] trait, since:
///
/// - Some operations are not yet refactored into bedrock.
/// - Some operations fundamentally are not fit for bedrock.
///
/// The detailed errors are hidden, but users may get strings or codes for debugging.
pub trait Explain: std::error::Error + Debug + Display + Send + Sync + sealed::AnyCast {
    fn as_zx_status(&self) -> zx::Status;
}

impl Explain for BedrockError {
    fn as_zx_status(&self) -> zx::Status {
        match self {
            BedrockError::RoutingError(err) => err.as_zx_status(),
            BedrockError::LifecycleError(err) => err.as_zx_status(),
            BedrockError::OpenError(err) => err.as_zx_status(),
        }
    }
}

/// To test the error case of e.g. a `Router` implementation, it will be helpful
/// to cast the erased error back to an expected error type and match on it.
///
/// Do not use this in production as conditioning behavior on error cases is
/// extremely fragile.
pub trait DowncastErrorForTest {
    /// For tests only. Downcast the erased error to `E` or panic if fails.
    fn downcast_for_test<E: Explain>(&self) -> &E;
}

impl DowncastErrorForTest for dyn Explain {
    fn downcast_for_test<E: Explain>(&self) -> &E {
        match self.as_any().downcast_ref::<E>() {
            Some(value) => value,
            None => {
                let expected = std::any::type_name::<E>();
                panic!("Cannot downcast `{self}` to the {expected} error type!");
            }
        }
    }
}

mod sealed {
    use std::any::Any;

    pub trait AnyCast: Any {
        fn as_any(&self) -> &dyn Any;
    }

    impl<T: Any> AnyCast for T {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
}
