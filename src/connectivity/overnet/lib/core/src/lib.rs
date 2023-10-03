// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Main Overnet functionality.

#![deny(missing_docs)]

mod coding;
mod future_help;
mod handle_info;
mod labels;
mod peer;
mod proxy;
mod router;
mod test_util;

// Export selected types from modules.
pub use coding::{decode_fidl, encode_fidl};
pub use future_help::log_errors;
pub use labels::{Endpoint, NodeId, NodeLinkId};
pub use router::{AscenddClientRouting, ListPeersContext, Router};

pub use test_util::NodeIdGenerator;

/// Utility trait to trace a variable to the log.
pub(crate) trait Trace {
    /// Trace the caller - add `msg` as text to display, and `ctx` as some context
    /// for the system that caused this value to be traced.
    fn trace(self, msg: impl std::fmt::Display, ctx: impl std::fmt::Debug) -> Self
    where
        Self: Sized;

    fn maybe_trace(
        self,
        trace: bool,
        msg: impl std::fmt::Display,
        ctx: impl std::fmt::Debug,
    ) -> Self
    where
        Self: Sized,
    {
        if trace {
            self.trace(msg, ctx)
        } else {
            self
        }
    }
}

impl<X: std::fmt::Debug> Trace for X {
    fn trace(self, msg: impl std::fmt::Display, ctx: impl std::fmt::Debug) -> Self
    where
        Self: Sized,
    {
        tracing::info!("[{:?}] {}: {:?}", ctx, msg, self);
        self
    }
}
