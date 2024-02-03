// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::{ensure, Context};
use assembly_config_schema::platform_config::fonts_config::FontsConfig;

const FONT_SERVER_COMPONENT_LOCAL_URL: &str = "meta/font-server.cm";
const FONT_SERVER_PACKAGE_NAME: &str = "font-server";

const LEGACY_FONT_OMPONENT_LOCAL_URL: &str = "meta/fonts.cm";
const LEGACY_FONT_PACKAGE_NAME: &str = "fonts";

pub(crate) struct FontsSubsystem;
impl DefineSubsystemConfiguration<FontsConfig> for FontsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        fonts_config: &FontsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Adding the platform bundle conditionally allows us to soft-migrate
        // products that use the packages from the `fonts` bundle.
        if *context.feature_set_level == FeatureSupportLevel::Standard {
            if let Some(ref font_collection_name) = fonts_config.font_collection {
                ensure!(
                    fonts_config.enabled,
                    "fonts.enabled must be `true` if `fonts.font_collection` is set"
                );
                ensure!(
                    !font_collection_name.is_empty(),
                    "fonts.font_collection must not be empty",
                );
                builder.platform_bundle("fonts_hermetic");
                let mut component = builder
                    .package(FONT_SERVER_PACKAGE_NAME)
                    .component(FONT_SERVER_COMPONENT_LOCAL_URL)
                    .with_context(|| {
                        format!("while setting component {}", FONT_SERVER_COMPONENT_LOCAL_URL)
                    })?;
                component
                    .field(
                        "verbose_logging",
                        matches!(context.build_type, BuildType::Eng | BuildType::UserDebug),
                    )
                    .context("while setting verbose_logging")?;
                let font_manifest = format!(
                    "/fonts/data/assets/{}_all.hermetic_assets.font_manifest.json",
                    font_collection_name
                );
                component
                    .field("font_manifest", font_manifest)
                    .context("while setting font_manifest")?;
            } else if fonts_config.enabled {
                builder.platform_bundle("fonts");
                let mut component = builder
                    .package(LEGACY_FONT_PACKAGE_NAME)
                    .component(LEGACY_FONT_OMPONENT_LOCAL_URL)
                    .with_context(|| {
                        format!("while setting component {}", LEGACY_FONT_OMPONENT_LOCAL_URL)
                    })?;
                component
                    .field(
                        "verbose_logging",
                        matches!(context.build_type, BuildType::Eng | BuildType::UserDebug),
                    )
                    .context("while setting verbose_logging")?;
                // Fallback to using fonts from `config-data` since a font collection was not
                // specified. Signified by an empty value in `font_manifest`.
                component
                    .field("font_manifest", String::from(""))
                    .context("while setting legacy value for font_manifest")?;
            }
        }
        Ok(())
    }
}
