// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability_source::{
            AggregateInstance, AggregateMember, AnonymizedAggregateCapabilityProvider,
            CapabilitySource, ComponentCapability, FilteredAggregateCapabilityProvider,
            FilteredAggregateCapabilityRouteData,
        },
        component_instance::{
            ComponentInstanceInterface, ExtendedInstanceInterface, ResolvedInstanceInterface,
            WeakComponentInstanceInterface,
        },
        error::RoutingError,
        legacy_router::{
            self, CapabilityVisitor, ErrorNotFoundFromParent, ErrorNotFoundInChild, ExposeVisitor,
            OfferVisitor, RouteBundle, Sources,
        },
        mapper::NoopRouteMapper,
    },
    async_trait::async_trait,
    cm_rust::{ExposeDecl, NameMapping, OfferDecl, OfferServiceDecl},
    cm_types::Name,
    derivative::Derivative,
    futures::future::BoxFuture,
    moniker::ChildName,
    std::collections::HashSet,
};

/// Provides capabilities exposed by an anonymized aggregates.
///
/// Given a set of collections and static children and the name of a capability, this provider
/// returns a list of children within them that expose the capability, and routes to a particular
/// child's exposed capability with that name.
///
/// This is used during collection routing from anonymized aggregate service instances.
#[derive(Derivative)]
#[derivative(Clone(bound = "V: Clone"))]
pub(super) struct AnonymizedAggregateServiceProvider<C: ComponentInstanceInterface, V> {
    /// Component that defines the aggregate.
    pub containing_component: WeakComponentInstanceInterface<C>,

    /// The members relative to `containing_component` that make up the aggregate.
    pub members: Vec<AggregateMember>,

    /// Name of the capability as exposed by children in the collection.
    pub capability_name: Name,

    pub capability_type: cm_rust::CapabilityTypeName,

    pub sources: Sources,
    pub visitor: V,
}

#[async_trait]
impl<C, V> AnonymizedAggregateCapabilityProvider<C> for AnonymizedAggregateServiceProvider<C, V>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    /// Returns a list of instances contributing capabilities to this provider.
    ///
    /// In the case of service capabilities, they are *not* instances inside that service, but
    /// rather service capabilities with the same name that are exposed by different components.
    async fn list_instances(&self) -> Result<Vec<AggregateInstance>, RoutingError> {
        let mut instances = Vec::new();
        let component = self.containing_component.upgrade()?;
        let mut child_components = vec![];
        let mut parent = None;
        {
            let resolved_state = component.lock_resolved_state().await?;
            for s in &self.members {
                match s {
                    AggregateMember::Child(child_name) => {
                        if let Some(child) = resolved_state.get_child(&child_name) {
                            child_components.push((child_name.clone(), child.clone()));
                        }
                    }
                    AggregateMember::Collection(collection) => {
                        child_components.extend(resolved_state.children_in_collection(&collection));
                    }
                    AggregateMember::Parent => {
                        if let Ok(p) = component.try_get_parent() {
                            match p {
                                ExtendedInstanceInterface::AboveRoot(_) => {
                                    // Services from above parent are not supported.
                                }
                                ExtendedInstanceInterface::Component(p) => {
                                    parent = Some(p);
                                }
                            }
                        }
                    }
                }
            }
        }
        for (child_name, child_component) in child_components {
            let child_exposes = child_component.lock_resolved_state().await.map(|c| c.exposes());
            match child_exposes {
                Ok(child_exposes) => {
                    if let Some(_) = legacy_router::find_matching_exposes(
                        self.capability_type,
                        &self.capability_name,
                        &child_exposes,
                    ) {
                        instances.push(AggregateInstance::Child(child_name.clone()));
                    }
                }
                // Ignore errors. One misbehaving component should not affect the entire collection.
                Err(_) => {}
            }
        }
        if let Some(parent) = parent {
            let parent_offers = parent.lock_resolved_state().await.map(|c| c.offers());
            match parent_offers {
                Ok(parent_offers) => {
                    let child_moniker = component.child_moniker().expect("ChildName should exist");
                    if let Some(_) = legacy_router::find_matching_offers(
                        self.capability_type,
                        &self.capability_name,
                        &child_moniker,
                        &parent_offers,
                    ) {
                        instances.push(AggregateInstance::Parent);
                    }
                }
                // Ignore errors. One misbehaving component should not affect the entire collection.
                Err(_) => {}
            }
        }
        Ok(instances)
    }

    /// Returns a `CapabilitySource` to a capability exposed by a child.
    ///
    /// `instance` is the name of the child that exposes the capability, as returned by
    /// `list_instances`.
    async fn route_instance(
        &self,
        instance: &AggregateInstance,
    ) -> Result<CapabilitySource<C>, RoutingError> {
        match instance {
            AggregateInstance::Child(name) => self.route_child_instance(&name).await,
            AggregateInstance::Parent => self.route_parent_instance().await,
        }
    }

    fn clone_boxed(&self) -> Box<dyn AnonymizedAggregateCapabilityProvider<C>> {
        Box::new(self.clone())
    }
}

