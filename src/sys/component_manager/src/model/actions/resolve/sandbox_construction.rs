// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::{ComponentInstance, ResolvedInstanceState, WeakComponentInstance},
            routing::router::{Request, Router},
        },
        sandbox_util::{DictExt, LaunchTaskOnReceive},
    },
    ::routing::{
        capability_source::{ComponentCapability, InternalCapability},
        component_instance::ComponentInstanceInterface,
        error::{ComponentInstanceError, RoutingError},
    },
    bedrock_error::BedrockError,
    cm_rust::{
        CapabilityDecl, ExposeDeclCommon, OfferDeclCommon, SourceName, SourcePath, UseDeclCommon,
    },
    cm_types::{IterablePath, Name, SeparatedPath},
    fidl_fuchsia_component_decl as fdecl,
    futures::FutureExt,
    itertools::Itertools,
    lazy_static::lazy_static,
    moniker::{ChildName, ChildNameBase, MonikerBase},
    sandbox::{Capability, Dict, Unit},
    std::{collections::HashMap, sync::Arc},
    tracing::warn,
    vfs::execution_scope::ExecutionScope,
};

// Dictionary keys for different kinds of sandboxes.
lazy_static! {
    /// Dictionary of capabilities from the parent.
    static ref PARENT: Name = "parent".parse().unwrap();

    /// Dictionary of capabilities from a component's environment.
    static ref ENVIRONMENT: Name = "environment".parse().unwrap();

    /// Dictionary of debug capabilities in a component's environment.
    static ref DEBUG: Name = "debug".parse().unwrap();
}

/// Contains the capabilities component receives from its parent and environment. Stored as a
/// [Dict] containing two nested [Dict]s for the parent and environment.
#[derive(Clone, Debug)]
pub struct ComponentInput(Dict);

impl Default for ComponentInput {
    fn default() -> Self {
        Self::new(ComponentEnvironment::new())
    }
}

impl ComponentInput {
    pub fn new(environment: ComponentEnvironment) -> Self {
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(PARENT.clone(), Dict::new().into());
        entries.insert(ENVIRONMENT.clone(), Dict::from(environment).into());
        drop(entries);
        Self(dict)
    }

    /// Creates a new ComponentInput with entries cloned from this ComponentInput.
    ///
    /// This is a shallow copy. Values are cloned, not copied, so are new references to the same
    /// underlying data.
    pub fn shallow_copy(&self) -> Self {
        // Note: We call [Dict::copy] on the nested [Dict]s, not the root [Dict], because
        // [Dict::copy] only goes one level deep and we want to copy the contents of the
        // inner sandboxes.
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(PARENT.clone(), self.capabilities().shallow_copy().into());
        entries.insert(ENVIRONMENT.clone(), Dict::from(self.environment()).shallow_copy().into());
        drop(entries);
        Self(dict)
    }

    /// Returns the sub-dictionary containing capabilities routed by the component's parent.
    pub fn capabilities(&self) -> Dict {
        let entries = self.0.lock_entries();
        let cap = entries.get(&*PARENT).unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("parent entry must be a dict: {cap:?}");
        };
        dict.clone()
    }

    /// Returns the sub-dictionary containing capabilities routed by the component's environment.
    pub fn environment(&self) -> ComponentEnvironment {
        let entries = self.0.lock_entries();
        let cap = entries.get(&*ENVIRONMENT).unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("environment entry must be a dict: {cap:?}");
        };
        ComponentEnvironment(dict.clone())
    }

    pub fn insert_capability(&self, path: &impl IterablePath, capability: Capability) {
        self.capabilities().insert_capability(path, capability.into())
    }
}

/// The capabilities a component has in its environment. Stored as a [Dict] containing a nested
/// [Dict] holding the environment's debug capabilities.
#[derive(Clone, Debug)]
pub struct ComponentEnvironment(Dict);

impl Default for ComponentEnvironment {
    fn default() -> Self {
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(DEBUG.clone(), Dict::new().into());
        drop(entries);
        Self(dict)
    }
}

