// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_stash::StoreMarker;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use fuchsia_fs::OpenFlags;
use fuchsia_inspect::component;
use futures::lock::Mutex;
use settings::base::get_default_interfaces;
use settings::config::base::get_default_agent_types;
use settings::config::default_settings::DefaultSetting;
use settings::display::build_display_default_settings;
use settings::handler::setting_proxy_inspect_info::SettingProxyInspectInfo;
use settings::input::build_input_default_settings;
use settings::inspect::listener_logger::ListenerInspectLogger;
use settings::light::build_light_default_settings;
use settings::AgentConfiguration;
use settings::EnabledInterfacesConfiguration;
use settings::EnvironmentBuilder;
use settings::ServiceConfiguration;
use settings::ServiceFlags;
use settings_storage::stash_logger::StashInspectLogger;
use settings_storage::storage_factory::StashDeviceStorageFactory;
use std::path::Path;
use std::sync::Arc;

const STASH_IDENTITY: &str = "settings_service";

#[fuchsia::main(logging_tags = ["setui-service"])]
fn main() -> Result<(), Error> {
    let executor = fasync::LocalExecutor::new();
    tracing::info!("Starting setui-service...");

    let inspector = component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    // Serve stats about inspect in a lazy node.
    component::serve_inspect_stats();

    let setting_proxy_inspect_info = SettingProxyInspectInfo::new(inspector.root());
    let stash_inspect_logger = StashInspectLogger::new(inspector.root());
    let listener_inspect_logger = ListenerInspectLogger::new();

    let default_enabled_interfaces_configuration =
        EnabledInterfacesConfiguration::with_interfaces(get_default_interfaces());

    let input_configuration = build_input_default_settings();

    let light_configuration = build_light_default_settings();

    let enabled_interface_configuration = DefaultSetting::new(
        Some(default_enabled_interfaces_configuration),
        "/config/data/interface_configuration.json",
    )
    .load_default_value()
    .expect("invalid default enabled interface configuration")
    .expect("no default enabled interfaces configuration");

    let flags =
        DefaultSetting::new(Some(ServiceFlags::default()), "/config/data/service_flags.json")
            .load_default_value()
            .expect("invalid service flag configuration")
            .expect("no default service flags");
    let display_configuration = build_display_default_settings();

    // Temporary solution for FEMU to have an agent config without camera agent.
    let agent_config = "/config/data/agent_configuration.json";
    let board_agent_config = "/config/data/board_agent_configuration.json";
    let agent_configuration_file_path =
        if Path::new(board_agent_config).exists() { board_agent_config } else { agent_config };

    let agent_types = DefaultSetting::new(
        Some(AgentConfiguration { agent_types: get_default_agent_types() }),
        agent_configuration_file_path,
    )
    .load_default_value()
    .expect("invalid default agent configuration")
    .expect("no default agent types");

    let configuration =
        ServiceConfiguration::from(agent_types, enabled_interface_configuration, flags);

    let store_proxy = connect_to_protocol::<StoreMarker>().expect("failed to connect to stash");
    store_proxy.identify(STASH_IDENTITY).expect("should identify");
    let storage_factory = StashDeviceStorageFactory::new(
        store_proxy.clone(),
        Arc::new(Mutex::new(stash_inspect_logger)),
    );

    let storage_dir = fuchsia_fs::directory::open_in_namespace(
        "/data/storage",
        OpenFlags::RIGHT_READABLE | OpenFlags::RIGHT_WRITABLE | OpenFlags::CREATE,
    )
    .context("unable to open data dir")?;
    // EnvironmentBuilder::spawn returns a future that can be awaited for the
    // result of the startup. Since main is a synchronous function, we cannot
    // block here and therefore continue without waiting for the result.

    let fs = ServiceFs::new();

    EnvironmentBuilder::new(Arc::new(storage_factory))
        .configuration(configuration)
        .display_configuration(display_configuration)
        .input_configuration(input_configuration)
        .light_configuration(light_configuration)
        .setting_proxy_inspect_info(
            setting_proxy_inspect_info.node(),
            Arc::new(Mutex::new(listener_inspect_logger)),
        )
        .storage_dir(storage_dir)
        .store_proxy(store_proxy)
        .spawn(executor, fs)
        .context("Failed to spawn environment for setui")
}
