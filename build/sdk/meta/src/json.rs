// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{from_reader, from_str, to_string, to_value};
use std::io::Read;
use thiserror::Error;
use url::Url;
use valico::common::error::ValicoError;
use valico::json_schema;

pub mod schema {
    pub const COMMON: &str = include_str!("../common.json");
    pub const HARDWARE_V1: &str = include_str!("../hardware-f6f47515.json");
}

/// The various types of errors raised by this module.
#[derive(Debug, Error)]
enum Error {
    #[error("invalid JSON schema: {:?}", _0)]
    SchemaInvalid(valico::json_schema::schema::SchemaError),
    #[error("could not validate file: {}", errors)]
    #[allow(dead_code)] // The compiler complains about the variant not being constructed.
    JsonFileInvalid { errors: String },
}

impl From<valico::json_schema::schema::SchemaError> for Error {
    fn from(err: valico::json_schema::schema::SchemaError) -> Self {
        Error::SchemaInvalid(err)
    }
}

/// Generates user-friendly messages for validation errors.
fn format_valico_error(error: &Box<dyn ValicoError>) -> String {
    // $title[at $path][: $detail] ($code)
    let mut result = String::new();
    result.push_str(error.get_title());
    let path = error.get_path();
    if !path.is_empty() {
        result.push_str(" at ");
        result.push_str(path);
    }
    if let Some(detail) = error.get_detail() {
        result.push_str(": ");
        result.push_str(detail);
    }
    result.push_str(" (");
    result.push_str(error.get_code());
    result.push_str(")");
    result
}

/// Generates the missing schema reference message for the given URL.
fn format_missing_refs(url: &Url) -> String {
    let mut result = String::new();
    result.push_str("Missing schema reference: ");
    result.push_str(&url.to_string());
    result
}

/// Augments metadata representations with utility methods to serialize/deserialize and validate
/// their contents.
pub trait JsonObject: for<'a> Deserialize<'a> + Serialize + Sized {
    /// Creates a new instance from its raw data.
    fn new<R: Read>(source: R) -> Result<Self> {
        Ok(from_reader(source)?)
    }

    /// Returns the schema matching the object type.
    fn get_schema() -> &'static str;

    /// Returns a list of schemas referenced by this schema.
    fn get_referenced_schemata() -> &'static [&'static str] {
        &[schema::COMMON]
    }

    // Returns schema ID.
    fn get_schema_id() -> Result<String> {
        let schema = from_str(Self::get_schema())?;
        let mut scope = json_schema::Scope::new();
        let url = scope.compile(schema, /*ban_unknown=*/ true).map_err(Error::SchemaInvalid)?;
        Ok(url.to_string())
    }

    /// Checks whether the object satisfies its associated JSON schema.
    fn validate(&self) -> Result<()> {
        let schema = from_str(Self::get_schema())?;
        let mut scope = json_schema::Scope::new();

        // Add the schema including all the common definitions.
        for schema in Self::get_referenced_schemata() {
            scope
                .compile(from_str(schema)?, /*ban_unknown=*/ true)
                .map_err(Error::SchemaInvalid)?;
        }

        let validator = scope
            .compile_and_return(schema, /*ban_unknown=*/ true)
            .map_err(Error::SchemaInvalid)?;
        let value = to_value(self)?;
        let result = validator.validate(&value);
        if !result.is_strictly_valid() {
            let mut error_messages: Vec<String> =
                result.errors.iter().map(format_valico_error).collect();
            error_messages.sort_unstable();

            let mut missing_refs: Vec<String> =
                result.missing.iter().map(format_missing_refs).collect();
            missing_refs.sort_unstable();
            error_messages.append(&mut missing_refs);

            return Err(Error::JsonFileInvalid { errors: error_messages.join(", ") }.into());
        }
        Ok(())
    }

    /// Serializes the object into its string representation.
    fn to_string(&self) -> Result<String> {
        Ok(to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Deserialize, Serialize)]
    struct Metadata {
        target: String,
    }

    impl JsonObject for Metadata {
        fn get_schema() -> &'static str {
            r#"{
                "$schema": "http://json-schema.org/draft-04/schema#",
                "id": "http://fuchsia.com/schemas/sdk/test_metadata.json",
                "properties": {
                    "target": {
                        "$ref": "common.json#/definitions/target_arch"
                    }
                }
            }"#
        }
    }

    #[test]
    fn test_id() {
        assert_eq!(
            String::from("http://fuchsia.com/schemas/sdk/test_metadata.json"),
            Metadata::get_schema_id().expect("schema id")
        );
    }

    #[test]
    /// Checks that references to common.json are properly resolved.
    fn test_common_reference() {
        let metadata = Metadata {
            target: "y128".to_string(), // Not a valid architecture.
        };
        let result = metadata.validate();
        assert!(result.is_err(), "Validation did not respect common schema.");
    }
}
