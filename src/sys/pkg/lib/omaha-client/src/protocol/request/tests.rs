// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn basic_serialization_test() {
    let expected = json!({
        "request": {
            "protocol": "3.0",
            "updater": "some_updater",
            "updaterversion": "1.0",
            "installsource": "ondemand",
            "ismachine": true,
            "requestid": "{00000000-0000-0000-0000-000000000000}",
            "sessionid": "{00000000-0000-0000-0000-000000000000}",
            "os": {
                "platform": "some_platform",
                "version": "4.5",
                "sp": "0.1",
                "arch": "some_architecture"
            },
            "app": [
                {
                    "appid": "{00000000-0000-0000-0000-000000000001}",
                    "version": "1.2.3.4",
                    "updatecheck": {},
                },
            ],
        }
    });

    let request = RequestWrapper {
        request: Request {
            protocol_version: "3.0".to_string(),
            updater: "some_updater".to_string(),
            updater_version: "1.0".to_string(),
            install_source: InstallSource::OnDemand,
            is_machine: true,
            request_id: Some(GUID::default()),
            session_id: Some(GUID::default()),
            os: OS {
                platform: "some_platform".to_string(),
                version: "4.5".to_string(),
                service_pack: "0.1".to_string(),
                arch: "some_architecture".to_string(),
            },
            apps: vec![App {
                id: "{00000000-0000-0000-0000-000000000001}".to_string(),
                version: "1.2.3.4".to_string(),
                update_check: Some(UpdateCheck::default()),
                ..App::default()
            }],
        },
    };

    // serde_json::to_value() is used to ensure that the fields are ordered the same as the JSON
    // generated by the json!() macro.
    assert_eq!(expected, serde_json::to_value(request).unwrap());
}

#[test]
fn basic_serialization_test_app_with_extras() {
    let expected = json!({
        "request": {
            "protocol": "3.0",
            "updater": "some_updater",
            "updaterversion": "1.0",
            "installsource": "ondemand",
            "ismachine": true,
            "requestid": "{00000000-0000-0000-0000-000000000000}",
            "sessionid": "{00000000-0000-0000-0000-000000000000}",
            "os": {
                "platform": "some_platform",
                "version": "4.5",
                "sp": "0.1",
                "arch": "some_architecture"
            },
            "app": [
                {
                    "appid": "{00000000-0000-0000-0000-000000000001}",
                    "version": "1.2.3.4",
                    "updatecheck": {},
                    "key1": "value1",
                    "key2": "value2",
                },
            ],
        }
    });

    let mut app = App {
        id: "{00000000-0000-0000-0000-000000000001}".to_string(),
        version: "1.2.3.4".to_string(),
        update_check: Some(UpdateCheck::default()),
        ..App::default()
    };
    app.extra_fields.insert("key1".to_string(), "value1".to_string());
    app.extra_fields.insert("key2".to_string(), "value2".to_string());
    let request = RequestWrapper {
        request: Request {
            protocol_version: "3.0".to_string(),
            updater: "some_updater".to_string(),
            updater_version: "1.0".to_string(),
            install_source: InstallSource::OnDemand,
            is_machine: true,
            request_id: Some(GUID::default()),
            session_id: Some(GUID::default()),
            os: OS {
                platform: "some_platform".to_string(),
                version: "4.5".to_string(),
                service_pack: "0.1".to_string(),
                arch: "some_architecture".to_string(),
            },
            apps: vec![app],
        },
    };

    // serde_json::to_value() is used to ensure that the fields are ordered the same as the JSON
    // generated by the json!() macro.
    assert_eq!(expected, serde_json::to_value(request).unwrap());
}

/// This test exists less to confirm that this behavior works as intended, but to show that this
/// behavior occurs, and needs to be kept in mind when using the `extra_fields` map.
#[test]
fn basic_serialization_test_app_with_extras_will_overwrite_protocol_fields() {
    let expected = json!({
        "request": {
            "protocol": "3.0",
            "updater": "some_updater",
            "updaterversion": "1.0",
            "installsource": "ondemand",
            "ismachine": true,
            "os": {
                "platform": "some_platform",
                "version": "4.5",
                "sp": "0.1",
                "arch": "some_architecture"
            },
            "app": [
                {
                    "appid": "{00000000-0000-0000-0000-000000000001}",
                    "version": "5.6.7.8",
                },
            ],
        }
    });

    let mut app = App {
        id: "{00000000-0000-0000-0000-000000000001}".to_string(),
        version: "1.2.3.4".to_string(),
        ..App::default()
    };

    // attempt to overwrite the version via "extra_fields", do not do this.
    app.extra_fields.insert("version".to_string(), "5.6.7.8".to_string());

    let request = RequestWrapper {
        request: Request {
            protocol_version: "3.0".to_string(),
            updater: "some_updater".to_string(),
            updater_version: "1.0".to_string(),
            install_source: InstallSource::OnDemand,
            is_machine: true,
            request_id: None,
            session_id: None,
            os: OS {
                platform: "some_platform".to_string(),
                version: "4.5".to_string(),
                service_pack: "0.1".to_string(),
                arch: "some_architecture".to_string(),
            },
            apps: vec![app],
        },
    };

    // serde_json::to_value() is used to ensure that the fields are ordered the same as the JSON
    // generated by the json!() macro.
    assert_eq!(expected, serde_json::to_value(request).unwrap());
}

