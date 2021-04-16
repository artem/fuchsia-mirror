use std::sync::{Arc, Mutex};

use mio::{Registry, Token};

use crate::{ErrorKind, Result};

/// Allows to wake up the `mio::Poll::poll()` method.
/// This type wraps `mio::Waker`, for more information see its documentation.
#[derive(Clone, Debug)]
pub(crate) struct Waker {
    inner: Arc<Mutex<mio::Waker>>,
}

impl Waker {
    /// Create a new `Waker`.
    pub(crate) fn new(registry: &Registry, waker_token: Token) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(mio::Waker::new(registry, waker_token)?)),
        })
    }

    /// Wake up the [`Poll`] associated with this `Waker`.
    ///
    /// Readiness is set to `Ready::readable()`.
    pub(crate) fn wake(&self) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .wake()
            .map_err(|e| ErrorKind::IoError(e))
    }

    /// Resets the state so the same waker can be reused.
    ///
    /// This function is not impl
    pub(crate) fn reset(&self) -> Result<()> {
        Ok(())
    }
}
