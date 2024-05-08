// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context};
use assembly_config_schema::platform_config::PlatformConfig;
use assembly_config_schema::product_config::ProductConfig;
use assembly_config_schema::{BoardInformation, BuildType, ExampleConfig};
use camino::Utf8Path;

use crate::common::{CompletedConfiguration, ConfigurationBuilderImpl};

pub(crate) mod prelude {

    #[allow(unused)]
    pub(crate) use crate::common::{
        BoardInformationExt, ComponentConfigBuilderExt, ConfigurationBuilder, ConfigurationContext,
        DefaultByBuildType, DefineSubsystemConfiguration, FeatureSupportLevel,
        OptionDefaultByBuildTypeExt,
    };

    #[allow(unused)]
    pub(crate) use assembly_config_schema::BuildType;
}

use prelude::*;

mod battery;
mod bluetooth;
mod build_info;
mod component;
mod connectivity;
mod development;
mod diagnostics;
mod driver_framework;
mod example;
mod fonts;
mod forensics;
mod graphics;
mod hwinfo;
mod icu;
mod input_groups;
mod intl;
mod kernel;
mod media;
mod paravirtualization;
mod power;
mod radar;
mod rcs;
mod recovery;
mod sensors;
mod session;
mod setui;
mod starnix;
mod storage;
mod swd;
mod thermal;
mod timekeeper;
mod ui;
mod usb;
mod virtualization;

/// ffx config flag for enabling configuring the assembly+structured config example.
const EXAMPLE_ENABLED_FLAG: &str = "assembly_example_enabled";

/// Convert the high-level description of product configuration into a series of configuration
/// value files with concrete package/component tuples.
///
/// Returns a map from package names to configuration updates.
pub fn define_configuration(
    platform: &PlatformConfig,
    product: &ProductConfig,
    board_info: &BoardInformation,
    ramdisk_image: bool,
    gendir: impl AsRef<Utf8Path>,
    resource_dir: impl AsRef<Utf8Path>,
) -> anyhow::Result<CompletedConfiguration> {
    let icu_config = &platform.icu;
    let mut builder = ConfigurationBuilderImpl::new(icu_config.clone());

    // The emulator support bundle is always added, even to an empty build.
    builder.platform_bundle("emulator_support");

    let feature_set_level = FeatureSupportLevel::from_deserialized(&platform.feature_set_level);

    // Only perform configuration if the feature_set_level is not None (ie, Empty).
    if let Some(feature_set_level) = &feature_set_level {
        let build_type = &platform.build_type;
        let gendir = gendir.as_ref().to_path_buf();
        let resource_dir = resource_dir.as_ref().to_path_buf();

        // Set up the context that's used by each subsystem to get the generally-
        // available platform information.
        let context = ConfigurationContext {
            feature_set_level,
            build_type,
            board_info,
            ramdisk_image,
            gendir,
            resource_dir,
        };

        // Call the configuration functions for each subsystem.
        configure_subsystems(&context, platform, product, &mut builder)?;
    }

    Ok(builder.build())
}

