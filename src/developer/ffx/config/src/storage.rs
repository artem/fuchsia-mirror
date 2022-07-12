// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::api::query::SelectMode,
    crate::environment::Environment,
    crate::nested::{nested_get, nested_remove, nested_set},
    crate::ConfigLevel,
    anyhow::{Context, Result},
    config_macros::include_default,
    serde_json::{Map, Value},
    std::{
        fmt,
        fs::{File, OpenOptions},
        io::{BufReader, BufWriter, Read, Write},
        path::Path,
    },
};

#[derive(Debug, Clone)]
pub struct Config {
    default: Option<Value>,
    global: Option<Value>,
    user: Option<Value>,
    build: Option<Value>,
    runtime: Option<Value>,
}

struct PriorityIterator<'a> {
    curr: Option<ConfigLevel>,
    config: &'a Config,
}

impl<'a> Iterator for PriorityIterator<'a> {
    type Item = &'a Option<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        use ConfigLevel::*;
        self.curr = ConfigLevel::next(self.curr);
        match self.curr {
            Some(Runtime) => Some(&self.config.runtime),
            Some(Build) => Some(&self.config.build),
            Some(User) => Some(&self.config.user),
            Some(Global) => Some(&self.config.global),
            Some(Default) => Some(&self.config.default),
            None => None,
        }
    }
}

/// Reads a JSON formatted reader permissively, returning None if for whatever reason
/// the file couldn't be read.
///
/// If the JSON is malformed, it will just get overwritten if set is ever used.
/// (TODO: Validate above assumptions)
fn read_json(file: impl Read) -> Option<Value> {
    serde_json::from_reader(file).ok()
}

/// Takes an optional path-like object and maps it to a buffer reader of that
/// file.
fn reader(path: Option<impl AsRef<Path>>) -> Result<Option<BufReader<File>>> {
    match path {
        Some(p) => OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&p)
            .map(|f| Some(BufReader::new(f)))
            .context("opening read buffer"),
        None => Ok(None),
    }
}

fn write_json<W: Write>(file: Option<W>, value: Option<&Value>) -> Result<()> {
    match (value, file) {
        (Some(v), Some(mut f)) => {
            serde_json::to_writer_pretty(&mut f, v).context("writing config file")?;
            f.flush().map_err(Into::into)
        }
        (_, _) => {
            // If either value or file are None, then return Ok(()). File being none will
            // presume the user doesn't want to save at this level.
            Ok(())
        }
    }
}

/// Atomically write to the file by creating a temporary file and passing it
/// to the closure, and atomically rename it to the destination file.
fn with_writer<F>(path: Option<&Path>, f: F) -> Result<()>
where
    F: FnOnce(Option<BufWriter<&mut tempfile::NamedTempFile>>) -> Result<()>,
{
    if let Some(path) = path {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;

        f(Some(BufWriter::new(&mut tmp)))?;

        tmp.persist(path)?;

        Ok(())
    } else {
        f(None)
    }
}

impl Config {
    fn new(
        global: Option<Value>,
        build: Option<Value>,
        user: Option<Value>,
        runtime: Option<Value>,
    ) -> Self {
        Self { user, build, global, runtime, default: include_default!() }
    }

    pub(crate) fn from_env(
        env: &Environment,
        build_dir: Option<&Path>,
        runtime: Option<Value>,
    ) -> Result<Self> {
        let build_conf = env.get_build(build_dir);

        let user = reader(env.get_user())?.and_then(read_json);
        let build = reader(build_conf)?.and_then(read_json);
        let global = reader(env.get_global())?.and_then(read_json);

        Ok(Self::new(global, build, user, runtime))
    }

    fn write<W: Write>(&self, global: Option<W>, build: Option<W>, user: Option<W>) -> Result<()> {
        write_json(user, self.user.as_ref())?;
        write_json(build, self.build.as_ref())?;
        write_json(global, self.global.as_ref())?;
        Ok(())
    }

