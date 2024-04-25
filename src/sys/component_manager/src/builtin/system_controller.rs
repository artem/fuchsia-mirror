// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{actions::ShutdownType, component::ComponentInstance, model::Model},
    anyhow::{format_err, Context as _, Error},
    fidl_fuchsia_sys2::*,
    fuchsia_async::{self as fasync},
    fuchsia_zircon as zx,
    futures::prelude::*,
    std::{
        collections::VecDeque,
        sync::{Arc, Weak},
        time::Duration,
    },
    tracing::*,
};

const SHUTDOWN_WATCHDOG_INTERVAL: zx::Duration = zx::Duration::from_seconds(15);

pub struct SystemController {
    model: Weak<Model>,
    request_timeout: Duration,
}

impl SystemController {
    // TODO(jmatt) allow timeout to be supplied in the constructor
    pub fn new(model: Weak<Model>, request_timeout: Duration) -> Self {
        Self { model, request_timeout }
    }

    pub async fn serve(self, mut stream: SystemControllerRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            // TODO(jmatt) There is the potential for a race here. If
            // the thing that called SystemController.Shutdown is a
            // component that component_manager controls, it should
            // be gone by now. Sending a response doesn't make a lot
            // of sense in this case. However, the caller might live
            // outside component_manager, in which case a response
            // does make sense. Figure out if our behavior should be
            // different and/or whether we should drop the response
            // from this API.
            match request {
                // Shutting down the root component causes component_manager to
                // exit. main.rs waits on the model to observe the root disappear.
                SystemControllerRequest::Shutdown { responder } => {
                    let timeout = zx::Duration::from(self.request_timeout);
                    fasync::Task::spawn(async move {
                        fasync::Timer::new(fasync::Time::after(timeout)).await;
                        panic!("Component manager did not complete shutdown in allowed time.");
                    })
                    .detach();
                    info!("Component manager is shutting down the system");
                    let root =
                        self.model.upgrade().ok_or(format_err!("model is dropped"))?.root().clone();

                    // Kick off a background task to log when shutdown is taking too long.
                    fuchsia_async::Task::spawn(shutdown_watchdog(root.clone())).detach();

                    root.shutdown(ShutdownType::System)
                        .await
                        .context("got error waiting for shutdown action to complete")?;
                    match responder.send() {
                        Ok(()) => {}
                        Err(e) => {
                            warn!(%e, "Error sending response to shutdown requester. Shut down proceeding");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

async fn shutdown_watchdog(root: Arc<ComponentInstance>) {
    let mut interval = fuchsia_async::Interval::new(SHUTDOWN_WATCHDOG_INTERVAL);
    while let Some(_) = interval.next().await {
        info!(
            "Shutdown not yet complete, pending components:\n{}",
            get_all_remaining_monikers(&root).await.join("\n\t")
        );
    }
}

async fn get_all_remaining_monikers(root: &Arc<ComponentInstance>) -> Vec<String> {
    let mut monikers = vec![];
    let mut queue = VecDeque::new();
    queue.push_back(root.clone());

    while let Some(next) = queue.pop_front() {
        let state = next.lock_state().await;
        if let Some(resolved_state) = state.get_resolved_state() {
            queue.extend(resolved_state.children().map(|(_, i)| i.clone()));
        }
        if state.is_started() {
            monikers.push(next.moniker.to_string());
        }
    }

    monikers
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            hooks::{Event, EventType, Hook, HooksRegistration},
            testing::test_helpers::{component_decl_with_test_runner, ActionsTest, ComponentInfo},
        },
        async_trait::async_trait,
        cm_rust_testing::*,
        errors::ModelError,
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
        moniker::{Moniker, MonikerBase},
    };

    /// Use SystemController to shut down a system whose root has the child `a`
    /// and `a` has descendents as shown in the diagram below.
    ///  a
    ///   \
    ///    b
    ///   / \
    ///  c   d
    #[fuchsia::test]
    async fn test_system_controller() {
        // Configure and start component
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager().build())
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager().build())
                    .child(ChildBuilder::new().name("d").eager().build())
                    .build(),
            ),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;

        // Start each component.
        test.start(Moniker::root()).await;
        let component_a = test.start(vec!["a"].try_into().unwrap()).await;
        let component_b = test.start(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.start(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.start(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Wire up connections to SystemController
        let sys_controller = SystemController::new(
            Arc::downgrade(&test.model),
            // allow simulated shutdown to take up to 30 days
            Duration::from_secs(60 * 60 * 24 * 30),
        );
        let (controller_proxy, stream) =
            create_proxy_and_stream::<fsys::SystemControllerMarker>().unwrap();
        let _task = fasync::Task::spawn(async move {
            sys_controller.serve(stream).await.expect("error serving system controller");
        });

        let root_component_info = ComponentInfo::new(test.model.root().clone()).await;
        let component_a_info = ComponentInfo::new(component_a.clone()).await;
        let component_b_info = ComponentInfo::new(component_b.clone()).await;
        let component_c_info = ComponentInfo::new(component_c.clone()).await;
        let component_d_info = ComponentInfo::new(component_d.clone()).await;

        // Check that the root component is still here
        root_component_info.check_not_shut_down(&test.runner).await;
        component_a_info.check_not_shut_down(&test.runner).await;
        component_b_info.check_not_shut_down(&test.runner).await;
        component_c_info.check_not_shut_down(&test.runner).await;
        component_d_info.check_not_shut_down(&test.runner).await;

        // Ask the SystemController to shut down the system and wait to be
        // notified that the root component stopped.
        let builtin_environment = test.builtin_environment.lock().await;
        let completion = builtin_environment.wait_for_root_stop();
        controller_proxy.shutdown().await.expect("shutdown request failed");
        completion.await;
        drop(builtin_environment);

        // Check state bits to confirm root component looks shut down
        root_component_info.check_is_shut_down(&test.runner).await;
        component_a_info.check_is_shut_down(&test.runner).await;
        component_b_info.check_is_shut_down(&test.runner).await;
        component_c_info.check_is_shut_down(&test.runner).await;
        component_d_info.check_is_shut_down(&test.runner).await;
    }

    #[fuchsia::test]
    #[should_panic(expected = "Component manager did not complete shutdown in allowed time.")]
    fn test_timeout() {
        const TIMEOUT_SECONDS: i64 = 6;
        const EVENT_PAUSE_SECONDS: i64 = TIMEOUT_SECONDS + 1;
        struct StopHook;
        #[async_trait]
        impl Hook for StopHook {
            async fn on(self: Arc<Self>, _event: &Event) -> Result<(), ModelError> {
                fasync::Timer::new(fasync::Time::after(zx::Duration::from_seconds(
                    EVENT_PAUSE_SECONDS.into(),
                )))
                .await;
                Ok(())
            }
        }

        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let mut test_logic = Box::pin(async {
            // Configure and start component
            let components = vec![
                (
                    "root",
                    ComponentDeclBuilder::new()
                        .child(ChildBuilder::new().name("a").eager().build())
                        .build(),
                ),
                ("a", ComponentDeclBuilder::new().build()),
            ];

            let s = StopHook {};
            let s_hook: Arc<dyn Hook> = Arc::new(s);
            let hooks_reg = HooksRegistration::new(
                "stop hook",
                vec![EventType::Stopped],
                Arc::downgrade(&s_hook),
            );

            let test = ActionsTest::new_with_hooks("root", components, None, vec![hooks_reg]).await;

            // Start root and `a`.
            test.start(Moniker::root()).await;
            let component_a = test.start(vec!["a"].try_into().unwrap()).await;

            // Wire up connections to SystemController
            let sys_controller = SystemController::new(
                Arc::downgrade(&test.model),
                // require shutdown in a second
                Duration::from_secs(u64::try_from(TIMEOUT_SECONDS).unwrap()),
            );
            let (controller_proxy, stream) =
                create_proxy_and_stream::<fsys::SystemControllerMarker>().unwrap();
            let _task = fasync::Task::spawn(async move {
                sys_controller.serve(stream).await.expect("error serving system controller");
            });

            let root_component_info = ComponentInfo::new(test.model.root().clone()).await;
            let component_a_info = ComponentInfo::new(component_a.clone()).await;

            // Check that the root component is still here
            root_component_info.check_not_shut_down(&test.runner).await;
            component_a_info.check_not_shut_down(&test.runner).await;

            // Ask the SystemController to shut down the system and wait to be
            // notified that the root component stopped.
            let builtin_environment = test.builtin_environment.lock().await;
            let _completion = builtin_environment.wait_for_root_stop();
            controller_proxy.shutdown().await.expect("shutdown request failed");
        });

        assert_eq!(std::task::Poll::Pending, exec.run_until_stalled(&mut test_logic));

        let new_time = fasync::Time::from_nanos(
            exec.now().into_nanos() + zx::Duration::from_seconds(TIMEOUT_SECONDS).into_nanos(),
        );

        exec.set_fake_time(new_time);
        exec.wake_expired_timers();

        assert_eq!(std::task::Poll::Pending, exec.run_until_stalled(&mut test_logic));
    }
}