struct CommonBundles;
impl DefineSubsystemConfiguration<()> for CommonBundles {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Set up the platform's common AIBs by feature_set_level and build_type.
        for bundle_name in match (context.feature_set_level, context.build_type) {
            (FeatureSupportLevel::Embeddable, BuildType::Eng) => {
                vec!["embeddable", "embeddable_userdebug", "embeddable_eng"]
            }
            (FeatureSupportLevel::Embeddable, BuildType::UserDebug) => {
                vec!["embeddable", "embeddable_userdebug"]
            }
            (FeatureSupportLevel::Embeddable, BuildType::User) => {
                vec!["embeddable"]
            }
            (FeatureSupportLevel::Bootstrap, _) => {
                vec![
                    "embeddable",
                    "embeddable_userdebug",
                    "embeddable_eng",
                    "bootstrap",
                    "bootstrap_userdebug",
                    "bootstrap_eng",
                ]
            }
            (FeatureSupportLevel::Utility, BuildType::Eng) => {
                vec![
                    "embeddable",
                    "embeddable_userdebug",
                    "embeddable_eng",
                    "bootstrap",
                    "bootstrap_userdebug",
                    "bootstrap_eng",
                    "core_realm",
                    "core_realm_development_access",
                    "core_realm_development_access_eng",
                    "core_realm_eng",
                ]
            }
            (FeatureSupportLevel::Utility, BuildType::UserDebug) => {
                vec![
                    "embeddable",
                    "embeddable_userdebug",
                    "bootstrap",
                    "bootstrap_userdebug",
                    "core_realm",
                    "core_realm_development_access",
                    "core_realm_development_access_userdebug",
                    "core_realm_user_and_userdebug",
                ]
            }
            (FeatureSupportLevel::Utility, BuildType::User) => {
                vec!["embeddable", "bootstrap", "core_realm", "core_realm_user_and_userdebug"]
            }
            (FeatureSupportLevel::Standard, BuildType::Eng) => {
                vec![
                    "embeddable",
                    "embeddable_userdebug",
                    "embeddable_eng",
                    "bootstrap",
                    "bootstrap_userdebug",
                    "bootstrap_eng",
                    "core_realm",
                    "core_realm_eng",
                    "core_realm_development_access",
                    "core_realm_development_access_eng",
                    "common_standard",
                    "standard_eng",
                    "standard_userdebug_and_eng",
                    "testing_support",
                ]
            }
            (FeatureSupportLevel::Standard, BuildType::UserDebug) => {
                vec![
                    "embeddable",
                    "embeddable_userdebug",
                    "bootstrap",
                    "bootstrap_userdebug",
                    "core_realm",
                    "core_realm_development_access",
                    "core_realm_development_access_userdebug",
                    "core_realm_user_and_userdebug",
                    "common_standard",
                    "standard_userdebug",
                    "standard_userdebug_and_eng",
                ]
            }
            (FeatureSupportLevel::Standard, BuildType::User) => {
                vec![
                    "embeddable",
                    "bootstrap",
                    "core_realm",
                    "core_realm_user_and_userdebug",
                    "common_standard",
                    "standard_user",
                ]
            }
        } {
            builder.platform_bundle(bundle_name);
        }

        Ok(())
    }
}