    pub(crate) fn save(
        &self,
        global: Option<&Path>,
        build: Option<&Path>,
        user: Option<&Path>,
    ) -> Result<()> {
        // First save the config to a temp file in the same location as the file, then atomically
        // rename the file to the final location to avoid partially written files.

        with_writer(global, |global| {
            with_writer(build, |build| with_writer(user, |user| self.write(global, build, user)))
        })
    }

    pub fn get_level(&self, level: ConfigLevel) -> Option<&Value> {
        match level {
            ConfigLevel::Runtime => self.runtime.as_ref(),
            ConfigLevel::User => self.user.as_ref(),
            ConfigLevel::Build => self.build.as_ref(),
            ConfigLevel::Global => self.global.as_ref(),
            ConfigLevel::Default => self.default.as_ref(),
        }
    }

    pub fn get_in_level(&self, key: &str, level: ConfigLevel) -> Option<Value> {
        let key_vec: Vec<&str> = key.split('.').collect();
        nested_get(self.get_level(level), key_vec.get(0)?, &key_vec[1..]).cloned()
    }

    pub fn get(&self, key: &str, select: SelectMode) -> Option<Value> {
        let key_vec: Vec<&str> = key.split('.').collect();
        match select {
            SelectMode::First => self
                .iter()
                .find_map(|c| nested_get(c.as_ref(), key_vec.get(0)?, &key_vec[1..]))
                .cloned(),
            SelectMode::All => {
                let result: Vec<Value> = self
                    .iter()
                    .filter_map(|c| nested_get(c.as_ref(), key_vec.get(0)?, &key_vec[1..]))
                    .cloned()
                    .collect();
                if result.len() > 0 {
                    Some(Value::Array(result))
                } else {
                    None
                }
            }
        }
    }

    pub fn set(&mut self, key: &str, level: ConfigLevel, value: Value) -> Result<bool> {
        anyhow::ensure!(level != ConfigLevel::Default, "Can't set values at Default level");
        let key_vec: Vec<&str> = key.split('.').collect();
        let config_changed =
            nested_set(&mut self.get_level_map(level), key_vec[0], &key_vec[1..], value);
        Ok(config_changed)
    }

    pub fn remove(&mut self, key: &str, level: ConfigLevel) -> Result<()> {
        anyhow::ensure!(level != ConfigLevel::Default, "Can't remove values at Default level");
        let key_vec: Vec<&str> = key.split('.').collect();
        nested_remove(&mut self.get_level_map(level), key_vec[0], &key_vec[1..])
    }