impl ComponentEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capabilities listed in the `debug_capabilities` portion of its environment.
    pub fn debug(&self) -> Dict {
        let entries = self.0.lock_entries();
        let cap = entries.get(&*DEBUG).unwrap();
        let Capability::Dictionary(dict) = cap else {
            unreachable!("debug entry must be a dict: {cap:?}");
        };
        dict.clone()
    }

    pub fn shallow_copy(&self) -> Self {
        // Note: We call [Dict::copy] on the nested [Dict]s, not the root [Dict], because
        // [Dict::copy] only goes one level deep and we want to copy the contents of the
        // inner sandboxes.
        let dict = Dict::new();
        let mut entries = dict.lock_entries();
        entries.insert(DEBUG.clone(), self.debug().shallow_copy().into());
        drop(entries);
        Self(dict)
    }
}

impl From<ComponentEnvironment> for Dict {
    fn from(e: ComponentEnvironment) -> Self {
        e.0
    }
}

/// Once a component has been resolved and its manifest becomes known, this function produces the
/// various dicts the component needs based on the contents of its manifest.
pub fn build_component_sandbox(
    component: &Arc<ComponentInstance>,
    execution_scope: &ExecutionScope,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::ComponentDecl,
    component_input: &ComponentInput,
    component_output_dict: &Dict,
    program_input_dict: &Dict,
    program_output_dict: &Dict,
    child_inputs: &mut HashMap<Name, ComponentInput>,
    collection_inputs: &mut HashMap<Name, ComponentInput>,
    environments: &mut HashMap<Name, ComponentEnvironment>,
) {
    let declared_dictionaries = Dict::new();

    for environment_decl in &decl.environments {
        environments.insert(
            Name::new(&environment_decl.name).unwrap(),
            build_environment(
                component,
                children,
                component_input,
                program_output_dict,
                environment_decl,
            ),
        );
    }

    for child in &decl.children {
        let environment;
        if let Some(environment_name) = child.environment.as_ref() {
            environment = environments
                .get(&Name::new(environment_name).unwrap())
                .expect(
                    "child references nonexistent environment, \
                    this should be prevented in manifest validation",
                )
                .clone();
        } else {
            environment = component_input.environment();
        }
        let input = ComponentInput::new(environment);
        child_inputs.insert(Name::new(&child.name).unwrap(), input);
    }

    for collection in &decl.collections {
        let environment;
        if let Some(environment_name) = collection.environment.as_ref() {
            environment = environments
                .get(&Name::new(environment_name).unwrap())
                .expect(
                    "collection references nonexistent environment, \
                    this should be prevented in manifest validation",
                )
                .clone();
        } else {
            environment = component_input.environment();
        }
        let input = ComponentInput::new(environment);
        collection_inputs.insert(collection.name.clone(), input);
    }

    for capability in &decl.capabilities {
        extend_dict_with_capability(
            component,
            execution_scope,
            children,
            decl,
            capability,
            component_input,
            program_output_dict,
            &declared_dictionaries,
        );
    }

    for use_ in &decl.uses {
        extend_dict_with_use(
            component,
            children,
            component_input,
            program_input_dict,
            program_output_dict,
            use_,
        );
    }

    for offer in &decl.offers {
        // We only support protocol and dictionary capabilities right now
        if !is_supported_offer(offer) {
            continue;
        }
        let mut target_dict = match offer.target() {
            cm_rust::OfferTarget::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name = Name::new(&child_ref.name).unwrap();
                child_inputs.entry(child_name).or_insert(ComponentInput::default()).capabilities()
            }
            cm_rust::OfferTarget::Collection(name) => collection_inputs
                .entry(name.clone())
                .or_insert(ComponentInput::default())
                .capabilities(),
            cm_rust::OfferTarget::Capability(name) => {
                let mut entries = declared_dictionaries.lock_entries();
                let dict = entries
                    .entry(name.clone())
                    .or_insert_with(|| Capability::Dictionary(Dict::new()))
                    .clone();
                let Capability::Dictionary(dict) = dict else {
                    panic!("wrong type in dict");
                };
                dict
            }
        };
        extend_dict_with_offer(
            component,
            children,
            component_input,
            program_output_dict,
            offer,
            &mut target_dict,
        );
    }

    for expose in &decl.exposes {
        extend_dict_with_expose(
            component,
            children,
            program_output_dict,
            expose,
            component_output_dict,
        );
    }
}

