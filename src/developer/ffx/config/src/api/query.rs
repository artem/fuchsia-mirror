// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    api::ConfigResult, nested::RecursiveMap, validate_type, ConfigError, ConfigLevel, ConfigValue,
    Environment, EnvironmentContext, ValueStrategy,
};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::{
    default::Default,
    path::{Path, PathBuf},
};
use tracing::debug;

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum SelectMode {
    First,
    All,
}

impl Default for SelectMode {
    fn default() -> Self {
        SelectMode::First
    }
}

/// Overrides the build directory search
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum BuildOverride<'a> {
    /// Do not search a build directory, even if a 'default' one is known.
    NoBuild,
    /// Use a specific path to look up the build directory, ignoring the default.
    Path(&'a Path),
}

#[derive(Debug, Default, Clone)]
pub struct ConfigQuery<'a> {
    pub name: Option<&'a str>,
    pub level: Option<ConfigLevel>,
    pub build: Option<BuildOverride<'a>>,
    pub select: SelectMode,
    pub ctx: Option<&'a EnvironmentContext>,
}

impl<'a> ConfigQuery<'a> {
    pub fn new(
        name: Option<&'a str>,
        level: Option<ConfigLevel>,
        build: Option<BuildOverride<'a>>,
        select: SelectMode,
        ctx: Option<&'a EnvironmentContext>,
    ) -> Self {
        Self { ctx, name, level, build, select }
    }

    /// Adds the given name to the query and returns a new composed query.
    pub fn name(self, name: Option<&'a str>) -> Self {
        Self { name, ..self }
    }
    /// Adds the given level to the query and returns a new composed query.
    pub fn level(self, level: Option<ConfigLevel>) -> Self {
        Self { level, ..self }
    }
    /// Adds the given build to the query and returns a new composed query.
    pub fn build(self, build: Option<BuildOverride<'a>>) -> Self {
        Self { build, ..self }
    }
    /// Adds the given select mode to the query and returns a new composed query.
    pub fn select(self, select: SelectMode) -> Self {
        Self { select, ..self }
    }
    /// Use the given environment context instead of the global one and returns
    /// a new composed query.
    pub fn context(self, ctx: Option<&'a EnvironmentContext>) -> Self {
        Self { ctx, ..self }
    }

    fn get_env_context(&self) -> Result<EnvironmentContext> {
        match self.ctx {
            Some(ctx) => Ok(ctx.clone()),
            None => crate::global_env_context().context("No configured global environment"),
        }
    }

    async fn get_env(&self) -> Result<Environment> {
        match self.ctx {
            Some(ctx) => ctx.load(),
            None => crate::global_env().context("No configured global environment"),
        }
    }

    fn get_config(&self, env: Environment) -> ConfigResult {
        debug!("{self}");
        let config = env.config_from_cache(self.build)?;
        let read_guard = config.read().map_err(|_| anyhow!("config read guard"))?;
        let result = match self {
            Self { name: Some(name), level: None, select, .. } => read_guard.get(*name, *select),
            Self { name: Some(name), level: Some(level), .. } => {
                read_guard.get_in_level(*name, *level)
            }
            Self { name: None, level: Some(level), .. } => {
                read_guard.get_level(*level).cloned().map(Value::Object)
            }
            _ => bail!("Invalid query: {self:?}"),
        };
        Ok(result.into())
    }

    /// Get a value with as little processing as possible
    pub fn get_raw<T>(&self) -> std::result::Result<T, T::Error>
    where
        T: TryFrom<ConfigValue> + ValueStrategy,
        <T as std::convert::TryFrom<ConfigValue>>::Error: std::convert::From<ConfigError>,
    {
        let ctx = self.get_env_context().map_err(|e| e.into())?;
        T::validate_query(self)?;
        self.get_config(ctx.load().map_err(|e| e.into())?)
            .map_err(|e| e.into())?
            .recursive_map(&validate_type::<T>)
            .try_into()
    }

    /// Get a value with the normal processing of substitution strings
    pub fn get<T>(&self) -> std::result::Result<T, T::Error>
    where
        T: TryFrom<ConfigValue> + ValueStrategy,
        <T as std::convert::TryFrom<ConfigValue>>::Error: std::convert::From<ConfigError>,
    {
        use crate::mapping::*;

        let ctx = self.get_env_context().map_err(|e| e.into())?;
        T::validate_query(self)?;

        self.get_config(ctx.load().map_err(|e| e.into())?)
            .map_err(|e| e.into())?
            .recursive_map(&|val| runtime(&ctx, val))
            .recursive_map(&|val| cache(&ctx, val))
            .recursive_map(&|val| data(&ctx, val))
            .recursive_map(&|val| config(&ctx, val))
            .recursive_map(&|val| home(&ctx, val))
            .recursive_map(&|val| build(&ctx, val))
            .recursive_map(&|val| workspace(&ctx, val))
            .recursive_map(&|val| env_var(&ctx, val))
            .recursive_map(&T::handle_arrays)
            .recursive_map(&validate_type::<T>)
            .try_into()
    }

