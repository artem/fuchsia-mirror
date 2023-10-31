// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Each routing method's name begins with `route_*`, and is an async function that returns
//! [Result<CapabilitySource<C>, RoutingError>], i.e. finds the capability source by walking the
//! route declarations and resolving components if necessary. Routing always walks in the direction
//! from the consuming side to the providing side.
//!
//! The most commonly used implementation is [route_from_use], which starts from a `Use`
//! declaration, walks along the chain of `Offer`s, and then the chain of `Expose`s. Similarly,
//! [route_from_registration] starts from a registration of a capability into an environment, then
//! walks the `Offer`s and `Expose`s.
//!
//! You can also start walking from halfway in this chain, e.g. [route_from_offer],
//! [route_from_expose].
//!
//! Some capability types cannot be exposed. In that case, use a routing function suffixed with
//! `_without_expose`.

use {
    crate::{
        capability_source::{
            AggregateCapability, CapabilitySource, ComponentCapability, InternalCapability,
        },
        collection::{
            AnonymizedAggregateServiceProvider, OfferAggregateServiceProvider,
            OfferFilteredServiceProvider,
        },
        component_instance::{
            ComponentInstanceInterface, ExtendedInstanceInterface, ResolvedInstanceInterface,
            TopInstanceInterface,
        },
        error::RoutingError,
        mapper::DebugRouteMapper,
        RegistrationDecl,
    },
    cm_rust::{
        Availability, CapabilityDecl, CapabilityTypeName, ExposeDecl, ExposeDeclCommon,
        ExposeSource, ExposeTarget, OfferDecl, OfferDeclCommon, OfferServiceDecl, OfferSource,
        OfferTarget, RegistrationDeclCommon, RegistrationSource, SourceName, UseDecl,
        UseDeclCommon, UseSource,
    },
    cm_types::Name,
    derivative::Derivative,
    moniker::{ChildName, ChildNameBase, Moniker},
    std::collections::HashSet,
    std::{fmt, slice},
    std::{marker::PhantomData, sync::Arc},
};

/// Routes a capability from its `Use` declaration to its source by following `Offer` and `Expose`
/// declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`AllowedSourcesBuilder`].
/// `visitor` is invoked for each `Offer` and `Expose` declaration in the routing path, as well as
/// the final `Capability` declaration if `sources` permits.
pub async fn route_from_use<U, C, S, V, M>(
    use_decl: U,
    use_target: Arc<C>,
    sources: S,
    visitor: &mut V,
    mapper: &mut M,
) -> Result<CapabilitySource<C>, RoutingError>
where
    U: UseDeclCommon
        + ErrorNotFoundFromParent
        + ErrorNotFoundInChild
        + Into<UseDecl>
        + Clone
        + 'static,
    C: ComponentInstanceInterface + 'static,
    S: Sources + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
    M: DebugRouteMapper + 'static,
{
    match Use::route(use_decl, use_target.clone(), &sources, visitor, mapper).await? {
        UseResult::Source(source) => return Ok(source),
        UseResult::OfferFromParent(offer, component) => {
            route_from_offer(offer, component, sources, visitor, mapper).await
        }
        UseResult::ExposeFromChild(use_decl, child_component) => {
            let child_exposes = child_component.lock_resolved_state().await?.exposes();
            let use_decl_full: UseDecl = use_decl.clone().into();
            let cap_type = CapabilityTypeName::from(&use_decl_full);
            let child_exposes =
                find_matching_exposes(cap_type, use_decl.source_name(), &child_exposes)
                    .ok_or_else(|| {
                        let child_moniker =
                            child_component.child_moniker().expect("ChildName should exist");
                        <U as ErrorNotFoundInChild>::error_not_found_in_child(
                            use_target.moniker().clone(),
                            child_moniker.clone(),
                            use_decl.source_name().clone(),
                        )
                    })?;
            route_from_expose(child_exposes, child_component, sources, visitor, mapper).await
        }
    }
}

/// Routes a capability from its environment `Registration` declaration to its source by following
/// `Offer` and `Expose` declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`AllowedSourcesBuilder`].
/// `visitor` is invoked for each `Offer` and `Expose` declaration in the routing path, as well as
/// the final `Capability` declaration if `sources` permits.
pub async fn route_from_registration<R, C, S, V, M>(
    registration_decl: R,
    registration_target: Arc<C>,
    sources: S,
    visitor: &mut V,
    mapper: &mut M,
) -> Result<CapabilitySource<C>, RoutingError>
where
    R: RegistrationDeclCommon
        + ErrorNotFoundFromParent
        + ErrorNotFoundInChild
        + Into<RegistrationDecl>
        + Clone
        + 'static,
    C: ComponentInstanceInterface + 'static,
    S: Sources + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
    M: DebugRouteMapper + 'static,
{
    match Registration::route(registration_decl, registration_target, &sources, visitor, mapper)
        .await?
    {
        RegistrationResult::Source(source) => return Ok(source),
        RegistrationResult::FromParent(offer, component) => {
            route_from_offer(offer, component, sources, visitor, mapper).await
        }
        RegistrationResult::FromChild(expose, component) => {
            route_from_expose(expose, component, sources, visitor, mapper).await
        }
    }
}

