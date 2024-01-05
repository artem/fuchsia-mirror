// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use crate::util;
use anyhow::bail;
use assembly_config_schema::platform_config::connectivity_config::{
    NetstackVersion, NetworkingConfig, PlatformConnectivityConfig,
};
use assembly_util::FileEntry;

pub(crate) struct ConnectivitySubsystemConfig;
impl DefineSubsystemConfiguration<PlatformConnectivityConfig> for ConnectivitySubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        connectivity_config: &PlatformConnectivityConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let publish_fuchsia_dev_wired_service = match (
            context.feature_set_level,
            context.build_type,
            connectivity_config.mdns.publish_fuchsia_dev_wired_service,
        ) {
            // FFX discovery is not enabled on bootstrap, therefore we do not need the wired
            // udp service.
            (FeatureSupportLevel::Bootstrap, _, _) => false,

            // User builds cannot have this service enabled.
            (_, BuildType::User, None) => false,
            (_, BuildType::User, Some(true)) => {
                bail!("A MDNS wired udp service cannot be enabled on user builds")
            }
            // Userdebug and eng builds have this service enabled by default.
            (_, _, None) => true,
            // The product can override the default only on userdebug and eng builds.
            (_, _, Some(b)) => b,
        };
        if publish_fuchsia_dev_wired_service {
            builder.platform_bundle("mdns_fuchsia_device_wired_service");
        }
        if let Some(mdns_config) = &connectivity_config.mdns.config {
            builder.package("mdns").config_data(FileEntry {
                source: mdns_config.clone(),
                destination: "assembly.config".into(),
            })?;
        }

        // The configuration of networking is dependent on all three of:
        // - the feature_set_level
        // - the build_type
        // - the requested configuration type
        let networking = match (
            context.feature_set_level,
            context.build_type,
            &connectivity_config.network.networking,
        ) {
            // bootstrap must not attempt to configure it, it's always None
            (FeatureSupportLevel::Bootstrap, _, Some(_)) => {
                bail!("The configuration of networking is not an option for `bootstrap`")
            }
            (FeatureSupportLevel::Bootstrap, _, None) => None,

            // utility, in user mode, only gets networking if requested.
            (FeatureSupportLevel::Utility, BuildType::User, networking) => networking.as_ref(),

            // all other combinations get the network package that they request
            (_, _, Some(networking)) => Some(networking),

            // otherwise, the 'standard' networking package is used
            (_, _, None) => Some(&NetworkingConfig::Standard),
        };
        if let Some(networking) = networking {
            let maybe_gub_bundle = |bundle| {
                if connectivity_config.network.use_unified_binary {
                    format!("{bundle}_gub").into()
                } else {
                    std::borrow::Cow::Borrowed(bundle)
                }
            };

            // The 'core_realm_networking' and 'network_realm' bundles are
            // required if networking is enabled.
            builder.platform_bundle("core_realm_networking");
            builder.platform_bundle("network_realm");
            builder.platform_bundle(maybe_gub_bundle("network_realm_packages").as_ref());

            // Which specific network package is selectable by the product.
            match networking {
                NetworkingConfig::Standard => {
                    builder.platform_bundle("networking_with_virtualization");
                }
                NetworkingConfig::Basic => {
                    builder.platform_bundle("networking_basic");
                    builder.platform_bundle(maybe_gub_bundle("networking_basic_packages").as_ref());
                }
            }

            if let Some(netcfg_config_path) = &connectivity_config.network.netcfg_config_path {
                builder.package("netcfg").config_data(FileEntry {
                    source: netcfg_config_path.clone(),
                    destination: "default.json".into(),
                })?;
            }

            if let Some(netstack_config_path) = &connectivity_config.network.netstack_config_path {
                builder.package("netstack").config_data(FileEntry {
                    source: netstack_config_path.clone(),
                    destination: "default.json".into(),
                })?;
            }

            if let Some(google_maps_api_key_path) =
                &connectivity_config.network.google_maps_api_key_path
            {
                builder.package("emergency").config_data(FileEntry {
                    source: google_maps_api_key_path.clone(),
                    destination: "google_maps_api_key.txt".into(),
                })?;
            }

            // The use of netstack3 can be forcibly required by the board,
            // otherwise it's selectable by the product.
            match (
                context.board_info.provides_feature("fuchsia::network_require_netstack3"),
                connectivity_config.network.netstack_version,
            ) {
                (true, _) | (false, NetstackVersion::Netstack3) => {
                    builder.platform_bundle("netstack3");
                    builder.platform_bundle(maybe_gub_bundle("netstack3_packages").as_ref());
                }
                (false, NetstackVersion::Netstack2) => builder.platform_bundle("netstack2"),
                (false, NetstackVersion::NetstackMigration) => {
                    builder.platform_bundle("netstack_migration");
                    builder
                        .platform_bundle(maybe_gub_bundle("netstack_migration_packages").as_ref());
                }
            }

            // Add the networking test collection on all eng builds. The test
            // collection allows components to be launched inside the network
            // realm with access to all networking related capabilities.
            match context.build_type {
                BuildType::Eng => builder.platform_bundle("networking_test_collection"),
                _ => {}
            }

            let has_fullmac = context.board_info.provides_feature("fuchsia::wlan_fullmac");
            let has_softmac = context.board_info.provides_feature("fuchsia::wlan_softmac");
            if has_fullmac || has_softmac {
                builder.platform_bundle("wlan_base");

                // Add development support on eng and userdebug systems if we have wlan.
                match context.build_type {
                    BuildType::Eng | BuildType::UserDebug => {
                        builder.platform_bundle("wlan_development")
                    }
                    _ => {}
                }

                // Some products require legacy security types to be supported.
                // Otherwise, they are disabled by default.
                if connectivity_config.wlan.legacy_privacy_support {
                    builder.platform_bundle("wlan_legacy_privacy_support");
                } else {
                    builder.platform_bundle("wlan_contemporary_privacy_only_support");
                }

                if has_fullmac {
                    builder.platform_bundle("wlan_fullmac_support");
                }
                if has_softmac {
                    builder.platform_bundle("wlan_softmac_support");
                }
            }

            if connectivity_config.network.include_tun {
                builder.platform_bundle("network_tun");
            }

            if connectivity_config.thread.include_lowpan {
                builder.platform_bundle("thread_lowpan");
            }
        }

        // Add the weave core shard if necessary.
        if let Some(url) = &connectivity_config.weave.component_url {
            util::add_platform_declared_product_provided_component(
                url,
                "weavestack.core_shard.cml.template",
                context,
                builder,
            )?;
        }

        Ok(())
    }
}