impl<C, V> AnonymizedAggregateServiceProvider<C, V>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor,
    V: ExposeVisitor,
    V: CapabilityVisitor,
    V: Clone + Send + Sync + 'static,
{
    async fn route_child_instance(
        &self,
        instance: &ChildName,
    ) -> Result<CapabilitySource<C>, RoutingError> {
        let containing_component = self.containing_component.upgrade()?;
        let child_component =
            containing_component.lock_resolved_state().await?.get_child(instance).ok_or_else(
                || RoutingError::OfferFromChildInstanceNotFound {
                    child_moniker: instance.clone(),
                    moniker: containing_component.moniker().clone(),
                    capability_id: self.capability_name.clone().into(),
                },
            )?;

        let child_exposes = child_component.lock_resolved_state().await?.exposes();
        let child_exposes = legacy_router::find_matching_exposes(
            self.capability_type,
            &self.capability_name,
            &child_exposes,
        )
        .ok_or_else(|| {
            ExposeDecl::error_not_found_in_child(
                containing_component.moniker().clone(),
                instance.clone(),
                self.capability_name.clone(),
            )
        })?;
        legacy_router::route_from_expose(
            child_exposes,
            child_component,
            self.sources.clone(),
            &mut self.visitor.clone(),
            &mut NoopRouteMapper,
        )
        .await
    }

    async fn route_parent_instance(&self) -> Result<CapabilitySource<C>, RoutingError> {
        let containing_component = self.containing_component.upgrade()?;
        let parent = match containing_component.try_get_parent().map_err(|_| {
            RoutingError::OfferFromParentNotFound {
                moniker: containing_component.moniker().clone(),
                capability_id: self.capability_name.clone().into(),
            }
        })? {
            ExtendedInstanceInterface::AboveRoot(_) => {
                return Err(RoutingError::unsupported_route_source("service above parent"));
            }
            ExtendedInstanceInterface::Component(p) => p,
        };

        let child_moniker = containing_component.child_moniker().expect("ChildName should exist");
        let parent_offers = parent.lock_resolved_state().await?.offers();
        let parent_offers = legacy_router::find_matching_offers(
            self.capability_type,
            &self.capability_name,
            &child_moniker,
            &parent_offers,
        )
        .ok_or_else(|| {
            OfferDecl::error_not_found_from_parent(
                containing_component.moniker().clone(),
                self.capability_name.clone(),
            )
        })?;
        legacy_router::route_from_offer(
            parent_offers,
            parent,
            self.sources.clone(),
            &mut self.visitor.clone(),
            &mut NoopRouteMapper,
        )
        .await
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub(super) struct OfferFilteredServiceProvider<C: ComponentInstanceInterface> {
    /// Component that offered the filtered service
    component: WeakComponentInstanceInterface<C>,
    /// The service capability
    capability: ComponentCapability,
    /// The service offer that has a filter.
    offer_decl: OfferServiceDecl,
}

impl<C> OfferFilteredServiceProvider<C>
where
    C: ComponentInstanceInterface + 'static,
{
    pub(super) fn new(
        offer_decl: OfferServiceDecl,
        component: WeakComponentInstanceInterface<C>,
        capability: ComponentCapability,
    ) -> Self {
        Self { offer_decl, component, capability }
    }
}

impl<C> FilteredAggregateCapabilityProvider<C> for OfferFilteredServiceProvider<C>
where
    C: ComponentInstanceInterface + 'static,
{
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData<C>, RoutingError>>> {
        let capability_source = CapabilitySource::Component {
            capability: self.capability.clone(),
            component: self.component.clone(),
        };
        let instance_filter = get_instance_filter(&self.offer_decl);
        let fut = async move {
            Ok(FilteredAggregateCapabilityRouteData { capability_source, instance_filter })
        };
        // Without the explicit type, this does not compile
        let mut out: Vec<
            BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData<C>, RoutingError>>,
        > = vec![];
        out.push(Box::pin(fut));
        out
    }

    fn clone_boxed(&self) -> Box<dyn FilteredAggregateCapabilityProvider<C>> {
        Box::new(self.clone())
    }
}

fn get_instance_filter(offer_decl: &OfferServiceDecl) -> Vec<NameMapping> {
    let renamed_instances = offer_decl.renamed_instances.as_ref().unwrap_or_else(|| {
        static EMPTY_VEC: Vec<NameMapping> = vec![];
        &EMPTY_VEC
    });
    if !renamed_instances.is_empty() {
        let source_instance_filter: HashSet<_> = offer_decl
            .source_instance_filter
            .as_ref()
            .unwrap_or_else(|| {
                static EMPTY_VEC: Vec<String> = vec![];
                &EMPTY_VEC
            })
            .iter()
            .map(|s| s.as_str())
            .collect();
        renamed_instances
            .clone()
            .into_iter()
            .filter_map(|m| {
                if source_instance_filter.is_empty()
                    || source_instance_filter.contains(&m.target_name.as_str())
                {
                    Some(NameMapping { source_name: m.source_name, target_name: m.target_name })
                } else {
                    None
                }
            })
            .collect()
    } else {
        offer_decl
            .source_instance_filter
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|n| NameMapping { source_name: n.clone(), target_name: n })
            .collect()
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = "V: Clone"))]
pub(super) struct OfferAggregateServiceProvider<C: ComponentInstanceInterface, V> {
    /// Component that offered the aggregate service
    component: WeakComponentInstanceInterface<C>,