/// Adds `capability` to the program output dict given the resolved `decl`. The program output dict
/// is a dict of routers, keyed by capability name.
fn extend_dict_with_capability(
    component: &Arc<ComponentInstance>,
    execution_scope: &ExecutionScope,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::ComponentDecl,
    capability: &cm_rust::CapabilityDecl,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
) {
    match capability {
        CapabilityDecl::Service(_)
        | CapabilityDecl::Protocol(_)
        | CapabilityDecl::Directory(_)
        | CapabilityDecl::Runner(_)
        | CapabilityDecl::Resolver(_) => {
            let path = capability.path().expect("must have path");
            let router = ResolvedInstanceState::make_program_outgoing_router(
                component,
                decl,
                capability,
                path,
                execution_scope,
            );
            let router = router.with_policy_check(
                CapabilitySource::Component {
                    capability: ComponentCapability::from(capability.clone()),
                    component: component.as_weak(),
                },
                component.policy_checker().clone(),
            );
            program_output_dict.insert_capability(capability.name(), router.into());
        }
        cm_rust::CapabilityDecl::Dictionary(d) => {
            extend_dict_with_dictionary(
                component,
                children,
                d,
                component_input,
                program_output_dict,
                declared_dictionaries,
            );
        }
        CapabilityDecl::EventStream(_) | CapabilityDecl::Config(_) | CapabilityDecl::Storage(_) => {
            // Capabilities not supported in bedrock program output dict yet.
            return;
        }
    }
}

fn extend_dict_with_dictionary(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    decl: &cm_rust::DictionaryDecl,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    declared_dictionaries: &Dict,
) {
    let dict = Dict::new();
    let router = if let Some(source) = decl.source.as_ref() {
        let source_path = decl
            .source_dictionary
            .as_ref()
            .expect("source_dictionary must be set if source is set");
        let source_dict_router = match &source {
            cm_rust::DictionarySource::Parent => {
                component_input.capabilities().get_router_or_error(
                    source_path,
                    RoutingError::use_from_parent_not_found(
                        &component.moniker,
                        source_path.iter_segments().join("/"),
                    ),
                )
            }
            cm_rust::DictionarySource::Self_ => program_output_dict.get_router_or_error(
                source_path,
                RoutingError::use_from_self_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                )
                .into(),
            ),
            cm_rust::DictionarySource::Child(child_ref) => {
                assert!(child_ref.collection.is_none(), "unexpected dynamic offer target");
                let child_name =
                    ChildName::parse(child_ref.name.as_str()).expect("invalid child name");
                match children.get(&child_name) {
                    Some(child) => {
                        let child = child.as_weak();
                        new_forwarding_router_to_child(
                            child,
                            source_path.clone(),
                            RoutingError::BedrockSourceDictionaryExposeNotFound,
                        )
                    }
                    None => Router::new_error(RoutingError::use_from_child_instance_not_found(
                        &child_name,
                        &component.moniker,
                        source_path.iter_segments().join("/"),
                    )),
                }
            }
        };
        make_dict_extending_router(component.as_weak(), dict.clone(), source_dict_router)
    } else {
        Router::new_ok(dict.clone())
    };
    declared_dictionaries.insert_capability(&decl.name, dict.into());
    program_output_dict.insert_capability(&decl.name, router.into());
}

fn build_environment(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    environment_decl: &cm_rust::EnvironmentDecl,
) -> ComponentEnvironment {
    let mut environment = ComponentEnvironment::new();
    if environment_decl.extends == fdecl::EnvironmentExtends::Realm {
        environment = component_input.environment().shallow_copy();
    }
    for debug_registration in &environment_decl.debug_capabilities {
        let cm_rust::DebugRegistration::Protocol(debug_protocol) = debug_registration;
        let source_path = SeparatedPath {
            dirname: Default::default(),
            basename: debug_protocol.source_name.clone(),
        };
        let router = match &debug_protocol.source {
            cm_rust::RegistrationSource::Parent => {
                use_from_parent_router(component_input, source_path, component.as_weak())
            }
            cm_rust::RegistrationSource::Self_ => program_output_dict.get_router_or_error(
                &source_path,
                RoutingError::use_from_self_not_found(
                    &component.moniker,
                    source_path.iter_segments().join("/"),
                )
                .into(),
            ),
            cm_rust::RegistrationSource::Child(child_name) => {
                let child_name = ChildName::parse(child_name).expect("invalid child name");
                let Some(child) = children.get(&child_name) else { continue };
                let weak_child = WeakComponentInstance::new(child);
                new_forwarding_router_to_child(
                    weak_child,
                    source_path,
                    RoutingError::use_from_child_expose_not_found(
                        child.moniker.leaf().unwrap(),
                        &child.moniker.parent().unwrap(),
                        debug_protocol.source_name.clone(),
                    ),
                )
            }
        };
        environment.debug().insert_capability(&debug_protocol.target_name, router.into());
    }
    environment
}

