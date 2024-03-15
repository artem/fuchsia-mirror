// Copyright 2019 The Fuchsia Authors
//
// Licensed under a BSD-style license <LICENSE-BSD>, Apache License, Version 2.0
// <LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0>, or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to
// those terms.

use super::*;
use crate::{
    configuration::test_support::config_generator,
    cup_ecdsa::{test_support::make_cup_handler_for_test, StandardCupv2Handler},
    protocol::{
        request::{EventErrorCode, EventResult, EventType},
        Cohort,
    },
};
use futures::executor::block_on;
use pretty_assertions::assert_eq;
use serde_json::json;
use url::Url;

/// Test that a simple request's fields are all correct:
///
/// - All request fields are set properly from the Config
/// - That the App is translated to a protocol::request:::App
#[test]
fn test_simple_request() {
    let config = config_generator();
    let cup_handler = make_cup_handler_for_test();

    let (intermediate, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::OnDemand,
            ..RequestParams::default()
        },
    )
    .add_update_check(
        &App::builder()
            .id("app id")
            .version([5, 6, 7, 8])
            .fingerprint("fp")
            .cohort(Cohort::new("some-channel"))
            .build(),
    )
    .session_id(GUID::from_u128(1))
    .request_id(GUID::from_u128(2))
    .build_intermediate(Some(&cup_handler))
    .unwrap();

    // Assert that all the request fields are accurate (this is in their order of declaration)
    let request = intermediate.body.request;
    assert_eq!(request.protocol_version, "3.0");
    assert_eq!(request.updater, config.updater.name);
    assert_eq!(request.updater_version, config.updater.version.to_string());
    assert_eq!(request.install_source, InstallSource::OnDemand);
    assert_eq!(request.is_machine, true);
    assert_eq!(request.session_id, Some(GUID::from_u128(1)));
    assert_eq!(request.request_id, Some(GUID::from_u128(2)));

    // Just test that the config OS object was passed through (as opposed to manually comparing
    // all the fields)
    assert_eq!(request.os, config.os);

    // Validate that the App was added, with it's cohort and all of the other expected
    // fields for an update check request.
    let app = &request.apps[0];
    assert_eq!(app.id, "app id");
    assert_eq!(app.version, "5.6.7.8");
    assert_eq!(app.fingerprint, Some("fp".to_string()));
    assert_eq!(app.cohort, Some(Cohort::new("some-channel")));
    assert_eq!(app.update_check, Some(UpdateCheck::default()));
    assert!(app.events.is_empty());
    assert_eq!(app.ping, None);

    // Assert that the headers are set correctly
    let headers = intermediate.headers;
    assert_eq!(4, headers.len());
    assert!(headers.contains(&("content-type", "application/json".to_string())));
    assert!(headers.contains(&(HEADER_UPDATER_NAME, config.updater.name)));
    assert!(headers.contains(&(HEADER_APP_ID, "app id".to_string())));
    assert!(headers.contains(&(HEADER_INTERACTIVITY, "fg".to_string())));

    // Assert that the cup2key query parameter was set.
    let parsed_uri = Url::parse(&intermediate.uri).unwrap();
    let mut query_pairs = parsed_uri.query_pairs();
    assert_eq!(query_pairs.next().unwrap().0, "cup2key");
    assert_eq!(query_pairs.next(), None);
}

/// Test that a request sets the updatedisabled field when configured to do so.
#[test]
fn test_updates_disabled_request() {
    let config = config_generator();

    let (intermediate, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::OnDemand,
            disable_updates: true,
            ..RequestParams::default()
        },
    )
    .add_update_check(
        &App::builder()
            .id("app id 1")
            .version([1, 2, 3, 4])
            .fingerprint("fp")
            .cohort(Cohort::new("some-channel"))
            .build(),
    )
    .add_update_check(
        &App::builder()
            .id("app id 2")
            .version([5, 6, 7, 8])
            .cohort(Cohort::new("some-channel"))
            .build(),
    )
    .session_id(GUID::from_u128(1))
    .request_id(GUID::from_u128(2))
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();

    // Assert that all the request fields are accurate (this is in their order of declaration)
    let request = intermediate.body.request;
    assert_eq!(request.protocol_version, "3.0");
    assert_eq!(request.updater, config.updater.name);
    assert_eq!(request.updater_version, config.updater.version.to_string());
    assert_eq!(request.install_source, InstallSource::OnDemand);
    assert_eq!(request.is_machine, true);
    assert_eq!(request.session_id, Some(GUID::from_u128(1)));
    assert_eq!(request.request_id, Some(GUID::from_u128(2)));

    // Just test that the config OS object was passed through (as opposed to manually comparing
    // all the fields)
    assert_eq!(request.os, config.os);

    // Validate that the App was added, with it's cohort and all of the other expected
    // fields for an update check request.
    let app = &request.apps[0];
    assert_eq!(app.id, "app id 1");
    assert_eq!(app.version, "1.2.3.4");
    assert_eq!(app.fingerprint, Some("fp".to_string()));
    assert_eq!(app.cohort, Some(Cohort::new("some-channel")));
    assert_eq!(app.update_check, Some(UpdateCheck::disabled()));
    assert!(app.events.is_empty());
    assert_eq!(app.ping, None);

    // Validate that the second App also has its update check disabled
    let app = &request.apps[1];
    assert_eq!(app.update_check, Some(UpdateCheck::disabled()));
}