/// Routes a capability from its `Offer` declaration to its source by following `Offer` and `Expose`
/// declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`AllowedSourcesBuilder`].
/// `visitor` is invoked for each `Offer` and `Expose` declaration in the routing path, as well as
/// the final `Capability` declaration if `sources` permits.
pub async fn route_from_offer<C, S, V, M>(
    offer: RouteBundle<OfferDecl>,
    offer_target: Arc<C>,
    sources: S,
    visitor: &mut V,
    mapper: &mut M,
) -> Result<CapabilitySource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    S: Sources + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
    M: DebugRouteMapper + 'static,
{
    match Offer::route(offer, offer_target, &sources, visitor, mapper).await? {
        OfferResult::Source(source) => return Ok(source),
        OfferResult::OfferFromChild(offer, component) => {
            let offer_decl: OfferDecl = offer.clone().into();

            let (exposes, component) = change_directions::<C>(offer, component).await?;

            let capability_source =
                route_from_expose(exposes, component, sources, visitor, mapper).await?;
            if let OfferDecl::Service(offer_service_decl) = offer_decl {
                if offer_service_decl.source_instance_filter.is_some()
                    || offer_service_decl.renamed_instances.is_some()
                {
                    // TODO(https://fxbug.dev/97147) support collection sources as well.
                    if let CapabilitySource::Component { capability, component } = capability_source
                    {
                        let source_name = offer_service_decl.source_name.clone();
                        let capability_provider = Box::new(OfferFilteredServiceProvider::new(
                            offer_service_decl,
                            component.clone(),
                            capability,
                        ));
                        return Ok(CapabilitySource::<C>::FilteredAggregate {
                            capability: AggregateCapability::Service(source_name),
                            component,
                            capability_provider,
                        });
                    }
                }
            }
            Ok(capability_source)
        }
        OfferResult::OfferFromAnonymizedAggregate(offers, aggregation_component) => {
            let mut collections = vec![];
            let mut children = vec![];
            for o in offers.iter() {
                match o.source() {
                    OfferSource::Collection(n) => {
                        collections.push(n.clone());
                    }
                    OfferSource::Child(c) => {
                        assert!(
                            c.collection.is_none(),
                            "Anonymized offer source contained a dynamic child"
                        );
                        children.push(
                            ChildName::try_new(c.name.clone(), None)
                                .expect("child source should be convertible to ChildName"),
                        );
                    }
                    _ => unreachable!("impossible source"),
                }
            }
            let first_offer = offers.iter().next().unwrap();
            let capability_type = CapabilityTypeName::from(first_offer);
            Ok(CapabilitySource::<C>::AnonymizedAggregate {
                capability: AggregateCapability::Service(first_offer.source_name().clone()),
                component: aggregation_component.as_weak(),
                aggregate_capability_provider: Box::new(AnonymizedAggregateServiceProvider {
                    collections: collections.clone(),
                    children: children.clone(),
                    containing_component: aggregation_component.as_weak(),
                    capability_name: first_offer.source_name().clone(),
                    capability_type,
                    sources: sources.clone(),
                    visitor: visitor.clone(),
                }),
                collections,
                children,
            })
        }
        OfferResult::OfferFromFilteredAggregate(offers, aggregation_component) => {
            // Check that all of the service offers contain non-conflicting filter instances.
            let mut seen_instances: HashSet<String> = HashSet::new();
            for o in offers.iter() {
                if let OfferDecl::Service(offer_service_decl) = o.clone().into() {
                    match offer_service_decl.source_instance_filter {
                        None => {
                            return Err(RoutingError::unsupported_route_source(
                                "Aggregate offers must be of service capabilities \
                            with source_instance_filter set",
                            ));
                        }
                        Some(allowed_instances) => {
                            for instance in allowed_instances.iter() {
                                if !seen_instances.insert(instance.clone()) {
                                    return Err(RoutingError::unsupported_route_source(format!(
                                        "Instance {} found in multiple offers \
                                                of the same service.",
                                        instance
                                    )));
                                }
                            }
                        }
                    }
                } else {
                    return Err(RoutingError::unsupported_route_source(
                        "Aggregate source must consist of only service capabilities",
                    ));
                }
            }
            // "_unused" is a placeholder value required by fold(). Since `offers` is guaranteed to
            // be nonempty, it will always be overwritten by the actual source name.
            let (source_name, offer_service_decls) = offers.iter().fold(
                ("_unused".parse().unwrap(), Vec::<OfferServiceDecl>::new()),
                |(_, mut decls), o| {
                    if let OfferDecl::Service(offer_service_decl) = o.clone().into() {
                        decls.push(offer_service_decl);
                    }
                    (o.source_name().clone(), decls)
                },
            );
            // TODO(fxbug.dev/71881) Make the Collection CapabilitySource type generic
            // for other types of aggregations.
            Ok(CapabilitySource::<C>::FilteredAggregate {
                capability: AggregateCapability::Service(source_name),
                component: aggregation_component.as_weak(),
                capability_provider: Box::new(OfferAggregateServiceProvider::new(
                    offer_service_decls,
                    aggregation_component.as_weak(),
                    sources.clone(),
                    visitor.clone(),
                )),
            })
        }
    }
}

/// Routes a capability from its `Expose` declaration to its source by following `Expose`
/// declarations.
///
/// `sources` defines what are the valid sources of the capability. See [`AllowedSourcesBuilder`].
/// `visitor` is invoked for each `Expose` declaration in the routing path, as well as the final
/// `Capability` declaration if `sources` permits.
pub async fn route_from_expose<C, S, V, M>(
    expose: RouteBundle<ExposeDecl>,
    expose_target: Arc<C>,
    sources: S,
    visitor: &mut V,
    mapper: &mut M,
) -> Result<CapabilitySource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    S: Sources + 'static,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
    M: DebugRouteMapper + 'static,
{
    match Expose::route(expose, expose_target, &sources, visitor, mapper).await? {
        ExposeResult::Source(source) => Ok(source),
        ExposeResult::ExposeFromAnonymizedAggregate(expose, aggregation_component) => {
            let mut collections = vec![];
            let mut children = vec![];
            for e in expose.iter() {
                match e.source() {
                    ExposeSource::Collection(n) => {
                        collections.push(n.clone());
                    }
                    ExposeSource::Child(n) => {
                        children.push(
                            ChildName::try_new(n.clone(), None)
                                .expect("child source should be convertible to ChildName"),
                        );
                    }
                    _ => unreachable!("this was checked before"),
                }
            }
            let first_expose = expose.iter().next().expect("empty bundle");
            let capability_type = CapabilityTypeName::from(first_expose);
            Ok(CapabilitySource::<C>::AnonymizedAggregate {
                capability: AggregateCapability::Service(first_expose.source_name().clone()),
                component: aggregation_component.as_weak(),
                aggregate_capability_provider: Box::new(AnonymizedAggregateServiceProvider {
                    collections: collections.clone(),
                    children: children.clone(),
                    containing_component: aggregation_component.as_weak(),
                    capability_name: first_expose.source_name().clone(),
                    capability_type,
                    sources: sources.clone(),
                    visitor: visitor.clone(),
                }),
                collections,
                children,
            })
        }
    }
}

/// Routes a capability from its `Use` declaration to its source by capabilities declarations, i.e.
/// whatever capabilities that this component itself provides.
///
/// `sources` defines what are the valid sources of the capability. See [`AllowedSourcesBuilder`].
/// `visitor` is invoked for each `Capability` declaration if `sources` permits.
pub async fn route_from_self<U, C, S, V, M>(
    use_decl: U,
    target: Arc<C>,
    sources: S,
    visitor: &mut V,
    mapper: &mut M,
) -> Result<CapabilitySource<C>, RoutingError>
where
    U: UseDeclCommon + ErrorNotFoundFromParent + Into<UseDecl> + Clone,
    C: ComponentInstanceInterface + 'static,
    S: Sources + 'static,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
    M: DebugRouteMapper + 'static,
{
    mapper.add_use(target.moniker().clone(), &use_decl.clone().into());
    let target_capabilities = target.lock_resolved_state().await?.capabilities();
    Ok(CapabilitySource::<C>::Component {
        capability: sources.find_component_source(
            use_decl.source_name(),
            target.moniker(),
            &target_capabilities,
            visitor,
            mapper,
        )?,
        component: target.as_weak(),
    })
}

/// Routes a capability from its `Use` declaration to its source by following `Use` and `Offer`
/// declarations, and where this capability type does not support `Expose`.
///
/// Panics if an `Expose` is encountered.
///
/// `sources` defines what are the valid sources of the capability. See [`AllowedSourcesBuilder`].
/// `visitor` is invoked for each `Offer` declaration in the routing path, as well as the final
/// `Capability` declaration if `sources` permits.
pub async fn route_from_use_without_expose<U, C, S, V, M>(
    use_decl: U,
    use_target: Arc<C>,
    sources: S,
    visitor: &mut V,
    mapper: &mut M,
) -> Result<CapabilitySource<C>, RoutingError>
where
    U: UseDeclCommon + ErrorNotFoundFromParent + Into<UseDecl> + Clone,
    C: ComponentInstanceInterface + 'static,
    S: Sources,
    V: OfferVisitor,
    V: CapabilityVisitor,
    M: DebugRouteMapper + 'static,
{
    match Use::route(use_decl, use_target, &sources, visitor, mapper).await? {
        UseResult::Source(source) => return Ok(source),
        UseResult::OfferFromParent(offer, component) => {
            route_from_offer_without_expose(offer, component, sources, visitor, mapper).await
        }
        UseResult::ExposeFromChild(_, _) => {
            unreachable!("found use from child but capability cannot be exposed")
        }
    }
}