/// Returns a [Router] that returns a [Dict].
/// The first time this router is called, it calls `source_dict_router` to get another [Dict],
/// and combines those entries with `dict``.
/// Each time after, this router returns `dict`` that has the combined entries.
/// NOTE: This function modifies `dict`!
fn make_dict_extending_router(
    component: WeakComponentInstance,
    dict: Dict,
    source_dict_router: Router,
) -> Router {
    let did_combine = Arc::new(std::sync::Mutex::new(false));
    let route_fn = move |_request: Request| {
        let source_dict_router = source_dict_router.clone();
        let dict = dict.clone();
        let did_combine = did_combine.clone();
        let component = component.clone();
        async move {
            // If we've already combined then return our dict.
            if *did_combine.lock().unwrap() {
                return Ok(dict.into());
            }

            // Otherwise combine the two.
            let source_request =
                Request { availability: cm_types::Availability::Required, target: component };
            let source_dict = match source_dict_router.route(source_request).await? {
                Capability::Dictionary(d) => d,
                _ => panic!("source_dict_router must return a Dict"),
            };
            {
                let mut entries = dict.lock_entries();
                let source_entries = source_dict.lock_entries();
                for source_key in source_entries.keys() {
                    if entries.contains_key(source_key.as_str()) {
                        return Err(RoutingError::BedrockSourceDictionaryCollision.into());
                    }
                }
                for (source_key, source_value) in &*source_entries {
                    entries.insert(source_key.clone(), source_value.clone());
                }
            }
            *did_combine.lock().unwrap() = true;
            Ok(dict.into())
        }
        .boxed()
    };
    Router::new(route_fn)
}

/// Extends the given dict based on offer declarations. All offer declarations in `offers` are
/// assumed to target `target_dict`.
pub fn extend_dict_with_offers(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    dynamic_offers: &Vec<cm_rust::OfferDecl>,
    target_input: &mut ComponentInput,
) {
    for offer in dynamic_offers {
        extend_dict_with_offer(
            component,
            children,
            component_input,
            program_output_dict,
            offer,
            &mut target_input.capabilities(),
        );
    }
}

fn supported_use(use_: &cm_rust::UseDecl) -> Option<&cm_rust::UseProtocolDecl> {
    match use_ {
        cm_rust::UseDecl::Protocol(p) => Some(p),
        _ => None,
    }
}