/// Test that a request sets the same version update field when configured to do so.
#[test]
fn test_same_version_update_request() {
    let config = config_generator();

    let (intermediate, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::OnDemand,
            offer_update_if_same_version: true,
            ..RequestParams::default()
        },
    )
    .add_update_check(
        &App::builder()
            .id("app id 1")
            .version([1, 2, 3, 4])
            .fingerprint("fp")
            .cohort(Cohort::new("some-channel"))
            .build(),
    )
    .add_update_check(
        &App::builder()
            .id("app id 2")
            .version([5, 6, 7, 8])
            .cohort(Cohort::new("some-channel"))
            .build(),
    )
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();

    let request = intermediate.body.request;
    // Validate that the App was added, with it's cohort and all of the other expected
    // fields for an update check request.
    let app = &request.apps[0];
    assert_eq!(app.id, "app id 1");
    assert_eq!(app.version, "1.2.3.4");
    assert_eq!(app.fingerprint, Some("fp".to_string()));
    assert_eq!(app.cohort, Some(Cohort::new("some-channel")));
    assert_eq!(
        app.update_check,
        Some(UpdateCheck {
            offer_update_if_same_version: true,
            ..UpdateCheck::default()
        })
    );
    assert!(app.events.is_empty());
    assert_eq!(app.ping, None);

    // Validate that the second App also has its same version update set.
    let app = &request.apps[1];
    assert_eq!(
        app.update_check,
        Some(UpdateCheck {
            offer_update_if_same_version: true,
            ..UpdateCheck::default()
        })
    );
}

/// Test that a request attaches the extras to the protocol App from the common App.
#[test]
fn test_app_includes_extras() {
    let config = config_generator();

    let (intermediate, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::OnDemand,
            ..RequestParams::default()
        },
    )
    .add_update_check(
        &App::builder()
            .id("app id")
            .version([5, 6, 7, 8])
            .extra_fields([("key".to_string(), "value".to_string())])
            .build(),
    )
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();
    let request = intermediate.body.request;

    // Validate that the App was added, with the expected extra fields
    let app = &request.apps[0];
    assert_eq!(app.id, "app id");
    assert_eq!(app.extra_fields.len(), 1);
    assert_eq!(app.extra_fields["key"], "value");
}

/// Test that a simple update check results in the correct HTTP request:
///  - service url
///  - headers
///  - request body
#[test]
fn test_single_request() {
    let config = config_generator();
    let cup_handler = make_cup_handler_for_test();

    let (http_request, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::OnDemand,
            ..RequestParams::default()
        },
    )
    .add_update_check(
        &App::builder()
            .id("app id")
            .version([5, 6, 7, 8])
            .cohort(Cohort::new("some-channel"))
            .build(),
    )
    .build(Some(&cup_handler))
    .unwrap();
    let (parts, body) = http_request.into_parts();

    // Assert that the HTTP method and uri are accurate
    assert_eq!(http::Method::POST, parts.method);
    let uri = parts.uri;
    assert!(uri.to_string().starts_with(&config.service_url));
    assert!(uri.query().unwrap().starts_with("cup2key="));

    // Assert that all the request body is correct, by generating an equivalent JSON one and
    // then comparing the resultant byte bodies
    let expected = json!({
        "request": {
            "protocol": "3.0",
            "updater": config.updater.name,
            "updaterversion": config.updater.version.to_string(),
            "installsource": "ondemand",
            "ismachine": true,
            "os": {
                "platform": config.os.platform,
                "version": config.os.version,
                "sp": config.os.service_pack,
                "arch": config.os.arch,
            },
            "app": [
                {
                    "appid": "app id",
                    "cohort": "some-channel",
                    "version": "5.6.7.8",
                    "updatecheck": {},
                },
            ],
        }
    });

    // Extract the request body out into a concatenated stream of Chunks, into a slice, so
    // that serde can be used to parse the body into a JSON Value object that can be compared
    // with the expected json constructed above.
    let body = block_on(hyper::body::to_bytes(body)).unwrap();
    let actual: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(expected, actual);

    // Assert that the headers are all correct
    let headers = parts.headers;
    assert_eq!(4, headers.len());
    assert_eq!(
        "application/json",
        headers.get("content-type").unwrap().to_str().unwrap()
    );
    assert_eq!(
        config.updater.name,
        headers.get(HEADER_UPDATER_NAME).unwrap().to_str().unwrap()
    );
    assert_eq!(
        "app id",
        headers.get(HEADER_APP_ID).unwrap().to_str().unwrap()
    );
    assert_eq!(
        "fg",
        headers.get(HEADER_INTERACTIVITY).unwrap().to_str().unwrap()
    );
}