fn configure_subsystems(
    context: &ConfigurationContext<'_>,
    platform: &PlatformConfig,
    product: &ProductConfig,
    builder: &mut dyn ConfigurationBuilder,
) -> anyhow::Result<()> {
    // Define the common platform bundles for this platform configuration.
    CommonBundles::define_configuration(context, &(), builder)
        .context("Selecting the common platform assembly input bundles")?;

    // Configure the Product Assembly + Structured Config example, if enabled.
    if should_configure_example() {
        example::ExampleSubsystemConfig::define_configuration(
            context,
            &platform.example_config,
            builder,
        )?;
    } else if platform.example_config != ExampleConfig::default() {
        bail!("Config options were set for the example subsystem, but the example is not enabled to be configured.");
    }

    // The real platform subsystems

    battery::BatterySubsystemConfig::define_configuration(context, &platform.battery, builder)
        .context("Configuring the 'battery' subsystem")?;

    bluetooth::BluetoothSubsystemConfig::define_configuration(
        context,
        &platform.bluetooth,
        builder,
    )
    .context("Configuring the `bluetooth` subsystem")?;

    build_info::BuildInfoSubsystem::define_configuration(context, &product.build_info, builder)
        .context("Configuring the 'build_info' subsystem")?;

    let component_config = component::ComponentConfig {
        policy: &product.component_policy,
        development_support: &platform.development_support,
        starnix: &platform.starnix,
    };
    component::ComponentSubsystem::define_configuration(context, &component_config, builder)
        .context("Configuring the 'component' subsystem")?;

    connectivity::ConnectivitySubsystemConfig::define_configuration(
        context,
        &platform.connectivity,
        builder,
    )
    .context("Configuring the 'connectivity' subsystem")?;

    development::DevelopmentConfig::define_configuration(
        context,
        &platform.development_support,
        builder,
    )
    .context("Configuring the 'development' subsystem")?;

    diagnostics::DiagnosticsSubsystem::define_configuration(
        context,
        &platform.diagnostics,
        builder,
    )
    .context("Configuring the 'diagnostics' subsystem")?;

    driver_framework::DriverFrameworkSubsystemConfig::define_configuration(
        context,
        &platform.driver_framework,
        builder,
    )
    .context("Configuring the 'driver_framework' subsystem")?;

    graphics::GraphicsSubsystemConfig::define_configuration(context, &platform.graphics, builder)
        .context("Configuring the 'graphics' subsystem")?;

    hwinfo::HwinfoSubsystem::define_configuration(context, &product.info, builder)
        .context("Configuring the 'hwinfo' subsystem")?;

    icu::IcuSubsystem::define_configuration(context, &platform.icu, builder)
        .context("Configuring the 'icu' subsystem")?;

    input_groups::InputGroupsSubsystem::define_configuration(
        context,
        &platform.input_groups,
        builder,
    )
    .context("Configuring the 'input_groups' subsystem")?;

    media::MediaSubsystem::define_configuration(context, &platform.media, builder)
        .context("Configuring the 'media' subsystem")?;

    power::PowerManagementSubsystem::define_configuration(context, &platform.power, builder)
        .context("Configuring the 'power' subsystem")?;

    paravirtualization::ParavirtualizationSubsystem::define_configuration(
        context,
        &platform.paravirtualization,
        builder,
    )
    .context("Configuring the 'paravirtualization' subsystem")?;

    radar::RadarSubsystemConfig::define_configuration(context, &(), builder)
        .context("Configuring the 'radar' subsystem")?;

    recovery::RecoverySubsystem::define_configuration(context, &platform.recovery, builder)
        .context("Configuring the 'recovery' subsystem")?;

    rcs::RcsSubsystemConfig::define_configuration(context, &(), builder)
        .context("Configuring the 'rcs' subsystem")?;

    sensors::SensorsSubsystemConfig::define_configuration(context, &platform.starnix, builder)
        .context("Configuring the 'sensors' subsystem")?;

    session::SessionConfig::define_configuration(
        context,
        &(&platform.session, &product.session_url),
        builder,
    )
    .context("Configuring the 'session' subsystem")?;

    starnix::StarnixSubsystem::define_configuration(context, &platform.starnix, builder)
        .context("Configuring the starnix subsystem")?;

    storage::StorageSubsystemConfig::define_configuration(context, &platform.storage, builder)
        .context("Configuring the 'storage' subsystem")?;

    swd::SwdSubsystemConfig::define_configuration(context, &platform.software_delivery, builder)
        .context("Configuring the 'software_delivery' subsystem")?;

    thermal::ThermalSubsystem::define_configuration(context, &(), builder)
        .context("Configuring the 'thermal' subsystem")?;

    ui::UiSubsystem::define_configuration(context, &platform.ui, builder)
        .context("Configuring the 'ui' subsystem")?;

    virtualization::VirtualizationSubsystem::define_configuration(
        context,
        &platform.virtualization,
        builder,
    )
    .context("Configuring the 'virtualization' subsystem")?;

    fonts::FontsSubsystem::define_configuration(context, &platform.fonts, builder)
        .context("Configuring the 'fonts' subsystem")?;

    intl::IntlSubsystem::define_configuration(
        context,
        &(&platform.intl, &platform.session),
        builder,
    )
    .context("Confguring the 'intl' subsystem")?;

    setui::SetUiSubsystem::define_configuration(context, &platform.setui, builder)
        .context("Confguring the 'SetUI' subsystem")?;

    kernel::KernelSubsystem::define_configuration(context, &platform.kernel, builder)
        .context("Configuring the 'kernel' subsystem")?;

    forensics::ForensicsSubsystem::define_configuration(context, &platform.forensics, builder)
        .context("Configuring the 'Forensics' subsystem")?;

    timekeeper::TimekeeperSubsystem::define_configuration(context, &platform.timekeeper, builder)
        .context("Configuring the 'timekeeper' subsystem")?;

    usb::UsbSubsystemConfig::define_configuration(context, &platform.usb, builder)
        .context("Configuring the 'usb' subsystem")?;

    Ok(())
}

/// Check ffx config for whether we should execute example code.
fn should_configure_example() -> bool {
    futures::executor::block_on(ffx_config::get::<bool, _>(EXAMPLE_ENABLED_FLAG))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_config_schema::AssemblyConfig;
    use assembly_util as util;

    #[test]
    fn test_example_config_without_configure_example_returns_err() {
        let json5 = r#"
            {
            platform: {
                build_type: "eng",
                example_config: {
                    include_example_aib: true
                }
            },
            product: {},
            }
        "#;

        let mut cursor = std::io::Cursor::new(json5);
        let AssemblyConfig { platform, product, .. } = util::from_reader(&mut cursor).unwrap();
        let result =
            define_configuration(&platform, &product, &BoardInformation::default(), false, "", "");

        assert!(result.is_err());
    }
}