/// Routes a capability from its `Offer` declaration to its source by following `Offer`
/// declarations, and where this capability type does not support `Expose`.
///
/// Panics if an `Expose` is encountered.
///
/// `sources` defines what are the valid sources of the capability. See [`AllowedSourcesBuilder`].
/// `visitor` is invoked for each `Offer` declaration in the routing path, as well as the final
/// `Capability` declaration if `sources` permits.
pub async fn route_from_offer_without_expose<C, S, V, M>(
    offer: RouteBundle<OfferDecl>,
    offer_target: Arc<C>,
    sources: S,
    visitor: &mut V,
    mapper: &mut M,
) -> Result<CapabilitySource<C>, RoutingError>
where
    C: ComponentInstanceInterface + 'static,
    S: Sources,
    V: OfferVisitor,
    V: CapabilityVisitor,
    M: DebugRouteMapper,
{
    match Offer::route(offer, offer_target, &sources, visitor, mapper).await? {
        OfferResult::Source(source) => Ok(source),
        OfferResult::OfferFromChild(_, _) => {
            // This condition should not happen since cm_fidl_validator ensures
            // that this kind of declaration cannot exist.
            unreachable!("found offer from child but capability cannot be exposed")
        }
        OfferResult::OfferFromAnonymizedAggregate(_, _) => {
            // This condition should not happen since cm_fidl_validator ensures
            // that this kind of declaration cannot exist.
            unreachable!("found offer from collections but capability cannot be exposed")
        }
        OfferResult::OfferFromFilteredAggregate(_, _) => {
            // This condition should not happen since cm_fidl_validator ensures
            // that this kind of declaration cannot exist.
            unreachable!("found offer from aggregate but capability cannot be exposed")
        }
    }
}

/// A trait for extracting the source of a capability.
pub trait Sources: Clone + Send + Sync {
    /// Return the [`InternalCapability`] representing this framework capability source, or
    /// [`RoutingError::UnsupportedRouteSource`] if unsupported.
    fn framework_source<M>(
        &self,
        name: Name,
        mapper: &mut M,
    ) -> Result<InternalCapability, RoutingError>
    where
        M: DebugRouteMapper;

    /// Checks whether capability sources are supported, returning [`RoutingError::UnsupportedRouteSource`]
    /// if they are not.
    // TODO(fxb/61861): Add route mapping for capability sources.
    fn capability_source(&self) -> Result<(), RoutingError>;

    /// Checks whether collection sources are supported, returning [`ModelError::Unsupported`]
    /// if they are not.
    // TODO(fxb/61861): Consider adding route mapping for collection sources, although we won't need this
    // for the route static analyzer.
    fn collection_source(&self) -> Result<(), RoutingError>;

    /// Checks whether namespace capability sources are supported.
    fn is_namespace_supported(&self) -> bool;

