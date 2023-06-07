// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::diagnostics_config::{
    ArchivistConfig, DiagnosticsConfig,
};
use std::collections::BTreeSet;

pub(crate) struct DiagnosticsSubsystem;
impl DefineSubsystemConfiguration<DiagnosticsConfig> for DiagnosticsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        diagnostics_config: &DiagnosticsConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // LINT.IfChange
        let mut bind_services = BTreeSet::from([
            "fuchsia.component.KcounterBinder",
            "fuchsia.component.PersistenceBinder",
            "fuchsia.component.SamplerBinder",
        ]);
        let mut num_threads = 4;
        let mut maximum_concurrent_snapshots_per_reader = 4;
        let mut logs_max_cached_original_bytes = 4194304;
        let mut archivist_config = builder.package("archivist").component("meta/archivist.cm")?;

        match (context.build_type, context.feature_set_level) {
            // Always clear bind_services for bootstrap (bringup) and utility
            // systems.
            (_, FeatureSupportLevel::Bootstrap) | (_, FeatureSupportLevel::Utility) => {
                bind_services.clear();
            }
            // Detect isn't present on user builds.
            (BuildType::User, FeatureSupportLevel::Minimal) => {}
            (_, FeatureSupportLevel::Minimal) => {
                bind_services.insert("fuchsia.component.DetectBinder");
            }
        };

        match diagnostics_config.archivist {
            Some(ArchivistConfig::Default) | None => {}
            Some(ArchivistConfig::LowMem) => {
                num_threads = 2;
                logs_max_cached_original_bytes = 2097152;
                maximum_concurrent_snapshots_per_reader = 2;
            }
        }

        archivist_config
            .field("bind_services", bind_services.into_iter().collect::<Vec<_>>())?
            .field("enable_component_event_provider", true)?
            .field("enable_event_source", true)?
            .field("enable_klog", true)?
            .field("enable_log_connector", true)?
            .field("log_to_debuglog", true)?
            .field("logs_max_cached_original_bytes", logs_max_cached_original_bytes)?
            .field(
                "maximum_concurrent_snapshots_per_reader",
                maximum_concurrent_snapshots_per_reader,
            )?
            .field("num_threads", num_threads)?
            .field("pipelines_path", "/config/data")?;
        // LINT.ThenChange(/src/diagnostics/archivist/configs.gni)

        let exception_handler_available =
            !matches!(context.feature_set_level, FeatureSupportLevel::Bootstrap);
        builder
            .package("svchost")
            .component("meta/svchost.cm")?
            .field("exception_handler_available", exception_handler_available)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ConfigurationBuilderImpl;
    use assembly_config_schema::BuildType;
    use serde_json::{Number, Value};

    #[test]
    fn test_define_configuration_default() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Minimal,
            build_type: &BuildType::Eng,
            ..Default::default()
        };
        let diagnostics = DiagnosticsConfig { archivist: Some(ArchivistConfig::Default) };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(&context, &diagnostics, &mut builder).unwrap();
        let config = builder.build();
        let archivist_fields = &config.package_configs["archivist"]
            .components
            .get("meta/archivist.cm")
            .unwrap()
            .fields;
        assert_eq!(
            archivist_fields.get("bind_services"),
            Some(&Value::Array(vec![
                "fuchsia.component.DetectBinder".into(),
                "fuchsia.component.KcounterBinder".into(),
                "fuchsia.component.PersistenceBinder".into(),
                "fuchsia.component.SamplerBinder".into(),
            ]))
        );
        assert_eq!(
            archivist_fields.get("enable_component_event_provider"),
            Some(&Value::Bool(true))
        );
        assert_eq!(archivist_fields.get("enable_event_source"), Some(&Value::Bool(true)));
        assert_eq!(archivist_fields.get("enable_klog"), Some(&Value::Bool(true)));
        assert_eq!(archivist_fields.get("enable_log_connector"), Some(&Value::Bool(true)));
        assert_eq!(archivist_fields.get("log_to_debuglog"), Some(&Value::Bool(true)));
        assert_eq!(
            archivist_fields.get("logs_max_cached_original_bytes"),
            Some(&Value::Number(Number::from(4194304)))
        );
        assert_eq!(
            archivist_fields.get("maximum_concurrent_snapshots_per_reader"),
            Some(&Value::Number(Number::from(4)))
        );
        assert_eq!(archivist_fields.get("num_threads"), Some(&Value::Number(Number::from(4))));
        assert_eq!(
            archivist_fields.get("pipelines_path"),
            Some(&Value::String("/config/data".to_string()))
        );
    }

    #[test]
    fn test_define_configuration_low_mem() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Minimal,
            build_type: &BuildType::Eng,
            ..Default::default()
        };
        let diagnostics = DiagnosticsConfig { archivist: Some(ArchivistConfig::LowMem) };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(&context, &diagnostics, &mut builder).unwrap();
        let config = builder.build();
        let archivist_fields = &config.package_configs["archivist"]
            .components
            .get("meta/archivist.cm")
            .unwrap()
            .fields;
        assert_eq!(archivist_fields.get("num_threads"), Some(&Value::Number(Number::from(2))));
        assert_eq!(
            archivist_fields.get("logs_max_cached_original_bytes"),
            Some(&Value::Number(Number::from(2097152)))
        );
        assert_eq!(
            archivist_fields.get("maximum_concurrent_snapshots_per_reader"),
            Some(&Value::Number(Number::from(2)))
        );
    }

    #[test]
    fn test_default_on_bootstrap() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Bootstrap,
            build_type: &BuildType::Eng,
            board_info: None,
            ..Default::default()
        };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsConfig::default(),
            &mut builder,
        )
        .unwrap();
        let config = builder.build();
        let archivist_fields = &config.package_configs["archivist"]
            .components
            .get("meta/archivist.cm")
            .unwrap()
            .fields;
        assert_eq!(archivist_fields.get("num_threads"), Some(&Value::Number(Number::from(4))));
        assert_eq!(archivist_fields.get("bind_services"), Some(&Value::Array(vec![])));
    }

    #[test]
    fn test_default_for_user() {
        let context = ConfigurationContext {
            feature_set_level: &FeatureSupportLevel::Minimal,
            build_type: &BuildType::User,
            board_info: None,
            ..Default::default()
        };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsConfig::default(),
            &mut builder,
        )
        .unwrap();
        let config = builder.build();
        let archivist_fields = &config.package_configs["archivist"]
            .components
            .get("meta/archivist.cm")
            .unwrap()
            .fields;
        assert_eq!(
            archivist_fields.get("bind_services"),
            Some(&Value::Array(vec![
                "fuchsia.component.KcounterBinder".into(),
                "fuchsia.component.PersistenceBinder".into(),
                "fuchsia.component.SamplerBinder".into(),
            ]))
        );
    }
}