/// Test that a ping is correctly added to an App entry.
#[test]
fn test_simple_ping() {
    let config = config_generator();

    let (intermediate, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::ScheduledTask,
            ..RequestParams::default()
        },
    )
    .add_ping(
        &App::builder()
            .id("ping app id")
            .version([6, 7, 8, 9])
            .cohort(Cohort::new("ping-channel"))
            .user_counting(UserCounting::ClientRegulatedByDate(Some(34)))
            .build(),
    )
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();

    // Validate that the App was added, with it's cohort
    let app = &intermediate.body.request.apps[0];
    assert_eq!(app.id, "ping app id");
    assert_eq!(app.version, "6.7.8.9");
    assert_eq!(app.cohort, Some(Cohort::new("ping-channel")));

    // And that the App has a Ping entry set, with the same values as was passed to the
    // Builder.
    let ping = app.ping.as_ref().unwrap();
    assert_eq!(ping.date_last_active, Some(34));
    assert_eq!(ping.date_last_roll_call, Some(34));

    // Assert that the headers are set correctly
    let headers = intermediate.headers;
    assert_eq!(4, headers.len());
    assert!(headers.contains(&("content-type", "application/json".to_string())));
    assert!(headers.contains(&(HEADER_UPDATER_NAME, config.updater.name)));
    assert!(headers.contains(&(HEADER_APP_ID, "ping app id".to_string())));
    assert!(headers.contains(&(HEADER_INTERACTIVITY, "bg".to_string())));
}

/// Test that an event is properly added to an App entry
#[test]
fn test_simple_event() {
    let config = config_generator();

    let (http_request, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::ScheduledTask,
            ..RequestParams::default()
        },
    )
    .add_event(
        &App::builder()
            .id("event app id")
            .version([6, 7, 8, 9])
            .cohort(Cohort::new("event-channel"))
            .build(),
        Event {
            event_type: EventType::UpdateDownloadStarted,
            event_result: EventResult::Success,
            errorcode: Some(EventErrorCode::Installation),
            ..Event::default()
        },
    )
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();

    let request = http_request.body.request;

    let app = &request.apps[0];
    assert_eq!(app.id, "event app id");
    assert_eq!(app.version, "6.7.8.9");
    assert_eq!(app.cohort, Some(Cohort::new("event-channel")));

    let event = &app.events[0];
    assert_eq!(event.event_type, EventType::UpdateDownloadStarted);
    assert_eq!(event.event_result, EventResult::Success);
    assert_eq!(event.errorcode, Some(EventErrorCode::Installation));
}