    /// Looks for a namespace capability in the list of capability sources.
    /// If found, the declaration is visited by `visitor` and the declaration is wrapped
    /// in a [`ComponentCapability`].
    /// Returns [`RoutingError::UnsupportedRouteSource`] if namespace capabilities are unsupported.
    fn find_namespace_source<V, M>(
        &self,
        name: &Name,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<Option<ComponentCapability>, RoutingError>
    where
        V: CapabilityVisitor,
        M: DebugRouteMapper;

    /// Looks for a built-in capability in the list of capability sources.
    /// If found, the capability's name is wrapped in an [`InternalCapability`].
    /// Returns [`RoutingError::UnsupportedRouteSource`] if built-in capabilities are unsupported.
    fn find_builtin_source<V, M>(
        &self,
        name: &Name,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<Option<InternalCapability>, RoutingError>
    where
        V: CapabilityVisitor,
        M: DebugRouteMapper;

    /// Looks for a component capability in the list of capability sources for the component instance
    /// with moniker `moniker`.
    /// If found, the declaration is visited by `visitor` and the declaration is wrapped
    /// in a [`ComponentCapability`].
    /// Returns [`RoutingError::UnsupportedRouteSource`] if component capabilities are unsupported.
    fn find_component_source<V, M>(
        &self,
        name: &Name,
        moniker: &Moniker,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<ComponentCapability, RoutingError>
    where
        V: CapabilityVisitor,
        M: DebugRouteMapper;
}

/// Defines which capability source types are supported.
#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub struct AllowedSourcesBuilder {
    framework: Option<fn(Name) -> InternalCapability>,
    builtin: bool,
    capability: bool,
    collection: bool,
    namespace: bool,
    component: bool,
    capability_type: CapabilityTypeName,
}

impl AllowedSourcesBuilder {
    /// Creates a new [`AllowedSourcesBuilder`] that does not allow any capability source types.
    pub fn new(capability: CapabilityTypeName) -> Self {
        Self {
            framework: None,
            builtin: false,
            capability: false,
            collection: false,
            namespace: false,
            component: false,
            capability_type: capability,
        }
    }

    /// Allows framework capability source types (`from: "framework"` in `CML`).
    pub fn framework(self, builder: fn(Name) -> InternalCapability) -> Self {
        Self { framework: Some(builder), ..self }
    }

    /// Allows capability source types that originate from other capabilities (`from: "#storage"` in
    /// `CML`).
    pub fn capability(self) -> Self {
        Self { capability: true, ..self }
    }

    /// Allows capability sources to originate from a collection.
    pub fn collection(self) -> Self {
        Self { collection: true, ..self }
    }

    /// Allows namespace capability source types, which are capabilities that are installed in
    /// component_manager's incoming namespace.
    pub fn namespace(self) -> Self {
        Self { namespace: true, ..self }
    }

    /// Allows component capability source types (`from: "self"` in `CML`).
    pub fn component(self) -> Self {
        Self { component: true, ..self }
    }

    /// Allows built-in capability source types (`from: "parent"` in `CML` where the parent component_instance is
    /// component_manager).
    pub fn builtin(self) -> Self {
        Self { builtin: true, ..self }
    }
}

// Implementation of `Sources` that allows namespace, component, and/or built-in source
// types.
impl Sources for AllowedSourcesBuilder {
    fn framework_source<M>(
        &self,
        name: Name,
        mapper: &mut M,
    ) -> Result<InternalCapability, RoutingError>
    where
        M: DebugRouteMapper,
    {
        let source = self
            .framework
            .as_ref()
            .map(|b| b(name.clone()))
            .ok_or_else(|| RoutingError::unsupported_route_source("framework"));
        mapper.add_framework_capability(name);
        source
    }

    fn capability_source(&self) -> Result<(), RoutingError> {
        if self.capability {
            Ok(())
        } else {
            Err(RoutingError::unsupported_route_source("capability"))
        }
    }

    fn collection_source(&self) -> Result<(), RoutingError> {
        if self.collection {
            Ok(())
        } else {
            Err(RoutingError::unsupported_route_source("collection"))
        }
    }

    fn is_namespace_supported(&self) -> bool {
        self.namespace
    }

    fn find_namespace_source<V, M>(
        &self,
        name: &Name,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<Option<ComponentCapability>, RoutingError>
    where
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        if self.namespace {
            if let Some(decl) = capabilities
                .iter()
                .find(|decl: &&CapabilityDecl| {
                    self.capability_type == CapabilityTypeName::from(*decl) && decl.name() == name
                })
                .cloned()
            {
                visitor.visit(&decl)?;
                mapper.add_namespace_capability(&decl);
                Ok(Some(decl.into()))
            } else {
                Ok(None)
            }
        } else {
            Err(RoutingError::unsupported_route_source("namespace"))
        }
    }

    fn find_builtin_source<V, M>(
        &self,
        name: &Name,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<Option<InternalCapability>, RoutingError>
    where
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        if self.builtin {
            if let Some(decl) = capabilities
                .iter()
                .find(|decl: &&CapabilityDecl| {
                    self.capability_type == CapabilityTypeName::from(*decl) && decl.name() == name
                })
                .cloned()
            {
                visitor.visit(&decl)?;
                mapper.add_builtin_capability(&decl);
                Ok(Some(decl.into()))
            } else {
                Ok(None)
            }
        } else {
            Err(RoutingError::unsupported_route_source("built-in"))
        }
    }

    fn find_component_source<V, M>(
        &self,
        name: &Name,
        moniker: &Moniker,
        capabilities: &[CapabilityDecl],
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<ComponentCapability, RoutingError>
    where
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        if self.component {
            let decl = capabilities
                .iter()
                .find(|decl: &&CapabilityDecl| {
                    self.capability_type == CapabilityTypeName::from(*decl) && decl.name() == name
                })
                .cloned()
                .expect("CapabilityDecl missing, FIDL validation should catch this");
            visitor.visit(&decl)?;
            mapper.add_component_capability(moniker.clone(), &decl);
            Ok(decl.into())
        } else {
            Err(RoutingError::unsupported_route_source("component"))
        }
    }
}

/// The `Use` phase of routing.
pub struct Use<U>(PhantomData<U>);

/// The result of routing a Use declaration to the next phase.
enum UseResult<C: ComponentInstanceInterface, O: Clone + fmt::Debug, U> {
    /// The source of the Use was found (Framework, AboveRoot, etc.)
    Source(CapabilitySource<C>),
    /// The Use led to a parent offer.
    OfferFromParent(RouteBundle<O>, Arc<C>),
    /// The Use led to a child Expose declaration.
    /// Note: Instead of FromChild carrying an ExposeDecl of the matching child, it carries a
    /// UseDecl. This is because some RoutingStrategy<> don't support Expose, but are still
    /// required to enumerate over UseResult<>.
    ExposeFromChild(U, Arc<C>),
}

impl<U> Use<U>
where
    U: UseDeclCommon + ErrorNotFoundFromParent + Into<UseDecl> + Clone,
{
    /// Routes the capability starting from the `use_` declaration at `target` to either a valid
    /// source (as defined by `sources`) or the Offer declaration that ends this phase of routing.
    async fn route<C, S, V, M>(
        use_: U,
        target: Arc<C>,
        sources: &S,
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<UseResult<C, OfferDecl, U>, RoutingError>
    where
        C: ComponentInstanceInterface,
        S: Sources,
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        let use_decl: UseDecl = use_.clone().into();
        mapper.add_use(target.moniker().clone(), &use_decl);
        match use_.source() {
            UseSource::Framework => Ok(UseResult::Source(CapabilitySource::<C>::Framework {
                capability: sources.framework_source(use_.source_name().clone(), mapper)?,
                component: target.as_weak(),
            })),
            UseSource::Capability(_) => {
                sources.capability_source()?;
                Ok(UseResult::Source(CapabilitySource::<C>::Capability {
                    component: target.as_weak(),
                    source_capability: ComponentCapability::Use(use_.into()),
                }))
            }
            UseSource::Parent => match target.try_get_parent()? {
                ExtendedInstanceInterface::<C>::AboveRoot(top_instance) => {
                    if sources.is_namespace_supported() {
                        if let Some(capability) = sources.find_namespace_source(
                            use_.source_name(),
                            top_instance.namespace_capabilities(),
                            visitor,
                            mapper,
                        )? {
                            return Ok(UseResult::Source(CapabilitySource::<C>::Namespace {
                                capability,
                                top_instance: Arc::downgrade(&top_instance),
                            }));
                        }
                    }
                    if let Some(capability) = sources.find_builtin_source(
                        use_.source_name(),
                        top_instance.builtin_capabilities(),
                        visitor,
                        mapper,
                    )? {
                        return Ok(UseResult::Source(CapabilitySource::<C>::Builtin {
                            capability,
                            top_instance: Arc::downgrade(&top_instance),
                        }));
                    }
                    Err(RoutingError::use_from_component_manager_not_found(
                        use_.source_name().to_string(),
                    ))
                }
                ExtendedInstanceInterface::<C>::Component(parent_component) => {
                    let parent_offers = parent_component.lock_resolved_state().await?.offers();
                    let child_moniker = target.child_moniker().expect("ChildName should exist");
                    let parent_offers = find_matching_offers(
                        CapabilityTypeName::from(&use_decl),
                        use_.source_name(),
                        &child_moniker,
                        &parent_offers,
                    )
                    .ok_or_else(|| {
                        <U as ErrorNotFoundFromParent>::error_not_found_from_parent(
                            target.moniker().clone(),
                            use_.source_name().clone(),
                        )
                    })?;
                    Ok(UseResult::OfferFromParent(parent_offers, parent_component))
                }
            },
            UseSource::Child(name) => {
                let moniker = target.moniker();
                let child_component = {
                    let child_moniker = ChildName::try_new(name, None)?;
                    target.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                        || {
                            RoutingError::use_from_child_instance_not_found(
                                &child_moniker,
                                moniker,
                                name.clone(),
                            )
                        },
                    )?
                };
                Ok(UseResult::ExposeFromChild(use_, child_component))
            }
            UseSource::Debug => {
                // This is not supported today. It might be worthwhile to support this if
                // more than just protocol has a debug capability.
                return Err(RoutingError::unsupported_route_source("debug capability"));
            }
            UseSource::Self_ => {
                return Err(RoutingError::unsupported_route_source("self"));
            }
            UseSource::Environment => {
                // This is not supported today. It might be worthwhile to support this if
                // capabilities other than runner can be used from environment.
                return Err(RoutingError::unsupported_route_source("environment"));
            }
        }
    }
}

/// The environment `Registration` phase of routing.
pub struct Registration<R>(PhantomData<R>);

/// The result of routing a Registration declaration to the next phase.
enum RegistrationResult<C: ComponentInstanceInterface, O: Clone + fmt::Debug> {
    /// The source of the Registration was found (Framework, AboveRoot, etc.).
    Source(CapabilitySource<C>),
    /// The Registration led to a parent Offer declaration.
    FromParent(RouteBundle<O>, Arc<C>),
    /// The Registration led to a child Expose declaration.
    FromChild(RouteBundle<ExposeDecl>, Arc<C>),
}

impl<R> Registration<R>
where
    R: RegistrationDeclCommon
        + ErrorNotFoundFromParent
        + ErrorNotFoundInChild
        + Into<RegistrationDecl>
        + Clone,
{
    /// Routes the capability starting from the `registration` declaration at `target` to either a
    /// valid source (as defined by `sources`) or the Offer or Expose declaration that ends this
    /// phase of routing.
    async fn route<C, S, V, M>(
        registration: R,
        target: Arc<C>,
        sources: &S,
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<RegistrationResult<C, OfferDecl>, RoutingError>
    where
        C: ComponentInstanceInterface,
        S: Sources,
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        let registration_decl: RegistrationDecl = registration.clone().into();
        mapper.add_registration(target.moniker().clone(), &registration_decl);
        match registration.source() {
            RegistrationSource::Self_ => {
                let target_capabilities = target.lock_resolved_state().await?.capabilities();
                Ok(RegistrationResult::Source(CapabilitySource::<C>::Component {
                    capability: sources.find_component_source(
                        registration.source_name(),
                        target.moniker(),
                        &target_capabilities,
                        visitor,
                        mapper,
                    )?,
                    component: target.as_weak(),
                }))
            }
            RegistrationSource::Parent => match target.try_get_parent()? {
                ExtendedInstanceInterface::<C>::AboveRoot(top_instance) => {
                    if sources.is_namespace_supported() {
                        if let Some(capability) = sources.find_namespace_source(
                            registration.source_name(),
                            top_instance.namespace_capabilities(),
                            visitor,
                            mapper,
                        )? {
                            return Ok(RegistrationResult::Source(
                                CapabilitySource::<C>::Namespace {
                                    capability,
                                    top_instance: Arc::downgrade(&top_instance),
                                },
                            ));
                        }
                    }
                    if let Some(capability) = sources.find_builtin_source(
                        registration.source_name(),
                        top_instance.builtin_capabilities(),
                        visitor,
                        mapper,
                    )? {
                        return Ok(RegistrationResult::Source(CapabilitySource::<C>::Builtin {
                            capability,
                            top_instance: Arc::downgrade(&top_instance),
                        }));
                    }
                    Err(RoutingError::register_from_component_manager_not_found(
                        registration.source_name().to_string(),
                    ))
                }
                ExtendedInstanceInterface::<C>::Component(parent_component) => {
                    let parent_offers = parent_component.lock_resolved_state().await?.offers();
                    let child_moniker = target.child_moniker().expect("ChildName should exist");
                    let parent_offers = find_matching_offers(
                        CapabilityTypeName::from(&registration_decl),
                        registration.source_name(),
                        &child_moniker,
                        &parent_offers,
                    )
                    .ok_or_else(|| {
                        <R as ErrorNotFoundFromParent>::error_not_found_from_parent(
                            target.moniker().clone(),
                            registration.source_name().clone(),
                        )
                    })?;
                    Ok(RegistrationResult::FromParent(parent_offers, parent_component))
                }
            },
            RegistrationSource::Child(child) => {
                let child_component = {
                    let child_moniker = ChildName::try_new(child, None)?;
                    target.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                        || RoutingError::EnvironmentFromChildInstanceNotFound {
                            child_moniker,
                            moniker: target.moniker().clone(),
                            capability_name: registration.source_name().clone(),
                            capability_type: R::TYPE.to_string(),
                        },
                    )?
                };

                let child_exposes = child_component.lock_resolved_state().await?.exposes();
                let capability_type = CapabilityTypeName::from(&registration.clone().into());
                let child_exposes = find_matching_exposes(
                    capability_type,
                    registration.source_name(),
                    &child_exposes,
                )
                .ok_or_else(|| {
                    let child_moniker =
                        child_component.child_moniker().expect("ChildName should exist");
                    <R as ErrorNotFoundInChild>::error_not_found_in_child(
                        target.moniker().clone(),
                        child_moniker.clone(),
                        registration.source_name().clone(),
                    )
                })?;
                Ok(RegistrationResult::FromChild(child_exposes, child_component.clone()))
            }
        }
    }
}

