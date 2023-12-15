// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Context, Error},
    component_events::{
        events::{Destroyed, Discovered, Event, EventStream, Started},
        matcher::EventMatcher,
        sequence::*,
    },
    fidl::endpoints::ServiceMarker,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_examples as fecho,
    fidl_fuchsia_examples_services as fexamples, fidl_fuchsia_io as fio,
    fidl_fuchsia_sys2 as fsys2,
    fuchsia_component::client,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, Ref, Route, ScopedInstance},
    futures::{channel::mpsc, FutureExt, SinkExt, StreamExt},
    moniker::{ChildName, ChildNameBase},
    test_case::test_case,
    tracing::*,
};

const BRANCHES_COLLECTION: &str = "branches";
const BRANCH_ONECOLL_COMPONENT_URL: &str = "#meta/service-routing-branch-onecoll.cm";
const BRANCH_TWOCOLL_COMPONENT_URL: &str = "#meta/service-routing-branch-twocoll.cm";
const A_ONECOLL_MONIKER: &str = "account_providers:a";
const B_ONECOLL_MONIKER: &str = "account_providers:b";
const A_TWOCOLL_MONIKER: &str = "account_providers_1:a";
const B_TWOCOLL_MONIKER: &str = "account_providers_2:b";
const ECHO_URL: &str = "#meta/multi-instance-echo-provider.cm";

struct TestInput {
    url: &'static str,
    provider_a_moniker: &'static str,
    provider_b_moniker: &'static str,
}

