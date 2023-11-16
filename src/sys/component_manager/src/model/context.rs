// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{
            BuiltinCapability, CapabilityProvider, CapabilitySource, DerivedCapability,
            FrameworkCapability,
        },
        model::component::WeakComponentInstance,
        model::error::ModelError,
    },
    ::routing::policy::GlobalPolicyChecker,
    cm_config::{AbiRevisionPolicy, RuntimeConfig},
    futures::lock::Mutex,
    std::sync::Arc,
};

/// The ModelContext provides the API boundary between the Model and Realms. It
/// defines what parts of the Model or authoritative state about the tree we
/// want to share with Realms.
pub struct ModelContext {
    component_id_index: component_id_index::Index,
    policy_checker: GlobalPolicyChecker,
    runtime_config: Arc<RuntimeConfig>,
    builtin_capabilities: Mutex<Option<Vec<Box<dyn BuiltinCapability>>>>,
    framework_capabilities: Mutex<Option<Vec<Box<dyn FrameworkCapability>>>>,
    derived_capabilities: Mutex<Option<Vec<Box<dyn DerivedCapability>>>>,
}

impl ModelContext {
    /// Constructs a new ModelContext from a RuntimeConfig.
    pub fn new(runtime_config: Arc<RuntimeConfig>) -> Result<Self, ModelError> {
        Ok(Self {
            component_id_index: match &runtime_config.component_id_index_path {
                Some(path) => component_id_index::Index::from_fidl_file(&path)?,
                None => component_id_index::Index::default(),
            },
            policy_checker: GlobalPolicyChecker::new(runtime_config.security_policy.clone()),
            runtime_config,
            builtin_capabilities: Mutex::new(None),
            framework_capabilities: Mutex::new(None),
            derived_capabilities: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let runtime_config = Arc::new(RuntimeConfig::default());
        Self::new(runtime_config).unwrap()
    }

    /// Returns the runtime policy checker for the model.
    pub fn policy(&self) -> &GlobalPolicyChecker {
        &self.policy_checker
    }

    pub fn runtime_config(&self) -> &Arc<RuntimeConfig> {
        &self.runtime_config
    }

    pub fn component_id_index(&self) -> &component_id_index::Index {
        &self.component_id_index
    }

    pub fn abi_revision_policy(&self) -> &AbiRevisionPolicy {
        &self.runtime_config.abi_revision_policy
    }

    pub async fn init_internal_capabilities(
        &self,
        b: Vec<Box<dyn BuiltinCapability>>,
        f: Vec<Box<dyn FrameworkCapability>>,
        d: Vec<Box<dyn DerivedCapability>>,
    ) {
        {
            let mut builtin_capabilities = self.builtin_capabilities.lock().await;
            assert!(builtin_capabilities.is_none(), "already initialized");
            *builtin_capabilities = Some(b);
        }
        {
            let mut framework_capabilities = self.framework_capabilities.lock().await;
            assert!(framework_capabilities.is_none(), "already initialized");
            *framework_capabilities = Some(f);
        }
        {
            let mut derived_capabilities = self.derived_capabilities.lock().await;
            assert!(derived_capabilities.is_none(), "already initialized");
            *derived_capabilities = Some(d);
        }
    }

    #[cfg(test)]
    pub async fn add_framework_capability(&self, c: Box<dyn FrameworkCapability>) {
        // Internal capabilities added for a test should preempt existing ones that match the
        // same metadata.
        let mut framework_capabilities = self.framework_capabilities.lock().await;
        framework_capabilities.as_mut().unwrap().insert(0, c);
    }

    pub async fn find_internal_provider(
        &self,
        source: &CapabilitySource,
        target: WeakComponentInstance,
    ) -> Option<Box<dyn CapabilityProvider>> {
        match source {
            CapabilitySource::Builtin { capability, top_instance: _ } => {
                let builtin_capabilities = self.builtin_capabilities.lock().await;
                for c in builtin_capabilities.as_ref().expect("not initialized") {
                    if c.matches(capability) {
                        return Some(c.new_provider(target));
                    }
                }
                None
            }
            CapabilitySource::Framework { capability, component } => {
                let framework_capabilities = self.framework_capabilities.lock().await;
                for c in framework_capabilities.as_ref().expect("not initialized") {
                    if c.matches(capability) {
                        return Some(c.new_provider(component.clone(), target));
                    }
                }
                None
            }
            CapabilitySource::Capability { source_capability, component } => {
                let derived_capabilities = self.derived_capabilities.lock().await;
                for c in derived_capabilities.as_ref().expect("not initialized") {
                    if let Some(provider) =
                        c.maybe_new_provider(source_capability, component.clone()).await
                    {
                        return Some(provider);
                    }
                }
                None
            }
            _ => None,
        }
    }
}