/// The `Offer` phase of routing.
pub struct Offer();

/// The result of routing an Offer declaration to the next phase.
enum OfferResult<C: ComponentInstanceInterface> {
    /// The source of the Offer was found (Framework, AboveRoot, Component, etc.).
    Source(CapabilitySource<C>),
    /// The Offer led to an Offer-from-child declaration.
    /// Not all capabilities can be exposed, so let the caller decide how to handle this.
    OfferFromChild(OfferDecl, Arc<C>),
    /// Offer from multiple static children, with filters.
    OfferFromFilteredAggregate(RouteBundle<OfferDecl>, Arc<C>),
    /// Offer from one or more collections and/or static children.
    OfferFromAnonymizedAggregate(RouteBundle<OfferDecl>, Arc<C>),
}

enum OfferSegment<C: ComponentInstanceInterface> {
    Done(OfferResult<C>),
    Next(RouteBundle<OfferDecl>, Arc<C>),
}

impl Offer {
    /// Routes the capability starting from the `offer` declaration at `target` to either a valid
    /// source (as defined by `sources`) or the declaration that ends this phase of routing.
    async fn route<C, S, V, M>(
        mut offer_bundle: RouteBundle<OfferDecl>,
        mut target: Arc<C>,
        sources: &S,
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<OfferResult<C>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        S: Sources,
        V: OfferVisitor,
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        loop {
            let visit_offer = match &offer_bundle {
                RouteBundle::Single(offer) => Some(offer),
                RouteBundle::Aggregate(offers) => {
                    // TODO(fxbug.dev/4776): Visit routes in all aggregates.
                    if offers.len() == 1 {
                        Some(&offers[0])
                    } else {
                        None
                    }
                }
            };
            if let Some(visit_offer) = visit_offer {
                mapper.add_offer(target.moniker().clone(), &visit_offer);
                OfferVisitor::visit(visitor, &visit_offer)?;
            }

            fn is_filtered_offer(o: &OfferDecl) -> bool {
                if let OfferDecl::Service(offer_service) = o {
                    if let Some(f) = offer_service.source_instance_filter.as_ref() {
                        if !f.is_empty() {
                            return true;
                        }
                    }
                    if let Some(f) = offer_service.renamed_instances.as_ref() {
                        if !f.is_empty() {
                            return true;
                        }
                    }
                }
                false
            }

            match offer_bundle {
                RouteBundle::Single(offer) => {
                    match Self::route_segment(offer, target, sources, visitor, mapper).await? {
                        OfferSegment::Done(r) => return Ok(r),
                        OfferSegment::Next(o, t) => {
                            offer_bundle = o;
                            target = t;
                        }
                    }
                }
                RouteBundle::Aggregate(_) => {
                    let decl_target = offer_bundle.iter().next().unwrap().target();
                    if offer_bundle.iter().any(|o| o.target() != decl_target) {
                        // TODO(fxbug.dev/298698003): This situation should be caught by the
                        // validator, but due to this bug it is not. Fail the route.
                        return Err(RoutingError::UnsupportedRouteSource {
                            source_type: "disallowed aggregate".into(),
                        });
                    }
                    if offer_bundle.iter().all(|o| !is_filtered_offer(o)) {
                        return Ok(OfferResult::OfferFromAnonymizedAggregate(offer_bundle, target));
                    } else {
                        return Ok(OfferResult::OfferFromFilteredAggregate(offer_bundle, target));
                    }
                }
            }
        }
    }

