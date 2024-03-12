// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The femu module encapsulates the interactions with the emulator instance
//! started via the Fuchsia emulator, Femu.

use super::get_host_tool;
use crate::qemu_based::QemuBasedEngine;
use async_trait::async_trait;
use emulator_instance::{
    get_instance_dir, EmulatorConfiguration, EmulatorInstanceData, EmulatorInstanceInfo,
    EngineState, EngineType,
};
use ffx_emulator_common::config;
use ffx_emulator_config::{EmulatorEngine, EngineConsoleType, ShowDetail};
use fho::{bug, return_bug, Result};
use fidl_fuchsia_developer_ffx as ffx;
use std::{env, process::Command};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FemuEngine {
    data: EmulatorInstanceData,
}

impl FemuEngine {
    pub(crate) fn new(data: EmulatorInstanceData) -> Self {
        Self { data }
    }

    fn validate_configuration(&self) -> Result<()> {
        let emulator_configuration = self.data.get_emulator_configuration();
        if !emulator_configuration.runtime.headless && std::env::var("DISPLAY").is_err() {
            eprintln!(
                "DISPLAY not set in the local environment, try running with --headless if you \
                encounter failures related to display or Qt.",
            );
        }
        self.validate_network_flags(emulator_configuration)
            .and_then(|()| self.check_required_files(&emulator_configuration.guest))
    }

    fn validate_staging(&self) -> Result<()> {
        self.check_required_files(&self.data.get_emulator_configuration().guest)
    }
}

#[async_trait]
impl EmulatorEngine for FemuEngine {
    async fn stage(&mut self) -> Result<()> {
        let result = <Self as QemuBasedEngine>::stage(&mut self)
            .await
            .and_then(|()| self.validate_staging());
        match result {
            Ok(()) => {
                self.data.set_engine_state(EngineState::Staged);
                self.save_to_disk().await
            }
            Err(e) => {
                self.data.set_engine_state(EngineState::Error);
                self.save_to_disk().await.and(Err(e))
            }
        }
    }

    async fn start(
        &mut self,
        emulator_cmd: Command,
        proxy: &ffx::TargetCollectionProxy,
    ) -> Result<i32> {
        self.run(emulator_cmd, proxy).await
    }

    fn show(&self, details: Vec<ShowDetail>) -> Vec<ShowDetail> {
        <Self as QemuBasedEngine>::show(self, details)
    }

    async fn stop(&mut self) -> Result<()> {
        self.stop_emulator().await
    }

    fn configure(&mut self) -> Result<()> {
        let result = if self.emu_config().runtime.config_override {
            println!("Custom configuration provided; bypassing validation.");
            Ok(())
        } else {
            self.validate_configuration()
        };
        if result.is_ok() {
            self.data.set_engine_state(EngineState::Configured);
        } else {
            self.data.set_engine_state(EngineState::Error);
        }
        result
    }

    fn engine_state(&self) -> EngineState {
        self.get_engine_state()
    }

    fn engine_type(&self) -> EngineType {
        self.data.get_engine_type()
    }

    async fn is_running(&mut self) -> bool {
        let running = self.data.is_running();
        if self.engine_state() == EngineState::Running && running == false {
            self.set_engine_state(EngineState::Staged);
            if self.save_to_disk().await.is_err() {
                tracing::warn!("Problem saving serialized emulator to disk during state update.");
            }
        }
        running
    }

    fn attach(&self, console: EngineConsoleType) -> Result<()> {
        self.attach_to(&self.data.get_emulator_configuration().runtime.instance_directory, console)
    }