    /// List of offer decl to follow for routing each service provider used in the overall aggregation
    offer_decls: Vec<OfferServiceDecl>,

    sources: Sources,
    visitor: V,
}

impl<C, V> OfferAggregateServiceProvider<C, V>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor + ExposeVisitor + CapabilityVisitor,
    V: Send + Sync + Clone + 'static,
{
    pub(super) fn new(
        offer_decls: Vec<OfferServiceDecl>,
        component: WeakComponentInstanceInterface<C>,
        sources: Sources,
        visitor: V,
    ) -> Self {
        Self { offer_decls, sources, visitor, component }
    }
}

impl<C, V> FilteredAggregateCapabilityProvider<C> for OfferAggregateServiceProvider<C, V>
where
    C: ComponentInstanceInterface + 'static,
    V: OfferVisitor + ExposeVisitor + CapabilityVisitor,
    V: Send + Sync + Clone + 'static,
{
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData<C>, RoutingError>>> {
        // Without the explicit type, this does not compile
        let mut out: Vec<
            BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData<C>, RoutingError>>,
        > = vec![];
        for offer_decl in &self.offer_decls {
            let instance_filter = get_instance_filter(offer_decl);
            if instance_filter.is_empty() {
                continue;
            }
            let mut offer_decl = offer_decl.clone();
            offer_decl.source_instance_filter = None;
            offer_decl.renamed_instances = None;
            let offer_decl = OfferDecl::Service(offer_decl);
            let fut = async {
                let component = self.component.upgrade().map_err(|e| {
                    RoutingError::unsupported_route_source(format!(
                        "error upgrading aggregation point component {}",
                        e
                    ))
                })?;
                let capability_source = legacy_router::route_from_offer(
                    RouteBundle::from_offer(offer_decl),
                    component,
                    self.sources.clone(),
                    &mut self.visitor.clone(),
                    &mut NoopRouteMapper,
                )
                .await?;
                Ok(FilteredAggregateCapabilityRouteData { capability_source, instance_filter })
            };
            out.push(Box::pin(fut));
        }
        out
    }

    fn clone_boxed(&self) -> Box<dyn FilteredAggregateCapabilityProvider<C>> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_rust::{Availability, ChildRef, OfferSource, OfferTarget};

    #[test]
    fn test_get_instance_filter() {
        fn get_instance_filter(
            source_instance_filter: Option<Vec<String>>,
            renamed_instances: Option<Vec<NameMapping>>,
        ) -> Vec<NameMapping> {
            super::get_instance_filter(&OfferServiceDecl {
                source: OfferSource::Parent,
                source_name: "foo".parse().unwrap(),
                source_dictionary: None,
                target: OfferTarget::Child(ChildRef { name: "a".into(), collection: None }),
                target_name: "bar".parse().unwrap(),
                source_instance_filter,
                renamed_instances,
                availability: Availability::Required,
            })
        }

        assert_eq!(get_instance_filter(None, None), vec![]);
        assert_eq!(get_instance_filter(Some(vec![]), Some(vec![])), vec![]);
        let same_name_map = vec![
            NameMapping { source_name: "a".into(), target_name: "a".into() },
            NameMapping { source_name: "b".into(), target_name: "b".into() },
        ];
        assert_eq!(get_instance_filter(Some(vec!["a".into(), "b".into()]), None), same_name_map);
        assert_eq!(
            get_instance_filter(Some(vec!["a".into(), "b".into()]), Some(vec![])),
            same_name_map
        );
        let renamed_map = vec![
            NameMapping { source_name: "one".into(), target_name: "a".into() },
            NameMapping { source_name: "two".into(), target_name: "b".into() },
        ];
        assert_eq!(get_instance_filter(None, Some(renamed_map.clone())), renamed_map);
        assert_eq!(get_instance_filter(Some(vec![]), Some(renamed_map.clone())), renamed_map);
        assert_eq!(
            get_instance_filter(Some(vec!["b".into()]), Some(renamed_map.clone())),
            vec![NameMapping { source_name: "two".into(), target_name: "b".into() }]
        );
    }
}