    async fn route_segment<C, S, V, M>(
        offer: OfferDecl,
        target: Arc<C>,
        sources: &S,
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<OfferSegment<C>, RoutingError>
    where
        C: ComponentInstanceInterface + 'static,
        S: Sources,
        V: OfferVisitor,
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        let res = match offer.source() {
            OfferSource::Void => {
                panic!("an error should have been emitted by the availability walker before we reach this point");
            }
            OfferSource::Self_ => {
                let target_capabilities = target.lock_resolved_state().await?.capabilities();
                let capability = sources.find_component_source(
                    offer.source_name(),
                    target.moniker(),
                    &target_capabilities,
                    visitor,
                    mapper,
                )?;
                // if offerdecl is for a filtered service return the associated filtered source.
                let component = target.as_weak();
                let res = match offer.into() {
                    OfferDecl::Service(offer_service_decl) => {
                        if offer_service_decl.source_instance_filter.is_some()
                            || offer_service_decl.renamed_instances.is_some()
                        {
                            let source_name = offer_service_decl.source_name.clone();
                            let capability_provider = Box::new(OfferFilteredServiceProvider::new(
                                offer_service_decl,
                                component.clone(),
                                capability,
                            ));
                            OfferResult::Source(CapabilitySource::<C>::FilteredAggregate {
                                capability: AggregateCapability::Service(source_name),
                                component,
                                capability_provider,
                            })
                        } else {
                            OfferResult::Source(CapabilitySource::<C>::Component {
                                capability,
                                component,
                            })
                        }
                    }
                    _ => OfferResult::Source(CapabilitySource::<C>::Component {
                        capability,
                        component,
                    }),
                };
                OfferSegment::Done(res)
            }
            OfferSource::Framework => {
                OfferSegment::Done(OfferResult::Source(CapabilitySource::<C>::Framework {
                    capability: sources.framework_source(offer.source_name().clone(), mapper)?,
                    component: target.as_weak(),
                }))
            }
            OfferSource::Capability(_) => {
                sources.capability_source()?;
                OfferSegment::Done(OfferResult::Source(CapabilitySource::<C>::Capability {
                    source_capability: ComponentCapability::Offer(offer.into()),
                    component: target.as_weak(),
                }))
            }
            OfferSource::Parent => {
                let parent_component = match target.try_get_parent()? {
                    ExtendedInstanceInterface::<C>::AboveRoot(top_instance) => {
                        if sources.is_namespace_supported() {
                            if let Some(capability) = sources.find_namespace_source(
                                offer.source_name(),
                                top_instance.namespace_capabilities(),
                                visitor,
                                mapper,
                            )? {
                                return Ok(OfferSegment::Done(OfferResult::Source(
                                    CapabilitySource::<C>::Namespace {
                                        capability,
                                        top_instance: Arc::downgrade(&top_instance),
                                    },
                                )));
                            }
                        }
                        if let Some(capability) = sources.find_builtin_source(
                            offer.source_name(),
                            top_instance.builtin_capabilities(),
                            visitor,
                            mapper,
                        )? {
                            return Ok(OfferSegment::Done(OfferResult::Source(
                                CapabilitySource::<C>::Builtin {
                                    capability,
                                    top_instance: Arc::downgrade(&top_instance),
                                },
                            )));
                        }
                        return Err(RoutingError::offer_from_component_manager_not_found(
                            offer.source_name().to_string(),
                        ));
                    }
                    ExtendedInstanceInterface::<C>::Component(component) => component,
                };
                let child_moniker = target.child_moniker().expect("ChildName should exist");
                let parent_offers = parent_component.lock_resolved_state().await?.offers();
                let parent_offers = find_matching_offers(
                    CapabilityTypeName::from(&offer),
                    offer.source_name(),
                    &child_moniker,
                    &parent_offers,
                )
                .ok_or_else(|| {
                    <OfferDecl as ErrorNotFoundFromParent>::error_not_found_from_parent(
                        target.moniker().clone(),
                        offer.source_name().clone(),
                    )
                })?;
                OfferSegment::Next(parent_offers, parent_component)
            }
            OfferSource::Child(_) => OfferSegment::Done(OfferResult::OfferFromChild(offer, target)),
            OfferSource::Collection(_) => OfferSegment::Done(
                OfferResult::OfferFromAnonymizedAggregate(RouteBundle::from_offer(offer), target),
            ),
        };
        Ok(res)
    }
}

/// Finds the matching Expose declaration for an Offer-from-child, changing the
/// direction in which the Component Tree is being navigated (from up to down).
async fn change_directions<C>(
    offer: OfferDecl,
    component: Arc<C>,
) -> Result<(RouteBundle<ExposeDecl>, Arc<C>), RoutingError>
where
    C: ComponentInstanceInterface,
{
    match offer.source() {
        OfferSource::Child(child) => {
            let child_component = {
                let child_moniker = ChildName::try_new(
                    child.name.as_str(),
                    child.collection.as_ref().map(|s| s.as_str()),
                )?;
                component.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                    || RoutingError::OfferFromChildInstanceNotFound {
                        child_moniker,
                        moniker: component.moniker().clone(),
                        capability_id: offer.source_name().clone().into(),
                    },
                )?
            };
            let child_exposes = child_component.lock_resolved_state().await?.exposes();
            let child_exposes = find_matching_exposes(
                CapabilityTypeName::from(&offer),
                offer.source_name(),
                &child_exposes,
            )
            .ok_or_else(|| {
                let child_moniker =
                    child_component.child_moniker().expect("ChildName should exist");
                <OfferDecl as ErrorNotFoundInChild>::error_not_found_in_child(
                    component.moniker().clone(),
                    child_moniker.clone(),
                    offer.source_name().clone(),
                )
            })?;
            Ok((child_exposes, child_component.clone()))
        }
        _ => panic!("change_direction called with offer that does not change direction"),
    }
}

/// The `Expose` phase of routing.
#[derive(Debug)]
pub struct Expose();

/// The result of routing an Expose declaration to the next phase.
enum ExposeResult<C: ComponentInstanceInterface> {
    /// The source of the Expose was found (Framework, Component, etc.).
    Source(CapabilitySource<C>),
    /// The source of the Expose comes from an aggregation of collections and/or static children
    ExposeFromAnonymizedAggregate(RouteBundle<ExposeDecl>, Arc<C>),
}

/// A bundle of one or more routing declarations to route together, that share the same target_name
#[derive(Clone, Debug)]
pub enum RouteBundle<T>
where
    T: Clone + fmt::Debug,
{
    /// A single route from a unique source.
    Single(T),
    /// A bundle of routes representing an aggregated capability. This can be a vector of one,
    /// e.g. exposing a service from a collection.
    Aggregate(Vec<T>),
}

impl<T> RouteBundle<T>
where
    T: Clone + fmt::Debug,
{
    pub fn map<U: Clone + fmt::Debug>(self, mut f: impl FnMut(T) -> U) -> RouteBundle<U> {
        match self {
            RouteBundle::Single(r) => RouteBundle::Single(f(r)),
            RouteBundle::Aggregate(r) => {
                RouteBundle::Aggregate(r.into_iter().map(&mut f).collect())
            }
        }
    }

    /// Returns an iterator over the values of `OneOrMany<T>`.
    pub fn iter(&self) -> RouteBundleIter<'_, T> {
        match self {
            Self::Single(item) => {
                RouteBundleIter { inner_single: Some(item), inner_aggregate: None }
            }
            Self::Aggregate(items) => {
                RouteBundleIter { inner_single: None, inner_aggregate: Some(items.iter()) }
            }
        }
    }

    /// Returns the number of routes in this bundle.
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Aggregate(v) => v.len(),
        }
    }

    /// Creates a `RouteBundle` from of a single offer routing declaration.
    pub fn from_offer(input: T) -> Self
    where
        T: OfferDeclCommon + Clone,
    {
        Self::from_offers(vec![input])
    }

    /// Creates a `RouteBundle` from of a list of offer routing declarations.
    ///
    /// REQUIRES: `input` is nonempty.
    /// REQUIRES: All elements of `input` share the same `target_name`.
    pub fn from_offers(mut input: Vec<T>) -> Self
    where
        T: OfferDeclCommon + Clone,
    {
        match input.len() {
            1 => {
                let input = input.remove(0);
                match input.source() {
                    OfferSource::Collection(_) => Self::Aggregate(vec![input]),
                    _ => Self::Single(input),
                }
            }
            0 => panic!("empty bundles are not allowed"),
            _ => Self::Aggregate(input),
        }
    }

    /// Creates a `RouteBundle` from of a single expose routing declaration.
    pub fn from_expose(input: T) -> Self
    where
        T: ExposeDeclCommon + Clone,
    {
        Self::from_exposes(vec![input])
    }

    /// Creates a `RouteBundle` from of a list of expose routing declarations.
    ///
    /// REQUIRES: `input` is nonempty.
    /// REQUIRES: All elements of `input` share the same `target_name`.
    pub fn from_exposes(mut input: Vec<T>) -> Self
    where
        T: ExposeDeclCommon + Clone,
    {
        match input.len() {
            1 => {
                let input = input.remove(0);
                match input.source() {
                    ExposeSource::Collection(_) => Self::Aggregate(vec![input]),
                    _ => Self::Single(input),
                }
            }
            0 => panic!("empty bundles are not allowed"),
            _ => Self::Aggregate(input),
        }
    }
}