#[test]
fn basic_ping_serialization_test() {
    let expected = json!({
        "request":{
            "protocol": "3.0",
            "updater": "some_updater",
            "updaterversion": "1.0",
            "installsource": "scheduledtask",
            "ismachine": true,
            "os": {
                "platform": "some_platform",
                "version": "4.5",
                "sp": "0.1",
                "arch": "some_architecture"
            },
            "app": [
                {
                    "appid": "{00000000-0000-0000-0000-000000000001}",
                    "version": "1.2.3.4",
                    "ping": {
                        "ad": 2000,
                        "rd": 2001,
                    }
                },
            ],
        }
    });

    let request = RequestWrapper {
        request: Request {
            protocol_version: "3.0".to_string(),
            updater: "some_updater".to_string(),
            updater_version: "1.0".to_string(),
            install_source: InstallSource::ScheduledTask,
            is_machine: true,
            request_id: None,
            session_id: None,
            os: OS {
                platform: "some_platform".to_string(),
                version: "4.5".to_string(),
                service_pack: "0.1".to_string(),
                arch: "some_architecture".to_string(),
            },
            apps: vec![App {
                id: "{00000000-0000-0000-0000-000000000001}".to_string(),
                version: "1.2.3.4".to_string(),
                ping: Some(Ping { date_last_active: Some(2000), date_last_roll_call: Some(2001) }),
                ..App::default()
            }],
        },
    };

    // serde_json::to_value() is used to ensure that the fields are ordered the same as the JSON
    // generated by the json!() macro.
    assert_eq!(expected, serde_json::to_value(request).unwrap());
}

#[test]
fn basic_event_serialization_test() {
    let expected = json!({
        "request": {
            "protocol": "3.0",
            "updater": "some_updater",
            "updaterversion": "1.0",
            "installsource": "ondemand",
            "ismachine": true,
            "os": {
                "platform": "some_platform",
                "version": "4.5",
                "sp": "0.1",
                "arch": "some_architecture"
            },
            "app": [
                {
                    "appid": "{00000000-0000-0000-0000-000000000001}",
                    "version": "1.2.3.4",
                    "event": [{
                        "eventtype": 2,
                        "eventresult": 1,
                        "errorcode": 0
                    }],
                },
            ],
        }
    });

    let request = RequestWrapper {
        request: Request {
            protocol_version: "3.0".to_string(),
            updater: "some_updater".to_string(),
            updater_version: "1.0".to_string(),
            install_source: InstallSource::OnDemand,
            is_machine: true,
            request_id: None,
            session_id: None,
            os: OS {
                platform: "some_platform".to_string(),
                version: "4.5".to_string(),
                service_pack: "0.1".to_string(),
                arch: "some_architecture".to_string(),
            },
            apps: vec![App {
                id: "{00000000-0000-0000-0000-000000000001}".to_string(),
                version: "1.2.3.4".to_string(),
                events: vec![Event {
                    event_type: EventType::InstallComplete,
                    event_result: EventResult::Success,
                    errorcode: Some(EventErrorCode::ParseResponse),
                    ..Event::default()
                }],
                ..App::default()
            }],
        },
    };

    // serde_json::to_value() is used to ensure that the fields are ordered the same as the JSON
    // generated by the json!() macro.
    assert_eq!(expected, serde_json::to_value(request).unwrap());
}