    /// Build the Command to launch Android emulator running Fuchsia.
    fn build_emulator_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.data.get_emulator_binary());
        let emulator_configuration = self.data.get_emulator_configuration();
        let feature_arg = emulator_configuration
            .flags
            .features
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",");
        if feature_arg.len() > 0 {
            cmd.arg("-feature").arg(feature_arg);
        }
        cmd.args(&emulator_configuration.flags.options)
            .arg("-fuchsia")
            .args(&emulator_configuration.flags.args);
        let extra_args = emulator_configuration
            .flags
            .kernel_args
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        if extra_args.len() > 0 {
            cmd.args(["-append", &extra_args]);
        }
        if self.data.get_emulator_configuration().flags.envs.len() > 0 {
            // Add environment variables if not already present.
            // This does not overwrite any existing values.
            let unset_envs =
                emulator_configuration.flags.envs.iter().filter(|(k, _)| env::var(k).is_err());
            if unset_envs.clone().count() > 0 {
                cmd.envs(unset_envs);
            }
        }
        cmd
    }

    /// Get the AEMU binary path from the SDK manifest and verify it exists.
    async fn load_emulator_binary(&mut self) -> Result<()> {
        let emulator_binary = match get_host_tool(config::FEMU_TOOL).await {
            Ok(aemu_path) => aemu_path.canonicalize().map_err(|e| {
                bug!("Failed to canonicalize the path to the emulator binary: {aemu_path:?}: {e}")
            })?,
            Err(e) => {
                return_bug!("Cannot find {} in the SDK: {:?}", config::FEMU_TOOL, e);
            }
        };

        if !emulator_binary.exists() || !emulator_binary.is_file() {
            return_bug!("Giving up finding emulator binary. Tried {:?}", emulator_binary)
        }
        self.data.set_emulator_binary(emulator_binary);
        Ok(())
    }

    fn emu_config(&self) -> &EmulatorConfiguration {
        self.data.get_emulator_configuration()
    }

    fn emu_config_mut(&mut self) -> &mut EmulatorConfiguration {
        self.data.get_emulator_configuration_mut()
    }

    async fn save_to_disk(&self) -> Result<()> {
        emulator_instance::write_to_disk(
            &self.data,
            &get_instance_dir(self.data.get_name(), true).await?,
        )
        .map_err(|e| bug!("Error saving instance to disk: {e}"))
    }
}

impl QemuBasedEngine for FemuEngine {
    fn set_pid(&mut self, pid: u32) {
        self.data.set_pid(pid)
    }

    fn get_pid(&self) -> u32 {
        self.data.get_pid()
    }

    fn set_engine_state(&mut self, state: EngineState) {
        self.data.set_engine_state(state);
    }

    fn get_engine_state(&self) -> EngineState {
        self.data.get_engine_state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulator_instance::NetworkingMode;
    use std::ffi::OsStr;

    #[test]
    fn test_build_emulator_cmd() {
        let program_name = "/test_femu_bin";
        let mut cfg = EmulatorConfiguration::default();
        cfg.host.networking = NetworkingMode::User;
        cfg.flags.envs.insert("FLAG_NAME_THAT_DOES_NOT_EXIST".into(), "1".into());

        let mut emu_data = EmulatorInstanceData::new(cfg, EngineType::Femu, EngineState::New);
        emu_data.set_emulator_binary(program_name.into());
        let test_engine = FemuEngine::new(emu_data);
        let cmd = test_engine.build_emulator_cmd();
        assert_eq!(cmd.get_program(), program_name);
        assert_eq!(
            cmd.get_envs().collect::<Vec<_>>(),
            [(OsStr::new("FLAG_NAME_THAT_DOES_NOT_EXIST"), Some(OsStr::new("1")))]
        );
    }
    #[test]
    fn test_build_emulator_cmd_existing_env() {
        env::set_var("FLAG_NAME_THAT_DOES_EXIST", "preset_value");
        let program_name = "/test_femu_bin";
        let mut cfg = EmulatorConfiguration::default();
        cfg.host.networking = NetworkingMode::User;
        cfg.flags.envs.insert("FLAG_NAME_THAT_DOES_EXIST".into(), "1".into());

        let mut emu_data = EmulatorInstanceData::new(cfg, EngineType::Femu, EngineState::New);
        emu_data.set_emulator_binary(program_name.into());
        let test_engine = FemuEngine::new(emu_data);
        let cmd = test_engine.build_emulator_cmd();
        assert_eq!(cmd.get_program(), program_name);
        assert_eq!(cmd.get_envs().collect::<Vec<_>>(), []);
    }
}
