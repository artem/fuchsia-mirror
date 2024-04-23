// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context, Result};
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_intl as fintl;
use fidl_fuchsia_intl_test::*;
use fidl_fuchsia_settings as fsettings;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at};
use futures::StreamExt;
use realm_client::{extend_namespace, InstalledNamespace};

async fn create_realm(options: RealmOptions) -> Result<InstalledNamespace> {
    let realm_factory = connect_to_protocol::<RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();

    realm_factory
        .create_realm2(options, dict_server)
        .await?
        .map_err(realm_client::Error::OperationError)?;
    let ns = extend_namespace(realm_factory, dict_client).await?;

    Ok(ns)
}

#[fuchsia::test]
async fn set_then_get() -> Result<()> {
    let realm_options = RealmOptions::default();
    let test_ns = create_realm(realm_options).await?;
    let intl = connect_to_protocol_at::<fsettings::IntlMarker>(test_ns.prefix())?;
    let property_provider =
        connect_to_protocol_at::<fintl::PropertyProviderMarker>(test_ns.prefix())?;
    let mut event_stream = property_provider.take_event_stream();

    // This warms up the intl services component and the set_ui component, avoiding potential
    // data races later.
    let _initial_profile = property_provider.get_profile().await.context("get initial profile")?;

    let new_settings = fsettings::IntlSettings {
        locales: Some(vec![fintl::LocaleId { id: "sr-RS".to_string() }]),
        time_zone_id: Some(fintl::TimeZoneId { id: "Europe/Belgrade".to_string() }),
        temperature_unit: Some(fintl::TemperatureUnit::Celsius),
        hour_cycle: Some(fsettings::HourCycle::H23),
        ..fsettings::IntlSettings::default()
    };

    intl.set(&new_settings)
        .await
        .context("modify settings (FIDL)")?
        .map_err(|e| format_err!("{e:?}"))
        .context("modify settings (Settings server)")?;

    match event_stream.next().await.ok_or_else(|| format_err!("No event"))?? {
        fintl::PropertyProviderEvent::OnChange {} => {}
    };

    let updated_profile = property_provider.get_profile().await.context("get updated profile")?;
    let expected_profile = fintl::Profile {
        locales: Some(vec![fintl::LocaleId {
            id: "sr-RS-u-ca-gregory-fw-mon-hc-h23-ms-metric-nu-latn-tz-rsbeg".to_string(),
        }]),
        time_zones: Some(vec![fintl::TimeZoneId { id: "Europe/Belgrade".to_string() }]),
        temperature_unit: Some(fintl::TemperatureUnit::Celsius),
        ..fintl::Profile::default()
    };

    assert_eq!(updated_profile.locales, expected_profile.locales);
    assert_eq!(updated_profile.time_zones, expected_profile.time_zones);
    assert_eq!(updated_profile.temperature_unit, expected_profile.temperature_unit);

    Ok(())
}