    fn iter(&self) -> PriorityIterator<'_> {
        PriorityIterator { curr: None, config: self }
    }

    fn get_level_map(&mut self, level: ConfigLevel) -> &mut Map<String, Value> {
        let config = match level {
            ConfigLevel::Runtime => &mut self.runtime,
            ConfigLevel::User => &mut self.user,
            ConfigLevel::Build => &mut self.build,
            ConfigLevel::Global => &mut self.global,
            ConfigLevel::Default => &mut self.default,
        };
        // Ensure current value is always a map.
        match config {
            Some(v) => {
                if !v.is_object() {
                    // This must be a map.  Will override any literals or arrays.
                    *config = Some(Value::Object(Map::new()));
                }
            }
            None => *config = Some(Value::Object(Map::new())),
        }
        // Ok to expect as this is ensured above.
        config
            .as_mut()
            .expect("uninitialzed configuration")
            .as_object_mut()
            .expect("unable to initialize configuration map")
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "FFX configuration can come from several places and has an inherent priority assigned\n\
            to the different ways the configuration is gathered. A configuration key can be set\n\
            in multiple locations but the first value found is returned. The following output\n\
            shows the locations checked in descending priority order.\n"
        )?;
        let mut iterator = self.iter();
        while let Some(next) = iterator.next() {
            if let Some(level) = iterator.curr {
                match level {
                    ConfigLevel::Runtime => {
                        write!(f, "Runtime Configuration")?;
                    }
                    ConfigLevel::User => {
                        write!(f, "User Configuration")?;
                    }
                    ConfigLevel::Build => {
                        write!(f, "Build Configuration")?;
                    }
                    ConfigLevel::Global => {
                        write!(f, "Global Configuration")?;
                    }
                    ConfigLevel::Default => {
                        write!(f, "Default Configuration")?;
                    }
                };
            }
            if let Some(value) = next {
                writeln!(f, "")?;
                writeln!(f, "{}", serde_json::to_string_pretty(&value).unwrap())?;
            } else {
                writeln!(f, ": {}", "none")?;
            }
            writeln!(f, "")?;
        }
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::nested::RecursiveMap;
    use regex::Regex;
    use serde_json::json;
    use std::io::{BufReader, BufWriter};

    const ERROR: &'static str = "0";

    const USER: &'static str = r#"
        {
            "name": "User"
        }"#;

    const BUILD: &'static str = r#"
        {
            "name": "Build"
        }"#;

    const GLOBAL: &'static str = r#"
        {
            "name": "Global"
        }"#;

    const DEFAULT: &'static str = r#"
        {
            "name": "Default"
        }"#;

    const RUNTIME: &'static str = r#"
        {
            "name": "Runtime"
        }"#;

    const MAPPED: &'static str = r#"
        {
            "name": "TEST_MAP"
        }"#;

    const NESTED: &'static str = r#"
        {
            "name": {
               "nested": "Nested"
            }
        }"#;

    const DEEP: &'static str = r#"
        {
            "name": {
               "nested": {
                    "deep": {
                        "name": "TEST_MAP"
                    }
               }
            }
        }"#;

    #[test]
    fn test_persistent_build() -> Result<()> {
        let mut user_file = String::from(USER);
        let mut build_file = String::from(BUILD);
        let mut global_file = String::from(GLOBAL);

        let persistent_config = Config::new(
            read_json(BufReader::new(global_file.as_bytes())),
            read_json(BufReader::new(build_file.as_bytes())),
            read_json(BufReader::new(user_file.as_bytes())),
            None,
        );

        let value = persistent_config.get("name", SelectMode::First);
        assert!(value.is_some());
        assert_eq!(value.unwrap(), Value::String(String::from("Build")));

        let mut user_file_out = String::new();
        let mut build_file_out = String::new();
        let mut global_file_out = String::new();

        unsafe {
            persistent_config.write(
                Some(BufWriter::new(global_file_out.as_mut_vec())),
                Some(BufWriter::new(build_file_out.as_mut_vec())),
                Some(BufWriter::new(user_file_out.as_mut_vec())),
            )?;
        }

        // Remove whitespace
        user_file.retain(|c| !c.is_whitespace());
        build_file.retain(|c| !c.is_whitespace());
        global_file.retain(|c| !c.is_whitespace());
        user_file_out.retain(|c| !c.is_whitespace());
        build_file_out.retain(|c| !c.is_whitespace());
        global_file_out.retain(|c| !c.is_whitespace());

        assert_eq!(user_file, user_file_out);
        assert_eq!(build_file, build_file_out);
        assert_eq!(global_file, global_file_out);

        Ok(())
    }

    #[test]
    fn test_priority_iterator() -> Result<()> {
        let test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: Some(serde_json::from_str(RUNTIME)?),
        };

        let mut test_iter = test.iter();
        assert_eq!(test_iter.next(), Some(&test.runtime));
        assert_eq!(test_iter.next(), Some(&test.build));
        assert_eq!(test_iter.next(), Some(&test.user));
        assert_eq!(test_iter.next(), Some(&test.global));
        assert_eq!(test_iter.next(), Some(&test.default));
        Ok(())
    }

    #[test]
    fn test_priority_iterator_with_nones() -> Result<()> {
        let test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: None,
            global: None,
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };

        let mut test_iter = test.iter();
        assert_eq!(test_iter.next(), Some(&test.runtime));
        assert_eq!(test_iter.next(), Some(&test.build));
        assert_eq!(test_iter.next(), Some(&test.user));
        assert_eq!(test_iter.next(), Some(&test.global));
        assert_eq!(test_iter.next(), Some(&test.default));
        Ok(())
    }

    #[test]
    fn test_get() -> Result<()> {
        let test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };

        let value = test.get("name", SelectMode::First);
        assert!(value.is_some());
        assert_eq!(value.unwrap(), Value::String(String::from("Build")));

        let test_build = Config {
            user: Some(serde_json::from_str(USER)?),
            build: None,
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };

        let value_build = test_build.get("name", SelectMode::First);
        assert!(value_build.is_some());
        assert_eq!(value_build.unwrap(), Value::String(String::from("User")));

        let test_global = Config {
            user: None,
            build: None,
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };

        let value_global = test_global.get("name", SelectMode::First);
        assert!(value_global.is_some());
        assert_eq!(value_global.unwrap(), Value::String(String::from("Global")));

        let test_default = Config {
            user: None,
            build: None,
            global: None,
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };

        let value_default = test_default.get("name", SelectMode::First);
        assert!(value_default.is_some());
        assert_eq!(value_default.unwrap(), Value::String(String::from("Default")));

        let test_none =
            Config { user: None, build: None, global: None, default: None, runtime: None };

        let value_none = test_none.get("name", SelectMode::First);
        assert!(value_none.is_none());
        Ok(())
    }

    #[test]
    fn test_set_non_map_value() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(ERROR)?),
            build: None,
            global: None,
            default: None,
            runtime: None,
        };
        test.set("name", ConfigLevel::User, Value::String(String::from("whatever")))?;
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, Some(Value::String(String::from("whatever"))));
        Ok(())
    }

    #[test]
    fn test_get_nonexistent_config() -> Result<()> {
        let test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };
        let value = test.get("field that does not exist", SelectMode::First);
        assert!(value.is_none());
        Ok(())
    }

    #[test]
    fn test_set() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };
        test.set("name", ConfigLevel::Build, Value::String(String::from("build-test")))?;
        let value = test.get("name", SelectMode::First);
        assert!(value.is_some());
        assert_eq!(value.unwrap(), Value::String(String::from("build-test")));
        Ok(())
    }

    #[test]
    fn test_set_twice_does_not_change_config() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };
        assert!(test.set(
            "name",
            ConfigLevel::Build,
            Value::String(String::from("build-test1"))
        )?);
        assert_eq!(
            test.get("name", SelectMode::First).unwrap(),
            Value::String(String::from("build-test1"))
        );

        assert!(!test.set(
            "name",
            ConfigLevel::Build,
            Value::String(String::from("build-test1"))
        )?);
        assert_eq!(
            test.get("name", SelectMode::First).unwrap(),
            Value::String(String::from("build-test1"))
        );

        assert!(test.set(
            "name",
            ConfigLevel::Build,
            Value::String(String::from("build-test2"))
        )?);
        assert_eq!(
            test.get("name", SelectMode::First).unwrap(),
            Value::String(String::from("build-test2"))
        );

        Ok(())
    }

    #[test]
    fn test_set_build_from_none() -> Result<()> {
        let mut test =
            Config { user: None, build: None, global: None, default: None, runtime: None };
        let value_none = test.get("name", SelectMode::First);
        assert!(value_none.is_none());
        let error_set =
            test.set("name", ConfigLevel::Default, Value::String(String::from("default")));
        assert!(error_set.is_err(), "Should not be able to set default values at runtime");
        let value_default = test.get("name", SelectMode::First);
        assert!(
            value_default.is_none(),
            "Default value should be unset after failed attempt to set it"
        );
        test.set("name", ConfigLevel::Global, Value::String(String::from("global")))?;
        let value_global = test.get("name", SelectMode::First);
        assert!(value_global.is_some());
        assert_eq!(value_global.unwrap(), Value::String(String::from("global")));
        test.set("name", ConfigLevel::User, Value::String(String::from("user")))?;
        let value_user = test.get("name", SelectMode::First);
        assert!(value_user.is_some());
        assert_eq!(value_user.unwrap(), Value::String(String::from("user")));
        test.set("name", ConfigLevel::Build, Value::String(String::from("build")))?;
        let value_build = test.get("name", SelectMode::First);
        assert!(value_build.is_some());
        assert_eq!(value_build.unwrap(), Value::String(String::from("build")));
        Ok(())
    }

    #[test]
    fn test_remove() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };
        test.remove("name", ConfigLevel::User)?;
        let user_value = test.get("name", SelectMode::First);
        assert!(user_value.is_some());
        assert_eq!(user_value.unwrap(), Value::String(String::from("Build")));
        test.remove("name", ConfigLevel::Build)?;
        let global_value = test.get("name", SelectMode::First);
        assert!(global_value.is_some());
        assert_eq!(global_value.unwrap(), Value::String(String::from("Global")));
        test.remove("name", ConfigLevel::Global)?;
        let default_value = test.get("name", SelectMode::First);
        assert!(default_value.is_some());
        assert_eq!(default_value.unwrap(), Value::String(String::from("Default")));
        let error_removed = test.remove("name", ConfigLevel::Default);
        assert!(error_removed.is_err(), "Should not be able to remove a default value");
        let default_value = test.get("name", SelectMode::First);
        assert_eq!(
            default_value,
            Some(Value::String(String::from("Default"))),
            "value should still be default after trying to remove it (was {:?})",
            default_value
        );
        Ok(())
    }

    #[test]
    fn test_default() {
        let test = Config::new(None, None, None, None);
        let default_value = test.get("log.enabled", SelectMode::First);
        assert_eq!(
            default_value.unwrap(),
            Value::Array(vec![Value::String("$FFX_LOG_ENABLED".to_string()), Value::Bool(true)])
        );
    }

    #[test]
    fn test_display() -> Result<()> {
        let test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: None,
        };
        let output = format!("{}", test);
        assert!(output.len() > 0);
        let user_reg = Regex::new("\"name\": \"User\"").expect("test regex");
        assert_eq!(1, user_reg.find_iter(&output).count());
        let build_reg = Regex::new("\"name\": \"Build\"").expect("test regex");
        assert_eq!(1, build_reg.find_iter(&output).count());
        let global_reg = Regex::new("\"name\": \"Global\"").expect("test regex");
        assert_eq!(1, global_reg.find_iter(&output).count());
        let default_reg = Regex::new("\"name\": \"Default\"").expect("test regex");
        assert_eq!(1, default_reg.find_iter(&output).count());
        Ok(())
    }

    fn test_map(value: Value) -> Option<Value> {
        value
            .as_str()
            .map(|s| match s {
                "TEST_MAP" => Value::String("passed".to_string()),
                _ => Value::String("failed".to_string()),
            })
            .or(Some(value))
    }

    #[test]
    fn test_mapping() -> Result<()> {
        let test = Config {
            user: Some(serde_json::from_str(MAPPED)?),
            build: None,
            global: None,
            default: None,
            runtime: None,
        };
        let test_mapping = "TEST_MAP".to_string();
        let test_passed = "passed".to_string();
        let mapped_value = test.get("name", SelectMode::First).as_ref().recursive_map(&test_map);
        assert_eq!(mapped_value, Some(Value::String(test_passed)));
        let identity_value = test.get("name", SelectMode::First);
        assert_eq!(identity_value, Some(Value::String(test_mapping)));
        Ok(())
    }

    #[test]
    fn test_nested_get() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: None,
            runtime: Some(serde_json::from_str(NESTED)?),
        };
        let value = test.get("name.nested", SelectMode::First);
        assert_eq!(value, Some(Value::String("Nested".to_string())));
        Ok(())
    }

    #[test]
    fn test_nested_get_should_return_sub_tree() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: Some(serde_json::from_str(NESTED)?),
        };
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, Some(serde_json::from_str("{\"nested\": \"Nested\"}")?));
        Ok(())
    }

    #[test]
    fn test_nested_get_should_return_full_match() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: Some(serde_json::from_str(NESTED)?),
            runtime: Some(serde_json::from_str(RUNTIME)?),
        };
        let value = test.get("name.nested", SelectMode::First);
        assert_eq!(value, Some(Value::String("Nested".to_string())));
        Ok(())
    }

    #[test]
    fn test_nested_get_should_map_values_in_sub_tree() -> Result<()> {
        let test = Config {
            user: None,
            build: None,
            global: None,
            default: Some(serde_json::from_str(NESTED)?),
            runtime: Some(serde_json::from_str(DEEP)?),
        };
        let value = test.get("name.nested", SelectMode::First).as_ref().recursive_map(&test_map);
        assert_eq!(value, Some(serde_json::from_str("{\"deep\": {\"name\": \"passed\"}}")?));
        Ok(())
    }

    #[test]
    fn test_nested_set_from_none() -> Result<()> {
        let mut test =
            Config { user: None, build: None, global: None, default: None, runtime: None };
        test.set("name.nested", ConfigLevel::User, Value::Bool(false))?;
        let nested_value = test.get("name", SelectMode::First);
        assert_eq!(nested_value, Some(serde_json::from_str("{\"nested\": false}")?));
        Ok(())
    }

    #[test]
    fn test_nested_set_from_already_populated_tree() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(NESTED)?),
            build: None,
            global: None,
            default: None,
            runtime: None,
        };
        test.set("name.updated", ConfigLevel::User, Value::Bool(true))?;
        let expected = json!({
           "nested": "Nested",
           "updated": true
        });
        let nested_value = test.get("name", SelectMode::First);
        assert_eq!(nested_value, Some(expected));
        Ok(())
    }

    #[test]
    fn test_nested_set_override_literals() -> Result<()> {
        let mut test = Config {
            user: Some(json!([])),
            build: None,
            global: None,
            default: None,
            runtime: None,
        };
        test.set("name.updated", ConfigLevel::User, Value::Bool(true))?;
        let expected = json!({
           "updated": true
        });
        let nested_value = test.get("name", SelectMode::First);
        assert_eq!(nested_value, Some(expected));
        test.set("name.updated", ConfigLevel::User, serde_json::from_str(NESTED)?)?;
        let nested_value = test.get("name.updated.name.nested", SelectMode::First);
        assert_eq!(nested_value, Some(Value::String(String::from("Nested"))));
        Ok(())
    }

    #[test]
    fn test_nested_remove_from_none() -> Result<()> {
        let mut test =
            Config { user: None, build: None, global: None, default: None, runtime: None };
        let result = test.remove("name.nested", ConfigLevel::User);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_nested_remove_throws_error_if_key_not_found() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(NESTED)?),
            build: None,
            global: None,
            default: None,
            runtime: None,
        };
        let result = test.remove("name.unknown", ConfigLevel::User);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_nested_remove_deletes_literals() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(DEEP)?),
            build: None,
            global: None,
            default: None,
            runtime: None,
        };
        test.remove("name.nested.deep.name", ConfigLevel::User)?;
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, None);
        Ok(())
    }

    #[test]
    fn test_nested_remove_deletes_subtrees() -> Result<()> {
        let mut test = Config {
            user: Some(serde_json::from_str(DEEP)?),
            build: None,
            global: None,
            default: None,
            runtime: None,
        };
        test.remove("name.nested", ConfigLevel::User)?;
        let value = test.get("name", SelectMode::First);
        assert_eq!(value, None);
        Ok(())
    }

    #[test]
    fn test_additive_mode() -> Result<()> {
        let test = Config {
            user: Some(serde_json::from_str(USER)?),
            build: Some(serde_json::from_str(BUILD)?),
            global: Some(serde_json::from_str(GLOBAL)?),
            default: Some(serde_json::from_str(DEFAULT)?),
            runtime: Some(serde_json::from_str(RUNTIME)?),
        };
        let value = test.get("name", SelectMode::All);
        match value {
            Some(Value::Array(v)) => {
                assert_eq!(v.len(), 5);
                let mut v = v.into_iter();
                assert_eq!(v.next(), Some(Value::String("Runtime".to_string())));
                assert_eq!(v.next(), Some(Value::String("Build".to_string())));
                assert_eq!(v.next(), Some(Value::String("User".to_string())));
                assert_eq!(v.next(), Some(Value::String("Global".to_string())));
                assert_eq!(v.next(), Some(Value::String("Default".to_string())));
            }
            _ => anyhow::bail!("additive mode should return a Value::Array full of all values."),
        }
        Ok(())
    }
}
