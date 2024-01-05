// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{anyhow, bail, Context, Result};
use assembly_config_schema::platform_config::swd_config::{
    OtaConfigs, PolicyConfig, PolicyLabels, SwdConfig, UpdateChecker, VerificationFailureAction,
};
use assembly_util::FileEntry;
use camino::Utf8PathBuf;
use std::fs::File;

#[allow(dead_code)]
const FUZZ_PERCENTAGE_RANGE: u32 = 25;
#[allow(dead_code)]
const RETRY_DELAY_SECONDS: u32 = 300;

/// SWD Configuration implementation detail which clarifies different subsystem behavior, derived
/// from the PolicyLabels
#[allow(dead_code)]
#[derive(Debug)]
pub struct PolicyLabelDetails {
    enable_dynamic_configuration: bool,
    persisted_repos_dir: bool,
    disable_executability_restrictions: bool,
}

#[allow(dead_code)]
impl PolicyLabelDetails {
    fn from_policy_labels(policy_labels: &PolicyLabels) -> Self {
        match policy_labels {
            PolicyLabels::BaseComponentsOnly => PolicyLabelDetails {
                enable_dynamic_configuration: false,
                disable_executability_restrictions: false,
                persisted_repos_dir: false,
            },
            PolicyLabels::LocalDynamicConfig => PolicyLabelDetails {
                enable_dynamic_configuration: true,
                disable_executability_restrictions: false,
                persisted_repos_dir: false,
            },
            PolicyLabels::Unrestricted => PolicyLabelDetails {
                enable_dynamic_configuration: true,
                disable_executability_restrictions: true,
                persisted_repos_dir: true,
            },
        }
    }
}

impl DefaultByBuildType for SwdConfig {
    fn default_by_build_type(build_type: &BuildType) -> Self {
        SwdConfig {
            policy: Some(PolicyLabels::default_by_build_type(build_type)),
            update_checker: Some(UpdateChecker::default_by_build_type(build_type)),
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_paths: vec![],
            include_configurator: false,
        }
    }
}

pub(crate) struct SwdSubsystemConfig;
impl DefineSubsystemConfiguration<SwdConfig> for SwdSubsystemConfig {
    /// Configures the SWD system. If a specific field is not specified, a
    /// default will be used based on the feature set level and the build type.
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        subsystem_config: &SwdConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        match &subsystem_config.update_checker {
            // The product set a specific update checker. Use that one.
            Some(update_checker) => {
                Self::set_update_checker(&update_checker, context.build_type, builder)?;
                Self::set_policy_by_build_type(&context.build_type, context, builder)?;
            }
            // The product does not specify. Set based on feature set level.
            None => {
                match context.feature_set_level {
                    // Minimal has an update checker
                    FeatureSupportLevel::Minimal => {
                        let update_checker =
                            UpdateChecker::default_by_build_type(context.build_type);
                        Self::set_update_checker(&update_checker, context.build_type, builder)?;
                        Self::set_policy_by_build_type(&context.build_type, context, builder)?;
                    }
                    // Utility has no update checker
                    FeatureSupportLevel::Utility => {
                        builder.platform_bundle("no_update_checker");
                    }
                    // Bootstrap has neither an update checker nor the system-update realm,
                    // so do not include `no_update_checker` AIB that requires the realm.
                    FeatureSupportLevel::Bootstrap => {}
                };
            }
        }

        for tuf_config in &subsystem_config.tuf_config_paths {
            let filename = tuf_config.file_name().ok_or(anyhow!(
                "Failed to get the filename from the tuf config: {}",
                &tuf_config
            ))?;
            builder.package("pkg-resolver").config_data(FileEntry {
                source: tuf_config.clone(),
                destination: format!("repositories/{}", filename),
            })?;
        }

        if subsystem_config.include_configurator {
            builder.platform_bundle("system_update_configurator");
        }

        Ok(())
    }
}

impl SwdSubsystemConfig {
    /// Configure which AIB to select based on the UpdateChecker
    fn set_update_checker(
        update_checker: &UpdateChecker,
        build_type: &BuildType,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        match update_checker {
            UpdateChecker::OmahaClient(OtaConfigs {
                channels_path,
                policy_config,
                include_empty_eager_config,
                ..
            }) => {
                if *include_empty_eager_config {
                    if build_type == &BuildType::User {
                        bail!("The empty_eager_config cannot be enabled on user builds");
                    }
                    builder.platform_bundle("omaha_client_empty_eager_config");
                }

                builder.platform_bundle("omaha_client");
                let mut omaha_config =
                    builder.package("omaha-client").component("meta/omaha-client-service.cm")?;
                omaha_config
                    .field("periodic_interval_minutes", policy_config.periodic_interval_minutes)?
                    .field("startup_delay_seconds", policy_config.startup_delay_seconds)?
                    .field("allow_reboot_when_idle", policy_config.allow_reboot_when_idle)?
                    .field("retry_delay_seconds", policy_config.retry_delay_seconds)?
                    .field("fuzz_percentage_range", policy_config.fuzz_percentage_range)?;

                if let Some(channel_config) = channels_path {
                    builder.package("omaha-client").config_data(FileEntry {
                        source: channel_config.clone(),
                        destination: "channel_config.json".into(),
                    })?;
                }
            }
            UpdateChecker::SystemUpdateChecker => {
                builder.platform_bundle("system_update_checker");
            }
        }
        Ok(())
    }

