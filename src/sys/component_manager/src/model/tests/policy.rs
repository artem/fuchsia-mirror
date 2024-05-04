// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::{
            manager::ComponentManagerInstance, ComponentInstance, WeakComponentInstance,
            WeakExtendedInstance,
        },
        context::ModelContext,
        environment::Environment,
        resolver::ResolverRegistry,
    },
    anyhow::Error,
    async_trait::async_trait,
    cm_moniker::InstancedMoniker,
    fidl_fuchsia_component_decl as fdecl,
    hooks::Hooks,
    routing::environment::{DebugRegistry, RunnerRegistry},
    routing_test_helpers::{
        instantiate_global_policy_checker_tests, policy::GlobalPolicyCheckerTest,
    },
    std::sync::Arc,
};

// Tests `GlobalPolicyChecker` methods for `ComponentInstance`s.
#[derive(Default)]
struct GlobalPolicyCheckerTestForCm {}

#[async_trait]
impl GlobalPolicyCheckerTest<ComponentInstance> for GlobalPolicyCheckerTestForCm {
    async fn make_component(&self, instanced_moniker: InstancedMoniker) -> Arc<ComponentInstance> {
        let top_instance = Arc::new(ComponentManagerInstance::new(vec![], vec![]));
        ComponentInstance::new(
            Arc::new(Environment::new_root(
                &top_instance,
                RunnerRegistry::default(),
                ResolverRegistry::new(),
                DebugRegistry::default(),
            )),
            instanced_moniker,
            "test:///bar".parse().unwrap(),
            fdecl::StartupMode::Lazy,
            fdecl::OnTerminate::None,
            None,
            Arc::new(ModelContext::new_for_test()),
            WeakExtendedInstance::Component(WeakComponentInstance::default()),
            Arc::new(Hooks::new()),
            false,
        )
        .await
    }
}

instantiate_global_policy_checker_tests!(GlobalPolicyCheckerTestForCm);