fn extend_dict_with_use(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    program_input_dict: &Dict,
    program_output_dict: &Dict,
    use_: &cm_rust::UseDecl,
) {
    let Some(use_protocol) = supported_use(use_) else {
        return;
    };

    let source_path = use_.source_path();
    let router = match use_.source() {
        cm_rust::UseSource::Parent => {
            use_from_parent_router(component_input, source_path.to_owned(), component.as_weak())
        }
        cm_rust::UseSource::Self_ => program_output_dict.get_router_or_error(
            &source_path,
            RoutingError::use_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::UseSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid child name");
            let Some(child) = children.get(&child_name) else {
                panic!("use declaration in manifest for component {} has a source of a nonexistent child {}, this should be prevented by manifest validation", component.moniker, child_name);
            };
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                weak_child,
                source_path.to_owned(),
                RoutingError::use_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    use_.source_name().clone(),
                ),
            )
        }
        cm_rust::UseSource::Framework if use_.is_from_dictionary() => {
            Router::new_error(RoutingError::capability_from_framework_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ))
        }
        cm_rust::UseSource::Framework => {
            let source_name = use_.source_name().clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such framework capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::UseSource::Capability(_) => {
            let use_ = use_.clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Capability {
                    source_capability: ComponentCapability::Use(use_.clone()),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::UseSource::Debug => component_input.environment().debug().get_router_or_error(
            &use_protocol.source_name,
            RoutingError::use_from_environment_not_found(
                &component.moniker,
                "protocol",
                &use_protocol.source_name,
            ),
        ),
        // UseSource::Environment is not used for protocol capabilities
        cm_rust::UseSource::Environment => return,
    };
    program_input_dict.insert_capability(
        &use_protocol.target_path,
        router.with_availability(*use_.availability()).into(),
    );
}

/// Builds a router that obtains a capability that the program uses from `parent`.
///
/// The capability is usually an entry in the `component_input.capabilities` dict unless it is
/// overridden by an eponymous capability in the `program_input_dict_additions` when started.
fn use_from_parent_router(
    component_input: &ComponentInput,
    source_path: impl IterablePath + 'static,
    weak_component: WeakComponentInstance,
) -> Router {
    let component_input_capability = component_input.capabilities().get_router_or_error(
        &source_path,
        RoutingError::use_from_parent_not_found(
            &weak_component.moniker,
            source_path.iter_segments().join("/"),
        ),
    );

    Router::new(move |request| {
        let source_path = source_path.clone();
        let component_input_capability = component_input_capability.clone();
        let Ok(component) = weak_component.upgrade() else {
            return std::future::ready(Err(RoutingError::from(
                ComponentInstanceError::InstanceNotFound {
                    moniker: weak_component.moniker.clone(),
                },
            )
            .into()))
            .boxed();
        };
        async move {
            let router = {
                let state = match component.lock_resolved_state().await {
                    Ok(state) => state,
                    Err(err) => {
                        return Err(RoutingError::from(ComponentInstanceError::resolve_failed(
                            component.moniker.clone(),
                            err,
                        ))
                        .into());
                    }
                };
                // Try to get the capability from the incoming dict, which was passed when the child was
                // started.
                //
                // Unlike the program input dict below that contains Routers created by
                // component manager, the incoming dict may contain capabilities created externally.
                // Currently there is no way to create a Router externally, so assume these
                // are Open capabilities and convert them to Router here.
                //
                // TODO(https://fxbug.dev/319542502): Convert from the external Router type, once it
                // exists.
                state
                    .program_input_dict_additions
                    .as_ref()
                    .and_then(|dict| match dict.get_capability(&source_path) {
                        Some(Capability::Open(o)) => Some(Router::new_ok(o)),
                        _ => None,
                    })
                    // Try to get the capability from the component input dict, created from static
                    // routes when the component was resolved.
                    .unwrap_or(component_input_capability)
            };
            router.route(request).await
        }
        .boxed()
    })
}

fn is_supported_offer(offer: &cm_rust::OfferDecl) -> bool {
    matches!(offer, cm_rust::OfferDecl::Protocol(_) | cm_rust::OfferDecl::Dictionary(_))
}

fn extend_dict_with_offer(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    component_input: &ComponentInput,
    program_output_dict: &Dict,
    offer: &cm_rust::OfferDecl,
    target_dict: &mut Dict,
) {
    // We only support protocol and dictionary capabilities right now
    if !is_supported_offer(offer) {
        return;
    }
    let source_path = offer.source_path();
    let target_name = offer.target_name();
    if target_dict.get_capability(&source_path).is_some() {
        warn!(
            "duplicate sources for protocol {} in a dict, unable to populate dict entry",
            target_name
        );
        target_dict.remove_capability(target_name);
        return;
    }
    let router = match offer.source() {
        cm_rust::OfferSource::Parent => component_input.capabilities().get_router_or_error(
            &source_path,
            RoutingError::offer_from_parent_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::OfferSource::Self_ => program_output_dict.get_router_or_error(
            &source_path,
            RoutingError::offer_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::OfferSource::Child(child_ref) => {
            let child_name: ChildName = child_ref.clone().try_into().expect("invalid child ref");
            let Some(child) = children.get(&child_name) else { return };
            let weak_child = WeakComponentInstance::new(child);
            new_forwarding_router_to_child(
                weak_child,
                source_path.to_owned(),
                RoutingError::offer_from_child_expose_not_found(
                    child.moniker.leaf().unwrap(),
                    &child.moniker.parent().unwrap(),
                    offer.source_name().clone(),
                ),
            )
        }
        cm_rust::OfferSource::Framework => {
            if offer.is_from_dictionary() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let source_name = offer.source_name().clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such framework capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::OfferSource::Capability(_) => {
            let offer = offer.clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Capability {
                    source_capability: ComponentCapability::Offer(offer.clone()),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::OfferSource::Void => new_unit_router(),
        // This is only relevant for services, so this arm is never reached.
        cm_rust::OfferSource::Collection(_name) => return,
    };
    target_dict
        .insert_capability(target_name, router.with_availability(*offer.availability()).into());
}

pub fn is_supported_expose(expose: &cm_rust::ExposeDecl) -> bool {
    matches!(expose, cm_rust::ExposeDecl::Protocol(_) | cm_rust::ExposeDecl::Dictionary(_))
}

fn extend_dict_with_expose(
    component: &Arc<ComponentInstance>,
    children: &HashMap<ChildName, Arc<ComponentInstance>>,
    program_output_dict: &Dict,
    expose: &cm_rust::ExposeDecl,
    target_dict: &Dict,
) {
    if !is_supported_expose(expose) {
        return;
    }
    // We only support exposing to the parent right now
    if expose.target() != &cm_rust::ExposeTarget::Parent {
        return;
    }
    let source_path = expose.source_path();
    let target_name = expose.target_name();

    let router = match expose.source() {
        cm_rust::ExposeSource::Self_ => program_output_dict.get_router_or_error(
            &source_path,
            RoutingError::expose_from_self_not_found(
                &component.moniker,
                source_path.iter_segments().join("/"),
            ),
        ),
        cm_rust::ExposeSource::Child(child_name) => {
            let child_name = ChildName::parse(child_name).expect("invalid static child name");
            if let Some(child) = children.get(&child_name) {
                let weak_child = WeakComponentInstance::new(child);
                new_forwarding_router_to_child(
                    weak_child,
                    source_path.to_owned(),
                    RoutingError::expose_from_child_expose_not_found(
                        child.moniker.leaf().unwrap(),
                        &child.moniker.parent().unwrap(),
                        expose.source_name().clone(),
                    ),
                )
            } else {
                return;
            }
        }
        cm_rust::ExposeSource::Framework => {
            if expose.is_from_dictionary() {
                warn!(
                    "routing from framework with dictionary path is not supported: {source_path}"
                );
                return;
            }
            let source_name = expose.source_name().clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such framework capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Framework {
                    capability: InternalCapability::Protocol(source_name),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::ExposeSource::Capability(_) => {
            let expose = expose.clone();
            // TODO(https://fxbug.dev/323926925): place a router here that will return an error if
            // there's no such capability.
            LaunchTaskOnReceive::new_hook_launch_task(
                component,
                CapabilitySource::Capability {
                    source_capability: ComponentCapability::Expose(expose.clone()),
                    component: component.into(),
                },
            )
            .into_router()
        }
        cm_rust::ExposeSource::Void => new_unit_router(),
        // This is only relevant for services, so this arm is never reached.
        cm_rust::ExposeSource::Collection(_name) => return,
    };
    target_dict
        .insert_capability(target_name, router.with_availability(*expose.availability()).into());
}

fn new_unit_router() -> Router {
    Router::new_ok(Unit {})
}

fn new_forwarding_router_to_child(
    weak_child: WeakComponentInstance,
    capability_path: impl IterablePath + 'static,
    expose_not_found_error: RoutingError,
) -> Router {
    Router::new(move |request: Request| {
        forward_request_to_child(
            weak_child.clone(),
            capability_path.clone(),
            expose_not_found_error.clone(),
            request,
        )
        .boxed()
    })
}

async fn forward_request_to_child(
    weak_child: WeakComponentInstance,
    capability_path: impl IterablePath + 'static,
    expose_not_found_error: RoutingError,
    request: Request,
) -> Result<Capability, BedrockError> {
    let router = {
        let child = weak_child.upgrade().map_err(|e| RoutingError::ComponentInstanceError(e))?;
        let child_state = child.lock_resolved_state().await.map_err(|e| {
            RoutingError::ComponentInstanceError(ComponentInstanceError::resolve_failed(
                child.moniker.clone(),
                e,
            ))
        })?;
        child_state
            .component_output_dict
            .get_router_or_error(&capability_path, expose_not_found_error.clone())
    };
    router.route(request).await
}
