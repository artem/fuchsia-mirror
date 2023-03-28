// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{mapping::replace, EnvironmentContext};
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;

pub(crate) fn data(ctx: &EnvironmentContext, value: Value) -> Option<Value> {
    lazy_static! {
        static ref REGEX: Regex = Regex::new(r"\$(DATA)").unwrap();
    }

    replace(&*REGEX, || ctx.get_data_path(), value)
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use super::*;
    use crate::{environment::ExecutableKind, ConfigMap};

    #[test]
    fn test_mapper() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            Default::default(),
            ConfigMap::default(),
            None,
        );
        let value =
            ctx.get_data_path().expect("Getting data directory").to_string_lossy().to_string();
        let test = Value::String("$DATA".to_string());
        assert_eq!(data(&ctx, test), Some(Value::String(value)));
    }

    #[test]
    fn test_mapper_multiple() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            Default::default(),
            ConfigMap::default(),
            None,
        );
        let value =
            ctx.get_data_path().expect("Getting data directory").to_string_lossy().to_string();
        let test = Value::String("$DATA/$DATA".to_string());
        assert_eq!(data(&ctx, test), Some(Value::String(format!("{}/{}", value, value))));
    }

    #[test]
    fn test_mapper_returns_pass_through() {
        let ctx = EnvironmentContext::isolated(
            ExecutableKind::Test,
            "/tmp".into(),
            Default::default(),
            ConfigMap::default(),
            None,
        );
        let test = Value::String("$WHATEVER".to_string());
        assert_eq!(data(&ctx, test), Some(Value::String("$WHATEVER".to_string())));
    }
}