/// Test that multiple events are properly added to an App entry
#[test]
fn test_multiple_events() {
    let config = config_generator();

    // Setup the first app and its cohort
    let app_1 = App::builder()
        .id("event app id")
        .version([6, 7, 8, 9])
        .cohort(Cohort::new("event-channel"))
        .build();

    // Make the call to the RequestBuilder that is being tested.
    let (http_request, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::ScheduledTask,
            ..RequestParams::default()
        },
    )
    .add_event(
        &app_1,
        Event {
            event_type: EventType::UpdateDownloadStarted,
            event_result: EventResult::Success,
            errorcode: Some(EventErrorCode::Installation),
            ..Event::default()
        },
    )
    .add_event(
        &app_1,
        Event {
            event_type: EventType::UpdateDownloadFinished,
            event_result: EventResult::Error,
            errorcode: Some(EventErrorCode::DeniedByPolicy),
            ..Event::default()
        },
    )
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();

    let request = http_request.body.request;

    // Validate that the resultant Request has the right fields and events

    let app = &request.apps[0];
    assert_eq!(app.id, "event app id");
    assert_eq!(app.version, "6.7.8.9");
    assert_eq!(app.cohort, Some(Cohort::new("event-channel")));

    let event = &app.events[0];
    assert_eq!(event.event_type, EventType::UpdateDownloadStarted);
    assert_eq!(event.event_result, EventResult::Success);
    assert_eq!(event.errorcode, Some(EventErrorCode::Installation));

    let event = &app.events[1];
    assert_eq!(event.event_type, EventType::UpdateDownloadFinished);
    assert_eq!(event.event_result, EventResult::Error);
    assert_eq!(event.errorcode, Some(EventErrorCode::DeniedByPolicy));
}

/// When adding multiple apps to a request, a ping or an event needs to be attached to the
/// correct app entry in the protocol request.  The next few tests are centered on validating
/// that in various scenarios.

/// This test ensures that if the matching app entry is the first one in the request, that the
/// ping is attached to it (and not the last that was added).
#[test]
fn test_ping_added_to_first_app_update_entry() {
    let config = config_generator();

    // Setup the first app and its cohort
    let app_1 = App::builder()
        .id("first app id")
        .version([1, 2, 3, 4])
        .cohort(Cohort::new("some-channel"))
        .user_counting(UserCounting::ClientRegulatedByDate(Some(34)))
        .build();

    // Setup the second app and its cohort
    let app_2 = App::builder()
        .id("second app id")
        .version([5, 6, 7, 8])
        .cohort(Cohort::new("some-other-channel"))
        .build();

    // Now make the call to the RequestBuilder that is being tested.
    let (http_request, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::ScheduledTask,
            ..RequestParams::default()
        },
    )
    .add_update_check(&app_1)
    .add_update_check(&app_2)
    .add_ping(&app_1)
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();
    let request = http_request.body.request;

    // Validate the resultant Request is correct.

    // There should only be the two app entries.
    assert_eq!(request.apps.len(), 2);

    // The first app should have the ping attached to it.
    let app = &request.apps[0];
    assert_eq!(app.id, "first app id");
    assert_eq!(app.version, "1.2.3.4");
    assert_eq!(app.cohort, Some(Cohort::new("some-channel")));
    let ping = &app.ping.as_ref().unwrap();
    assert_eq!(ping.date_last_active, Some(34));
    assert_eq!(ping.date_last_roll_call, Some(34));

    // And the second app should not.
    let app = &request.apps[1];
    assert_eq!(app.id, "second app id");
    assert_eq!(app.version, "5.6.7.8");
    assert_eq!(app.cohort, Some(Cohort::new("some-other-channel")));
    assert_eq!(app.ping, None);
}

/// This test ensures that if the matching app entry is the second one in the request, that the
/// ping is attached to it (and not to the first app that was added).
#[test]
fn test_ping_added_to_second_app_update_entry() {
    let config = config_generator();

    // Setup the first app and its cohort
    let app_1 = App::builder()
        .id("first app id")
        .version([1, 2, 3, 4])
        .cohort(Cohort::new("some-channel"))
        .build();

    // Setup the second app and its cohort
    let app_2 = App::builder()
        .id("second app id")
        .version([5, 6, 7, 8])
        .cohort(Cohort::new("some-other-channel"))
        .user_counting(UserCounting::ClientRegulatedByDate(Some(34)))
        .build();

    // Now make the call to the RequestBuilder that is being tested.
    let builder = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::ScheduledTask,
            ..RequestParams::default()
        },
    )
    .add_update_check(&app_1)
    .add_update_check(&app_2)
    .add_ping(&app_2);

    let (http_request, _request_metadata) = builder
        .build_intermediate(None::<&StandardCupv2Handler>)
        .unwrap();
    let request = http_request.body.request;

    // Validate that the resultant request is correct.

    // There should only be the two entries.
    assert_eq!(request.apps.len(), 2);

    // The first app should not have the ping attached to it.
    let app = &request.apps[0];
    assert_eq!(app.id, "first app id");
    assert_eq!(app.version, "1.2.3.4");
    assert_eq!(app.cohort, Some(Cohort::new("some-channel")));

    // And the second app should.
    let app = &request.apps[1];
    assert_eq!(app.id, "second app id");
    assert_eq!(app.version, "5.6.7.8");
    assert_eq!(app.cohort, Some(Cohort::new("some-other-channel")));

    let ping = app.ping.as_ref().unwrap();
    assert_eq!(ping.date_last_active, Some(34));
    assert_eq!(ping.date_last_roll_call, Some(34));
}