impl TestInput {
    fn new(test_type: TestType) -> Self {
        match test_type {
            TestType::OneCollection => Self {
                url: BRANCH_ONECOLL_COMPONENT_URL,
                provider_a_moniker: A_ONECOLL_MONIKER,
                provider_b_moniker: B_ONECOLL_MONIKER,
            },
            TestType::TwoCollections => Self {
                url: BRANCH_TWOCOLL_COMPONENT_URL,
                provider_a_moniker: A_TWOCOLL_MONIKER,
                provider_b_moniker: B_TWOCOLL_MONIKER,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TestType {
    OneCollection,
    TwoCollections,
}

#[test_case(TestType::OneCollection)]
#[test_case(TestType::TwoCollections)]
#[fuchsia::test]
async fn list_instances_test(test_type: TestType) {
    let input = TestInput::new(test_type);
    let branch = start_branch(&input).await.expect("failed to start branch component");
    start_provider(&branch, input.provider_a_moniker).await.expect("failed to start provider a");
    start_provider(&branch, input.provider_b_moniker).await.expect("failed to start provider b");

    // List the instances in the BankAccount service.
    let service_dir = fuchsia_fs::directory::open_directory(
        branch.get_exposed_dir(),
        fexamples::BankAccountMarker::SERVICE_NAME,
        fio::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service dir");

    let instances: Vec<String> = fuchsia_fs::directory::readdir(&service_dir)
        .await
        .expect("failed to read entries from service dir")
        .into_iter()
        .map(|dirent| dirent.name)
        .collect();
    verify_instances(instances, 2);
}

#[test_case(TestType::OneCollection)]
#[test_case(TestType::TwoCollections)]
#[fuchsia::test]
async fn connect_to_instances_test(test_type: TestType) {
    let input = TestInput::new(test_type);
    let branch = start_branch(&input).await.expect("failed to start branch component");
    start_provider(&branch, input.provider_a_moniker).await.expect("failed to start provider a");
    start_provider(&branch, input.provider_b_moniker).await.expect("failed to start provider b");

    // List the instances in the BankAccount service.
    let service_dir = fuchsia_fs::directory::open_directory(
        branch.get_exposed_dir(),
        fexamples::BankAccountMarker::SERVICE_NAME,
        fio::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service dir");
    let instances = fuchsia_fs::directory::readdir(&service_dir)
        .await
        .expect("failed to read entries from service dir")
        .into_iter()
        .map(|dirent| dirent.name);

    // Connect to every instance and ensure the protocols are functional.
    for instance in instances {
        let proxy = client::connect_to_service_instance_at_dir::<fexamples::BankAccountMarker>(
            branch.get_exposed_dir(),
            &instance,
        )
        .expect("failed to connect to service instance");
        let read_only_account = proxy.connect_to_read_only().expect("read_only protocol");
        let owner = read_only_account.get_owner().await.expect("failed to get owner");
        let initial_balance = read_only_account.get_balance().await.expect("failed to get_balance");
        info!("retrieved account for owner '{}' with balance ${}", &owner, &initial_balance);

        let read_write_account = proxy.connect_to_read_write().expect("read_write protocol");
        assert_eq!(read_write_account.get_owner().await.expect("failed to get_owner"), owner);
        assert_eq!(
            read_write_account.get_balance().await.expect("failed to get_balance"),
            initial_balance
        );
    }
}

#[test_case(TestType::OneCollection)]
#[test_case(TestType::TwoCollections)]
#[fuchsia::test]
async fn create_destroy_instance_test(test_type: TestType) {
    let input = TestInput::new(test_type);
    let branch = start_branch(&input).await.expect("failed to start branch component");
    start_provider(&branch, input.provider_a_moniker).await.expect("failed to start provider a");
    start_provider(&branch, input.provider_b_moniker).await.expect("failed to start provider b");

    // List the instances in the BankAccount service.
    let service_dir = fuchsia_fs::directory::open_directory(
        branch.get_exposed_dir(),
        fexamples::BankAccountMarker::SERVICE_NAME,
        fio::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service dir");

    let instances: Vec<String> = fuchsia_fs::directory::readdir(&service_dir)
        .await
        .expect("failed to read entries from service dir")
        .into_iter()
        .map(|dirent| dirent.name)
        .collect();

    // The aggregated service directory should contain instances from the provider.
    verify_instances(instances, 2);

    // Destroy provider a.
    destroy_provider(&branch, input.provider_a_moniker)
        .await
        .expect("failed to destroy provider a");

    let instances: Vec<String> = fuchsia_fs::directory::readdir(&service_dir)
        .await
        .expect("failed to read entries from service dir")
        .into_iter()
        .map(|dirent| dirent.name)
        .collect();

    // The provider's instances should be removed from the aggregated service directory.
    verify_instances(instances, 1);
}

#[fuchsia::test]
async fn static_aggregate_offer() {
    let builder = RealmBuilder::new().await.unwrap();
    // Add subrealm to test offer from parent.
    let parent_echo = builder.add_child("echo", ECHO_URL, ChildOptions::new()).await.unwrap();
    let subrealm = builder.add_child_realm("realm", ChildOptions::new().eager()).await.unwrap();
    // Initialize subrealm from echo component to test "offer from self"
    let echo_decl = builder.get_component_decl(&parent_echo).await.unwrap();
    subrealm.replace_realm_decl(echo_decl).await.unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::service::<fecho::EchoServiceMarker>())
                .from(&parent_echo)
                .to(&subrealm),
        )
        .await
        .unwrap();
    let echo = subrealm.add_child("echo", ECHO_URL, ChildOptions::new()).await.unwrap();
    let (handles_sender, mut handles_receiver) = mpsc::unbounded();
    let consumer = subrealm
        .add_local_child(
            "consumer",
            move |handles| {
                let mut handles_sender = handles_sender.clone();
                async move {
                    handles_sender.send(handles).await.unwrap();
                    Ok(())
                }
                .boxed()
            },
            ChildOptions::new().eager(),
        )
        .await
        .unwrap();
    subrealm
        .add_route(
            Route::new()
                .capability(Capability::service::<fecho::EchoServiceMarker>())
                .from(&echo)
                .to(&consumer),
        )
        .await
        .unwrap();
    subrealm
        .add_route(
            Route::new()
                .capability(Capability::service::<fecho::EchoServiceMarker>())
                .from(Ref::parent())
                .to(&consumer),
        )
        .await
        .unwrap();
    subrealm
        .add_route(
            Route::new()
                .capability(Capability::service::<fecho::EchoServiceMarker>())
                .from(Ref::self_())
                .to(&consumer),
        )
        .await
        .unwrap();
    let _realm = builder.build().await.unwrap();

    let handles = handles_receiver.next().await.unwrap();
    let echo_svc = handles.open_service::<fecho::EchoServiceMarker>().unwrap();
    let instances = fuchsia_fs::directory::readdir(&echo_svc)
        .await
        .unwrap()
        .into_iter()
        .map(|dirent| dirent.name);
    // parent, self, child in the aggregate x 3 instances each
    assert_eq!(instances.len(), 3 * 3);
    drop(echo_svc);

    // Connect to every instance and ensure the protocols are functional.
    for instance in instances {
        let proxy =
            handles.connect_to_service_instance::<fecho::EchoServiceMarker>(&instance).unwrap();

        let echo_proxy = proxy.connect_to_regular_echo().unwrap();
        let res = echo_proxy.echo_string("hello").await.unwrap();
        assert!(res.ends_with("hello"));

        let echo_proxy = proxy.connect_to_reversed_echo().unwrap();
        let res = echo_proxy.echo_string("hello").await.unwrap();
        assert!(res.ends_with("olleh"));
    }
}

#[fuchsia::test]
async fn static_aggregate_expose() {
    let builder = RealmBuilder::new().await.unwrap();
    // Add placeholder echo component so we can get its decl. Then initialize a subrealm with this
    // decl to test "expose from self".
    let placeholder_echo =
        builder.add_child("placeholder_echo", ECHO_URL, ChildOptions::new()).await.unwrap();
    let subrealm = builder.add_child_realm("realm", ChildOptions::new()).await.unwrap();
    let echo_decl = builder.get_component_decl(&placeholder_echo).await.unwrap();
    subrealm.replace_realm_decl(echo_decl).await.unwrap();
    let echo = subrealm.add_child("echo", ECHO_URL, ChildOptions::new()).await.unwrap();
    subrealm
        .add_route(
            Route::new()
                .capability(Capability::service::<fecho::EchoServiceMarker>())
                .from(&echo)
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    subrealm
        .add_route(
            Route::new()
                .capability(Capability::service::<fecho::EchoServiceMarker>())
                .from(Ref::self_())
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::service::<fecho::EchoServiceMarker>())
                .from(&subrealm)
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    let realm = builder.build().await.unwrap();

    let exposed_dir = realm.root.get_exposed_dir();
    let echo_svc = fuchsia_fs::directory::open_directory(
        exposed_dir,
        fecho::EchoServiceMarker::SERVICE_NAME,
        fio::OpenFlags::empty(),
    )
    .await
    .unwrap();
    let instances = fuchsia_fs::directory::readdir(&echo_svc)
        .await
        .unwrap()
        .into_iter()
        .map(|dirent| dirent.name);
    // self, child in the aggregate x 3 instances each
    assert_eq!(instances.len(), 3 * 2);
    drop(echo_svc);

    // Connect to every instance and ensure the protocols are functional.
    for instance in instances {
        let proxy = client::connect_to_service_instance_at_dir::<fecho::EchoServiceMarker>(
            exposed_dir,
            &instance,
        )
        .unwrap();

        let echo_proxy = proxy.connect_to_regular_echo().unwrap();
        let res = echo_proxy.echo_string("hello").await.unwrap();
        assert!(res.ends_with("hello"));

        let echo_proxy = proxy.connect_to_reversed_echo().unwrap();
        let res = echo_proxy.echo_string("hello").await.unwrap();
        assert!(res.ends_with("olleh"));
    }
}

/// Starts a branch child component.
async fn start_branch(input: &TestInput) -> Result<ScopedInstance, Error> {
    let event_stream = EventStream::open_at_path("/events/discovered")
        .await
        .context("failed to subscribe to EventSource")?;

    let branch = ScopedInstance::new(BRANCHES_COLLECTION.to_string(), input.url.to_string())
        .await
        .context("failed to create branch component instance")?;
    branch.start_with_binder_sync().await?;

    // Wait for the providers to be discovered (created) to ensure that
    // subsequent calls to `start_provider` can start them.
    EventSequence::new()
        .has_subset(
            vec![
                EventMatcher::ok().r#type(Discovered::TYPE).moniker(format!(
                    "./{}:{}/{}",
                    BRANCHES_COLLECTION,
                    branch.child_name(),
                    input.provider_a_moniker,
                )),
                EventMatcher::ok().r#type(Discovered::TYPE).moniker(format!(
                    "./{}:{}/{}",
                    BRANCHES_COLLECTION,
                    branch.child_name(),
                    input.provider_b_moniker,
                )),
            ],
            Ordering::Unordered,
        )
        .expect(event_stream)
        .await
        .context("event sequence did not match expected")?;

    Ok(branch)
}

/// Starts the provider with the name `child_name` in the branch component.
async fn start_provider(branch: &ScopedInstance, child_moniker: &str) -> Result<(), Error> {
    let lifecycle_controller_proxy =
        client::connect_to_protocol::<fsys2::LifecycleControllerMarker>()
            .context("failed to connect to LifecycleController")?;

    let event_stream = EventStream::open_at_path("/events/started")
        .await
        .context("failed to subscribe to EventSource")?;

    let provider_moniker =
        format!("./{}:{}/{}", BRANCHES_COLLECTION, branch.child_name(), child_moniker);

    let (_, binder_server) = fidl::endpoints::create_endpoints();

    // Start the provider child.
    lifecycle_controller_proxy
        .start_instance(&provider_moniker, binder_server)
        .await?
        .map_err(|err| format_err!("failed to start provider component: {:?}", err))?;

    // Wait for the provider to start.
    EventSequence::new()
        .has_subset(
            vec![EventMatcher::ok().r#type(Started::TYPE).moniker(provider_moniker)],
            Ordering::Unordered,
        )
        .expect(event_stream)
        .await
        .context("event sequence did not match expected")?;

    Ok(())
}

/// Destroys a BankAccount provider component with the name `child_name`.
async fn destroy_provider(branch: &ScopedInstance, child_moniker: &str) -> Result<(), Error> {
    info!(%child_moniker, "destroying BankAccount provider");

    let lifecycle_controller_proxy =
        client::connect_to_protocol::<fsys2::LifecycleControllerMarker>()
            .context("failed to connect to LifecycleController")?;

    let event_stream = EventStream::open_at_path("/events/destroyed")
        .await
        .context("failed to subscribe to EventSource")?;

    let provider_moniker =
        format!("./{}:{}/{}", BRANCHES_COLLECTION, branch.child_name(), child_moniker);
    let parent_moniker = format!("./{}:{}", BRANCHES_COLLECTION, branch.child_name());

    // Destroy the provider child.
    let child_moniker = ChildName::parse(child_moniker).unwrap();
    lifecycle_controller_proxy
        .destroy_instance(
            &parent_moniker,
            &fdecl::ChildRef {
                name: child_moniker.name().into(),
                collection: child_moniker.collection().map(|c| c.to_string()),
            },
        )
        .await?
        .map_err(|err| format_err!("failed to destroy provider component: {:?}", err))?;

    // Wait for the provider to be destroyed.
    EventSequence::new()
        .has_subset(
            vec![EventMatcher::ok().r#type(Destroyed::TYPE).moniker(provider_moniker)],
            Ordering::Unordered,
        )
        .expect(event_stream)
        .await
        .context("event sequence did not match expected")?;

    Ok(())
}

fn verify_instances(instances: Vec<String>, expected_len: usize) {
    assert_eq!(instances.len(), expected_len);
    assert!(instances.iter().all(|id| id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())));
}