impl<T> RouteBundle<T>
where
    T: ExposeDeclCommon + Clone,
{
    pub fn availability(&self) -> &Availability {
        match self {
            Self::Single(e) => e.availability(),
            Self::Aggregate(v) => {
                assert!(
                    v.iter().zip(v.iter().skip(1)).all(|(a, b)| a.availability() == b.availability()),
                    "CM validation should ensure all aggregated capabilities have the same availability");
                v[0].availability()
            }
        }
    }
}

/// Immutable iterator over a `RouteBundle`.
/// This `struct` is created by [`RouteBundle::iter`].
///
/// [`RouteBundle::iter`]: struct.RouteBundle.html#method.iter
pub struct RouteBundleIter<'a, T> {
    inner_single: Option<&'a T>,
    inner_aggregate: Option<slice::Iter<'a, T>>,
}

impl<'a, T> Iterator for RouteBundleIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(item) = self.inner_single.take() {
            Some(item)
        } else if let Some(ref mut iter) = &mut self.inner_aggregate {
            iter.next()
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if let Some(_) = self.inner_single {
            (1, Some(1))
        } else if let Some(iter) = &self.inner_aggregate {
            iter.size_hint()
        } else {
            (0, Some(0))
        }
    }
}

impl<'a, T> ExactSizeIterator for RouteBundleIter<'a, T> {}

enum ExposeSegment<C: ComponentInstanceInterface> {
    Done(ExposeResult<C>),
    Next(RouteBundle<ExposeDecl>, Arc<C>),
}

impl Expose {
    /// Routes the capability starting from the `expose` declaration at `target` to a valid source
    /// (as defined by `sources`).
    async fn route<C, S, V, M>(
        mut expose_bundle: RouteBundle<ExposeDecl>,
        mut target: Arc<C>,
        sources: &S,
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<ExposeResult<C>, RoutingError>
    where
        C: ComponentInstanceInterface,
        S: Sources,
        V: ExposeVisitor,
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        loop {
            let visit_expose = match &expose_bundle {
                RouteBundle::Single(expose) => Some(expose),
                RouteBundle::Aggregate(exposes) => {
                    // TODO(fxbug.dev/4776): Visit routes in all aggregates.
                    if exposes.len() == 1 {
                        Some(&exposes[0])
                    } else {
                        None
                    }
                }
            };
            if let Some(visit_expose) = visit_expose {
                mapper.add_expose(target.moniker().clone(), visit_expose.clone().into());
                ExposeVisitor::visit(visitor, &visit_expose)?;
            }

            match expose_bundle {
                RouteBundle::Single(expose) => {
                    match Self::route_segment(expose, target, sources, visitor, mapper).await? {
                        ExposeSegment::Done(r) => return Ok(r),
                        ExposeSegment::Next(e, t) => {
                            expose_bundle = e;
                            target = t;
                        }
                    }
                }
                RouteBundle::Aggregate(_) => {
                    if expose_bundle
                        .iter()
                        .any(|e| !matches!(e.source(), ExposeSource::Collection(_)))
                    {
                        // TODO(fxbug.dev/4776): Support aggregation over collections and children.
                        return Err(RoutingError::UnsupportedRouteSource {
                            source_type: "mix of collections and child routes".into(),
                        });
                    }
                    return Ok(ExposeResult::ExposeFromAnonymizedAggregate(expose_bundle, target));
                }
            }
        }
    }

    async fn route_segment<C, S, V, M>(
        expose: ExposeDecl,
        target: Arc<C>,
        sources: &S,
        visitor: &mut V,
        mapper: &mut M,
    ) -> Result<ExposeSegment<C>, RoutingError>
    where
        C: ComponentInstanceInterface,
        S: Sources,
        V: ExposeVisitor,
        V: CapabilityVisitor,
        M: DebugRouteMapper,
    {
        let res = match expose.source() {
            ExposeSource::Void => {
                panic!("an error should have been emitted by the availability walker before we reach this point");
            }
            ExposeSource::Self_ => {
                let target_capabilities = target.lock_resolved_state().await?.capabilities();
                ExposeSegment::Done(ExposeResult::Source(CapabilitySource::<C>::Component {
                    capability: sources.find_component_source(
                        expose.source_name(),
                        target.moniker(),
                        &target_capabilities,
                        visitor,
                        mapper,
                    )?,
                    component: target.as_weak(),
                }))
            }
            ExposeSource::Framework => {
                ExposeSegment::Done(ExposeResult::Source(CapabilitySource::<C>::Framework {
                    capability: sources.framework_source(expose.source_name().clone(), mapper)?,
                    component: target.as_weak(),
                }))
            }
            ExposeSource::Capability(_) => {
                sources.capability_source()?;
                ExposeSegment::Done(ExposeResult::Source(CapabilitySource::<C>::Capability {
                    source_capability: ComponentCapability::Expose(expose.into()),
                    component: target.as_weak(),
                }))
            }
            ExposeSource::Child(child) => {
                let child_component = {
                    let child_moniker = ChildName::try_new(child, None)?;
                    target.lock_resolved_state().await?.get_child(&child_moniker).ok_or_else(
                        || RoutingError::ExposeFromChildInstanceNotFound {
                            child_moniker,
                            moniker: target.moniker().clone(),
                            capability_id: expose.source_name().clone().into(),
                        },
                    )?
                };
                let child_exposes = child_component.lock_resolved_state().await?.exposes();
                let child_exposes = find_matching_exposes(
                    CapabilityTypeName::from(&expose),
                    expose.source_name(),
                    &child_exposes,
                )
                .ok_or_else(|| {
                    let child_moniker =
                        child_component.child_moniker().expect("ChildName should exist");
                    <ExposeDecl as ErrorNotFoundInChild>::error_not_found_in_child(
                        target.moniker().clone(),
                        child_moniker.clone(),
                        expose.source_name().clone(),
                    )
                })?;
                ExposeSegment::Next(child_exposes, child_component)
            }
            ExposeSource::Collection(_) => {
                ExposeSegment::Done(ExposeResult::ExposeFromAnonymizedAggregate(
                    RouteBundle::from_expose(expose),
                    target,
                ))
            }
        };
        Ok(res)
    }
}

