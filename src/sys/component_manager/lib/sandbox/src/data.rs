// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_component_sandbox as fsandbox;
use std::fmt::Debug;

use crate::RemoteError;

/// A capability that holds immutable data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    Bytes(Vec<u8>),
    String(String),
    Int64(i64),
    Uint64(u64),
}

#[cfg(target_os = "fuchsia")]
impl crate::CapabilityTrait for Data {}

impl TryFrom<fsandbox::Data> for Data {
    type Error = RemoteError;

    fn try_from(data_capability: fsandbox::Data) -> Result<Self, Self::Error> {
        match data_capability {
            fsandbox::Data::Bytes(bytes) => Ok(Self::Bytes(bytes)),
            fsandbox::Data::String(string) => Ok(Self::String(string)),
            fsandbox::Data::Int64(num) => Ok(Self::Int64(num)),
            fsandbox::Data::Uint64(num) => Ok(Self::Uint64(num)),
            fsandbox::DataUnknown!() => Err(RemoteError::UnknownVariant),
        }
    }
}

impl From<Data> for fsandbox::Data {
    fn from(data: Data) -> Self {
        match data {
            Data::Bytes(bytes) => fsandbox::Data::Bytes(bytes),
            Data::String(string) => fsandbox::Data::String(string),
            Data::Int64(num) => fsandbox::Data::Int64(num),
            Data::Uint64(num) => fsandbox::Data::Uint64(num),
        }
    }
}

impl From<Data> for fsandbox::Capability {
    fn from(data: Data) -> Self {
        Self::Data(data.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;
    use assert_matches::assert_matches;

    #[test]
    fn clone() {
        let data: Data = Data::String("abc".to_string());
        let any: Capability = data.into();
        let clone = any.clone();
        let data_back = assert_matches!(any, Capability::Data(d) => d);
        let clone_data_back = assert_matches!(clone, Capability::Data(d) => d);
        assert_eq!(data_back, clone_data_back);
    }
}