/// This test ensures that if the matching app entry is the first one in the request, that the
/// event is attached to it (and not the last that was added).
#[test]
fn test_event_added_to_first_app_update_entry() {
    let config = config_generator();

    // Setup the first app and its cohort
    let app_1 = App::builder()
        .id("first app id")
        .version([1, 2, 3, 4])
        .cohort(Cohort::new("some-channel"))
        .build();

    // Setup the second app and its cohort
    let app_2 = App::builder()
        .id("second app id")
        .version([5, 6, 7, 8])
        .cohort(Cohort::new("some-other-channel"))
        .build();

    // Now make the call to the RequestBuilder that is being tested.
    let (http_request, _request_metadata) = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::ScheduledTask,
            ..RequestParams::default()
        },
    )
    .add_update_check(&app_1)
    .add_update_check(&app_2)
    .add_event(
        &app_1,
        Event {
            event_type: EventType::UpdateDownloadFinished,
            event_result: EventResult::Success,
            errorcode: Some(EventErrorCode::Installation),
            ..Event::default()
        },
    )
    .build_intermediate(None::<&StandardCupv2Handler>)
    .unwrap();

    let request = http_request.body.request;

    // There should only be the two entries.
    assert_eq!(request.apps.len(), 2);

    // The first app should have the event attached to it.
    let app = &request.apps[0];
    assert_eq!(app.id, "first app id");
    assert_eq!(app.version, "1.2.3.4");
    assert_eq!(app.cohort, Some(Cohort::new("some-channel")));
    let event = &app.events[0];
    assert_eq!(event.event_type, EventType::UpdateDownloadFinished);
    assert_eq!(event.event_result, EventResult::Success);
    assert_eq!(event.errorcode, Some(EventErrorCode::Installation));

    // And the second app should not.
    let app = &request.apps[1];
    assert_eq!(app.id, "second app id");
    assert_eq!(app.version, "5.6.7.8");
    assert_eq!(app.cohort, Some(Cohort::new("some-other-channel")));
    assert!(app.events.is_empty());
}

/// This test ensures that if the matching app entry is the second one in the request, that the
/// event is attached to it (and not to the first app that was added).
#[test]
fn test_event_added_to_second_app_update_entry() {
    let config = config_generator();

    // Setup the first app and its cohort
    let app_1 = App::builder()
        .id("first app id")
        .version([1, 2, 3, 4])
        .cohort(Cohort::new("some-channel"))
        .build();

    // Setup the second app and its cohort
    let app_2 = App::builder()
        .id("second app id")
        .version([5, 6, 7, 8])
        .cohort(Cohort::new("some-other-channel"))
        .build();

    // Now make the call to the RequestBuilder that is being tested.
    let builder = RequestBuilder::new(
        &config,
        &RequestParams {
            source: InstallSource::ScheduledTask,
            ..RequestParams::default()
        },
    )
    .add_update_check(&app_1)
    .add_update_check(&app_2)
    .add_event(
        &app_2,
        Event {
            event_type: EventType::UpdateDownloadFinished,
            event_result: EventResult::Success,
            errorcode: Some(EventErrorCode::Installation),
            ..Event::default()
        },
    );

    let (http_request, _request_metadata) = builder
        .build_intermediate(None::<&StandardCupv2Handler>)
        .unwrap();
    let request = http_request.body.request;

    // There should only be the two entries.
    assert_eq!(request.apps.len(), 2);

    // The first app should not have the event attached.
    let app = &request.apps[0];
    assert_eq!(app.id, "first app id");
    assert_eq!(app.version, "1.2.3.4");
    assert_eq!(app.cohort, Some(Cohort::new("some-channel")));
    assert!(app.events.is_empty());

    // And the second app should.
    let app = &request.apps[1];
    assert_eq!(app.id, "second app id");
    assert_eq!(app.version, "5.6.7.8");
    assert_eq!(app.cohort, Some(Cohort::new("some-other-channel")));

    assert_eq!(app.events.len(), 1);
    let event = &app.events[0];
    assert_eq!(event.event_type, EventType::UpdateDownloadFinished);
    assert_eq!(event.event_result, EventResult::Success);
    assert_eq!(event.errorcode, Some(EventErrorCode::Installation));
}