fn target_matches_moniker(target: &OfferTarget, child_moniker: &ChildName) -> bool {
    match target {
        OfferTarget::Child(target_ref) => {
            target_ref.name == child_moniker.name()
                && target_ref.collection.as_ref() == child_moniker.collection()
        }
        OfferTarget::Collection(target_collection) => {
            Some(target_collection) == child_moniker.collection()
        }
        OfferTarget::Capability(_target_capability) => {
            // TODO(fxbug.dev/301674053): Support dictionary targets.
            false
        }
    }
}

/// Visitor pattern trait for visiting all [`OfferDecl`] during a route.
pub trait OfferVisitor {
    fn visit(&mut self, offer: &OfferDecl) -> Result<(), RoutingError>;
}

/// Visitor pattern trait for visiting all [`ExposeDecl`] during a route.
pub trait ExposeVisitor {
    /// Visit each [`ExposeDecl`] on the route.
    /// Returning an `Err` cancels visitation.
    fn visit(&mut self, expose: &ExposeDecl) -> Result<(), RoutingError>;
}

/// Visitor pattern trait for visiting all [`CapabilityDecl`] during a route.
pub trait CapabilityVisitor {
    /// Visit each [`CapabilityDecl`] on the route.
    /// Returning an `Err` cancels visitation.
    fn visit(&mut self, capability: &CapabilityDecl) -> Result<(), RoutingError>;
}

pub fn find_matching_offers(
    capability_type: CapabilityTypeName,
    source_name: &Name,
    child_moniker: &ChildName,
    offers: &Vec<OfferDecl>,
) -> Option<RouteBundle<OfferDecl>> {
    let offers: Vec<_> = offers
        .iter()
        .filter(|offer: &&OfferDecl| {
            capability_type == CapabilityTypeName::from(*offer)
                && *offer.target_name() == *source_name
                && target_matches_moniker(offer.target(), &child_moniker)
        })
        .cloned()
        .collect();
    if offers.is_empty() {
        return None;
    }
    Some(RouteBundle::from_offers(offers))
}

pub fn find_matching_exposes(
    capability_type: CapabilityTypeName,
    source_name: &Name,
    exposes: &Vec<ExposeDecl>,
) -> Option<RouteBundle<ExposeDecl>> {
    let exposes: Vec<_> = exposes
        .iter()
        .filter(|expose: &&ExposeDecl| {
            capability_type == CapabilityTypeName::from(*expose)
                && *expose.target_name() == *source_name
                && *expose.target() == ExposeTarget::Parent
        })
        .cloned()
        .collect();
    if exposes.is_empty() {
        return None;
    }
    Some(RouteBundle::from_exposes(exposes))
}

/// Implemented by declaration types to emit a proper error when a matching offer is not found in the parent.
pub trait ErrorNotFoundFromParent {
    fn error_not_found_from_parent(
        decl_site_moniker: Moniker,
        capability_name: Name,
    ) -> RoutingError;
}

/// Implemented by declaration types to emit a proper error when a matching expose is not found in the child.
pub trait ErrorNotFoundInChild {
    fn error_not_found_in_child(
        decl_site_moniker: Moniker,
        child_moniker: ChildName,
        capability_name: Name,
    ) -> RoutingError;
}

#[derive(Clone)]
pub struct NoopVisitor {}

impl NoopVisitor {
    pub fn new() -> NoopVisitor {
        NoopVisitor {}
    }
}

impl crate::router::OfferVisitor for NoopVisitor {
    fn visit(&mut self, _: &OfferDecl) -> Result<(), RoutingError> {
        Ok(())
    }
}

impl crate::router::ExposeVisitor for NoopVisitor {
    fn visit(&mut self, _: &ExposeDecl) -> Result<(), RoutingError> {
        Ok(())
    }
}

impl crate::router::CapabilityVisitor for NoopVisitor {
    fn visit(&mut self, _: &CapabilityDecl) -> Result<(), RoutingError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        cm_rust::{Availability, ChildRef, ExposeServiceDecl},
    };

    #[test]
    fn route_bundle_iter_single() {
        let v = RouteBundle::Single(34);
        let mut iter = v.iter();
        assert_matches!(iter.next(), Some(&34));
        assert_matches!(iter.next(), None);
    }

    #[test]
    fn route_bundle_iter_many() {
        let v = RouteBundle::Aggregate(vec![1, 2, 3]);
        let mut iter = v.iter();
        assert_matches!(iter.next(), Some(&1));
        assert_matches!(iter.next(), Some(&2));
        assert_matches!(iter.next(), Some(&3));
        assert_matches!(iter.next(), None);
    }

    #[test]
    fn route_bundle_from_offers() {
        let parent_offers: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|i| OfferServiceDecl {
                source: OfferSource::Parent,
                source_name: format!("foo_source_{}", i).parse().unwrap(),
                source_dictionary: None,
                target: OfferTarget::Collection("coll".parse().unwrap()),
                target_name: "foo_target".parse().unwrap(),
                source_instance_filter: None,
                renamed_instances: None,
                availability: Availability::Required,
            })
            .collect();
        let collection_offer = OfferServiceDecl {
            source: OfferSource::Collection("coll".parse().unwrap()),
            source_name: "foo_source".parse().unwrap(),
            source_dictionary: None,
            target: OfferTarget::Child(ChildRef { name: "target".into(), collection: None }),
            target_name: "foo_target".parse().unwrap(),
            source_instance_filter: None,
            renamed_instances: None,
            availability: Availability::Required,
        };
        assert_matches!(
            RouteBundle::from_offer(parent_offers[0].clone()),
            RouteBundle::Single(o) if o == parent_offers[0]
        );
        assert_matches!(
            RouteBundle::from_offers(vec![parent_offers[0].clone()]),
            RouteBundle::Single(o) if o == parent_offers[0]
        );
        assert_matches!(
            RouteBundle::from_offers(parent_offers.clone()),
            RouteBundle::Aggregate(v) if v == parent_offers
        );
        assert_matches!(
            RouteBundle::from_offer(collection_offer.clone()),
            RouteBundle::Aggregate(v) if v == vec![collection_offer.clone()]
        );
    }

    #[test]
    fn route_bundle_from_exposes() {
        let child_exposes: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|i| ExposeServiceDecl {
                source: ExposeSource::Child("source".into()),
                source_name: format!("foo_source_{}", i).parse().unwrap(),
                source_dictionary: None,
                target: ExposeTarget::Parent,
                target_name: "foo_target".parse().unwrap(),
                availability: Availability::Required,
            })
            .collect();
        let collection_expose = ExposeServiceDecl {
            source: ExposeSource::Collection("coll".parse().unwrap()),
            source_name: "foo_source".parse().unwrap(),
            source_dictionary: None,
            target: ExposeTarget::Parent,
            target_name: "foo_target".parse().unwrap(),
            availability: Availability::Required,
        };
        assert_matches!(
            RouteBundle::from_expose(child_exposes[0].clone()),
            RouteBundle::Single(o) if o == child_exposes[0]
        );
        assert_matches!(
            RouteBundle::from_exposes(vec![child_exposes[0].clone()]),
            RouteBundle::Single(o) if o == child_exposes[0]
        );
        assert_matches!(
            RouteBundle::from_exposes(child_exposes.clone()),
            RouteBundle::Aggregate(v) if v == child_exposes
        );
        assert_matches!(
            RouteBundle::from_expose(collection_expose.clone()),
            RouteBundle::Aggregate(v) if v == vec![collection_expose.clone()]
        );
    }
}