    /// Get a value with normal processing, but verifying that it's a file that exists.
    pub async fn get_file<T>(&self) -> std::result::Result<T, T::Error>
    where
        T: TryFrom<ConfigValue> + ValueStrategy,
        <T as std::convert::TryFrom<ConfigValue>>::Error: std::convert::From<ConfigError>,
    {
        use crate::mapping::*;

        let ctx = self.get_env_context().map_err(|e| e.into())?;
        T::validate_query(self)?;
        self.get_config(ctx.load().map_err(|e| e.into())?)
            .map_err(|e| e.into())?
            .recursive_map(&|val| runtime(&ctx, val))
            .recursive_map(&|val| cache(&ctx, val))
            .recursive_map(&|val| data(&ctx, val))
            .recursive_map(&|val| config(&ctx, val))
            .recursive_map(&|val| home(&ctx, val))
            .recursive_map(&|val| build(&ctx, val))
            .recursive_map(&|val| workspace(&ctx, val))
            .recursive_map(&|val| env_var(&ctx, val))
            .recursive_map(&T::handle_arrays)
            .recursive_map(&file_check)
            .try_into()
    }

    fn validate_write_query(&self) -> Result<(&str, ConfigLevel)> {
        match self {
            ConfigQuery { name: None, .. } => {
                bail!("Name of configuration is required to write to a value")
            }
            ConfigQuery { level: None, .. } => {
                bail!("Level of configuration is required to write to a value")
            }
            ConfigQuery { level: Some(level), .. } if level == &ConfigLevel::Default => {
                bail!("Cannot override defaults")
            }
            ConfigQuery { name: Some(key), level: Some(level), .. } => Ok((*key, *level)),
        }
    }

    /// Set the queried location to the given Value.
    pub async fn set(&self, value: Value) -> Result<()> {
        tracing::debug!("Setting config value");
        let (key, level) = self.validate_write_query()?;
        let mut env = self.get_env().await?;
        tracing::debug!("Config set got environment");
        env.populate_defaults(&level).await?;
        tracing::debug!("Config set defaults populated");
        let config = env.config_from_cache(self.build)?;
        tracing::debug!("Config set got value from cache");
        let mut write_guard = config.write().map_err(|_| anyhow!("config write guard"))?;
        tracing::debug!("Config set got write guard");
        write_guard.set(key, level, value)?;
        tracing::debug!("Config set performed");
        write_guard.save().await?;
        tracing::debug!("Config set saved");
        Ok(())
    }

    /// Remove the value at the queried location.
    pub async fn remove(&self) -> Result<()> {
        let (key, level) = self.validate_write_query()?;
        let env = self.get_env().await?;
        let config = env.config_from_cache(self.build)?;
        let mut write_guard = config.write().map_err(|_| anyhow!("config write guard"))?;
        write_guard.remove(key, level)?;
        write_guard.save().await
    }

    /// Add this value at the queried location as an array item, converting the location to an array
    /// if necessary.
    pub async fn add(&self, value: Value) -> Result<()> {
        let (key, level) = self.validate_write_query()?;
        let mut env = self.get_env().await?;
        env.populate_defaults(&level).await?;
        let config = env.config_from_cache(self.build)?;
        let mut write_guard = config.write().map_err(|_| anyhow!("config write guard"))?;
        if let Some(mut current) = write_guard.get_in_level(key, level) {
            if current.is_object() {
                bail!("cannot add a value to a subtree");
            } else {
                match current.as_array_mut() {
                    Some(v) => {
                        v.push(value);
                        write_guard.set(key, level, Value::Array(v.to_vec()))?
                    }
                    None => write_guard.set(key, level, Value::Array(vec![current, value]))?,
                }
            }
        } else {
            write_guard.set(key, level, value)?
        };

        write_guard.save().await
    }
}

impl<'a> std::fmt::Display for ConfigQuery<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { name, level, build, select, .. } = self;
        let mut sep = "";
        if let Some(name) = name {
            write!(f, "{sep}key='{name}'")?;
            sep = ", ";
        }
        if let Some(level) = level {
            write!(f, "{sep}level={level:?}")?;
            sep = ", ";
        }
        if let Some(build) = build {
            write!(f, "{sep}build_override={build:?}")?;
            sep = ", ";
        }
        write!(f, "{sep}select={select:?}")
    }
}

impl<'a> From<&'a Path> for BuildOverride<'a> {
    fn from(s: &'a Path) -> Self {
        BuildOverride::Path(s)
    }
}
impl<'a> From<&'a PathBuf> for BuildOverride<'a> {
    fn from(s: &'a PathBuf) -> Self {
        BuildOverride::Path(&s)
    }
}
impl<'a> From<&'a str> for BuildOverride<'a> {
    fn from(s: &'a str) -> Self {
        BuildOverride::Path(&Path::new(s))
    }
}
impl<'a> From<&'a String> for BuildOverride<'a> {
    fn from(s: &'a String) -> Self {
        BuildOverride::Path(&Path::new(s))
    }
}

impl<'a> From<&'a str> for ConfigQuery<'a> {
    fn from(value: &'a str) -> Self {
        let name = Some(value);
        ConfigQuery { name, ..Default::default() }
    }
}

impl<'a> From<&'a String> for ConfigQuery<'a> {
    fn from(value: &'a String) -> Self {
        let name = Some(value.as_str());
        ConfigQuery { name, ..Default::default() }
    }
}

impl<'a> From<ConfigLevel> for ConfigQuery<'a> {
    fn from(value: ConfigLevel) -> Self {
        let level = Some(value);
        ConfigQuery { level, ..Default::default() }
    }
}

impl<'a> From<BuildOverride<'a>> for ConfigQuery<'a> {
    fn from(build: BuildOverride<'a>) -> Self {
        let build = Some(build);
        ConfigQuery { build, ..Default::default() }
    }
}