#[test]
fn multiple_event_serialization_test() {
    let expected = json!({
        "request": {
            "protocol": "3.0",
            "updater": "some_updater",
            "updaterversion": "1.0",
            "installsource": "ondemand",
            "ismachine": true,
            "os": {
                "platform": "some_platform",
                "version": "4.5",
                "sp": "0.1",
                "arch": "some_architecture"
            },
            "app": [
                {
                    "appid": "{00000000-0000-0000-0000-000000000001}",
                    "version": "1.2.3.4",
                    "event": [{
                        "eventtype": 2,
                        "eventresult": 1,
                        "errorcode": 0
                    },
                    {
                        "eventtype": 3,
                        "eventresult": 2,
                    },
                    {
                        "eventtype": 54,
                        "eventresult": 1,
                        "previousversion": "3.4.5.6",
                        "nextversion": "4.5.6.7",
                        "download_time_ms": 42,
                    }],
                },
            ],
        }
    });

    let request = RequestWrapper {
        request: Request {
            protocol_version: "3.0".to_string(),
            updater: "some_updater".to_string(),
            updater_version: "1.0".to_string(),
            install_source: InstallSource::OnDemand,
            is_machine: true,
            request_id: None,
            session_id: None,
            os: OS {
                platform: "some_platform".to_string(),
                version: "4.5".to_string(),
                service_pack: "0.1".to_string(),
                arch: "some_architecture".to_string(),
            },
            apps: vec![App {
                id: "{00000000-0000-0000-0000-000000000001}".to_string(),
                version: "1.2.3.4".to_string(),
                events: vec![
                    Event {
                        event_type: EventType::InstallComplete,
                        event_result: EventResult::Success,
                        errorcode: Some(EventErrorCode::ParseResponse),
                        ..Event::default()
                    },
                    Event {
                        event_type: EventType::UpdateComplete,
                        event_result: EventResult::SuccessAndRestartRequired,
                        ..Event::default()
                    },
                    Event {
                        event_type: EventType::RebootedAfterUpdate,
                        event_result: EventResult::Success,
                        previous_version: Some("3.4.5.6".to_string()),
                        next_version: Some("4.5.6.7".to_string()),
                        download_time_ms: Some(42),
                        ..Event::default()
                    },
                ],
                ..App::default()
            }],
        },
    };

    // serde_json::to_value() is used to ensure that the fields are ordered the same as the JSON
    // generated by the json!() macro.
    assert_eq!(expected, serde_json::to_value(request).unwrap());
}

#[test]
fn all_fields_serialization_test() {
    let expected = json!({
        "request": {
            "protocol": "3.0",
            "updater": "some_updater",
            "updaterversion": "1.0",
            "installsource": "ondemand",
            "ismachine": true,
            "requestid": "{00000000-0000-0000-0000-000000000000}",
            "sessionid": "{00000000-0000-0000-0000-000000000000}",
            "os": {
                "platform": "some_platform",
                "version": "4.5",
                "sp": "0.1",
                "arch": "some_architecture"
            },
            "app": [
                {
                    "appid": "{00000000-0000-0000-0000-000000000001}",
                    "version": "1.2.3.4",
                    "fp": "some_fingerprint",
                    "cohort": "1",
                    "cohorthint": "stable",
                    "cohortname": "Production",
                    "updatecheck": { "updatedisabled": true},
                    "ping": {
                        "ad": 300,
                        "rd": 45,
                    },
                    "event": [{
                        "eventtype": 2,
                        "eventresult": 1,
                        "errorcode": 0
                    },
                    {
                        "eventtype": 3,
                        "eventresult": 2,
                    },
                    {
                        "eventtype": 54,
                        "eventresult": 1,
                        "previousversion": "3.4.5.6",
                    }],
                },
            ],
        }
    });

    let request = RequestWrapper {
        request: Request {
            protocol_version: "3.0".to_string(),
            updater: "some_updater".to_string(),
            updater_version: "1.0".to_string(),
            install_source: InstallSource::OnDemand,
            is_machine: true,
            request_id: Some(GUID::default()),
            session_id: Some(GUID::default()),
            os: OS {
                platform: "some_platform".to_string(),
                version: "4.5".to_string(),
                service_pack: "0.1".to_string(),
                arch: "some_architecture".to_string(),
            },
            apps: vec![App {
                id: "{00000000-0000-0000-0000-000000000001}".to_string(),
                version: "1.2.3.4".to_string(),
                fingerprint: Some("some_fingerprint".to_string()),
                cohort: Some(Cohort {
                    id: Some("1".to_string()),
                    hint: Some("stable".to_string()),
                    name: Some("Production".to_string()),
                }),
                update_check: Some(UpdateCheck::disabled()),
                ping: Some(Ping { date_last_active: Some(300), date_last_roll_call: Some(45) }),
                events: vec![
                    Event {
                        event_type: EventType::InstallComplete,
                        event_result: EventResult::Success,
                        errorcode: Some(EventErrorCode::ParseResponse),
                        ..Event::default()
                    },
                    Event {
                        event_type: EventType::UpdateComplete,
                        event_result: EventResult::SuccessAndRestartRequired,
                        ..Event::default()
                    },
                    Event {
                        event_type: EventType::RebootedAfterUpdate,
                        event_result: EventResult::Success,
                        previous_version: Some("3.4.5.6".to_string()),
                        ..Event::default()
                    },
                ],
                ..App::default()
            }],
        },
    };

    // serde_json::to_value() is used to ensure that the fields are ordered the same as the JSON
    // generated by the json!() macro.
    assert_eq!(expected, serde_json::to_value(request).unwrap());
}
