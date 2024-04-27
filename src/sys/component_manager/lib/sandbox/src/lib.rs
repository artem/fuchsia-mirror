// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Component sandbox traits and capability types.

mod capability;
mod component;
mod data;
mod dict;
mod directory;
mod handle;
mod open;
mod receiver;
mod registry;
mod router;
mod sender;
mod unit;

pub use self::capability::{Capability, CapabilityTrait, ConversionError, RemoteError};
pub use self::component::{WeakComponentToken, WeakComponentTokenAny};
pub use self::data::Data;
pub use self::dict::{Dict, DictEntries, Key as DictKey};
pub use self::directory::Directory;
pub use self::handle::OneShotHandle;
pub use self::open::Open;
pub use self::receiver::Receiver;
pub use self::router::{Request, Routable, Router};
pub use self::sender::{Message, Sendable, Sender};
pub use self::unit::Unit;