    fn set_policy_by_build_type(
        build_type: &BuildType,
        context: &ConfigurationContext<'_>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let gendir = context.get_gendir().context("Getting gendir for swd")?;

        let policy = PolicyLabels::default_by_build_type(build_type);
        let policy = PolicyLabelDetails::from_policy_labels(&policy);

        let write_config = |name: &str, value: serde_json::Value| -> Result<Utf8PathBuf> {
            let path = gendir.join(name);
            let file = File::create(&path).with_context(|| format!("Creating config: {}", name))?;
            serde_json::to_writer_pretty(file, &value)
                .with_context(|| format!("Writing config: {}", name))?;
            Ok(path)
        };

        if policy.persisted_repos_dir {
            let source = write_config(
                "pkg_resolver_repo_config.json",
                serde_json::json!({
                    "persisted_repos_dir": "repos",
                }),
            )?;
            builder
                .package("pkg-resolver")
                .config_data(FileEntry { source, destination: "persisted_repos_dir.json".into() })
                .context("Adding persisted repos dir config")?;
        }

        if policy.enable_dynamic_configuration {
            let source = write_config(
                "pkg_resolver_config.json",
                serde_json::json!({
                    "enable_dynamic_configuration": true,
                }),
            )?;
            builder
                .package("pkg-resolver")
                .config_data(FileEntry { source, destination: "config.json".into() })
                .context("Adding dynamic configuration config")?;
        }

        if policy.disable_executability_restrictions {
            builder.platform_bundle("pkgfs_disable_executability_restrictions");
        }

        Ok(())
    }
}

impl DefaultByBuildType for UpdateChecker {
    fn default_by_build_type(build_type: &BuildType) -> Self {
        match build_type {
            BuildType::Eng => UpdateChecker::SystemUpdateChecker,
            BuildType::UserDebug => UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default(),
                include_empty_eager_config: false,
            }),
            BuildType::User => UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default(),
                include_empty_eager_config: false,
            }),
        }
    }
}
impl DefaultByBuildType for PolicyLabels {
    fn default_by_build_type(build_type: &BuildType) -> Self {
        match build_type {
            BuildType::Eng => PolicyLabels::Unrestricted,
            BuildType::UserDebug => PolicyLabels::LocalDynamicConfig,
            BuildType::User => PolicyLabels::BaseComponentsOnly,
        }
    }
}

/// SWD Configuration implementation detail which allows the product owner to define custom failure
/// recovery rules for the system-update-committer
#[derive(Clone, Debug, PartialEq)]
struct VerificationFailureConfigDetails {
    enable: bool,
    blobfs: String,
}

impl VerificationFailureConfigDetails {
    #[allow(dead_code)]
    fn from_verification_failure_config(vfc: VerificationFailureAction) -> Self {
        match vfc {
            VerificationFailureAction::Reboot => VerificationFailureConfigDetails {
                enable: true,
                blobfs: "reboot_on_failure".to_string(),
            },
            VerificationFailureAction::Disabled => {
                VerificationFailureConfigDetails { enable: false, blobfs: "ignore".to_string() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swd_config_default_from_build_type_eng() {
        let build_type = &BuildType::Eng;
        let config = SwdConfig {
            policy: None,
            update_checker: None,
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_paths: vec![],
            ..Default::default()
        };
        let policy = config.policy.value_or_default_from_build_type(build_type);
        let update_checker = config.update_checker.value_or_default_from_build_type(build_type);
        let on_verification_failure = VerificationFailureAction::default();

        assert_eq!(policy, PolicyLabels::Unrestricted);
        assert_eq!(update_checker, UpdateChecker::SystemUpdateChecker);
        assert_eq!(on_verification_failure, VerificationFailureAction::Reboot);
    }
    #[test]
    fn test_swd_config_default_from_build_type_userdebug() {
        let build_type = &BuildType::UserDebug;
        let config = SwdConfig {
            policy: None,
            update_checker: None,
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_paths: vec![],
            ..Default::default()
        };
        let policy = config.policy.value_or_default_from_build_type(build_type);
        let update_checker = config.update_checker.value_or_default_from_build_type(build_type);
        let on_verification_failure = VerificationFailureAction::default();

        assert_eq!(policy, PolicyLabels::LocalDynamicConfig);
        assert_eq!(
            update_checker,
            UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default(),
                include_empty_eager_config: false,
            })
        );
        assert_eq!(on_verification_failure, VerificationFailureAction::Reboot);
    }
    #[test]
    fn test_swd_config_default_from_build_type_user() {
        let build_type = &BuildType::User;
        let config = SwdConfig {
            policy: None,
            update_checker: None,
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_paths: vec![],
            ..Default::default()
        };
        let policy = config.policy.value_or_default_from_build_type(build_type);
        let update_checker = config.update_checker.value_or_default_from_build_type(build_type);
        let on_verification_failure = VerificationFailureAction::default();

        assert_eq!(policy, PolicyLabels::BaseComponentsOnly);
        assert_eq!(
            update_checker,
            UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default(),
                include_empty_eager_config: false,
            })
        );
        assert_eq!(on_verification_failure, VerificationFailureAction::Reboot);
    }
}
