// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    assert_matches::assert_matches,
    cm_rust::{CapabilityTypeName, ComponentDecl, FidlIntoNative},
    cm_types::{Name, Path, RelativePath},
    derivative::Derivative,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio,
    std::{collections::BTreeMap, path::PathBuf},
};

/// Name of the test runner.
///
/// Several functions assume the existence of a runner with this name.
pub const TEST_RUNNER_NAME: &str = "test_runner";

/// Deserialize `object` into a cml::Document and then translate the result
/// to ComponentDecl.
pub fn new_decl_from_json(object: serde_json::Value) -> Result<ComponentDecl, Error> {
    let doc = serde_json::from_value(object).context("failed to deserialize manifest")?;
    let cm =
        cml::compile(&doc, cml::CompileOptions::default()).context("failed to compile manifest")?;
    Ok(cm.fidl_into_native())
}

/// Builder for constructing a ComponentDecl.
#[derive(Debug, Clone)]
pub struct ComponentDeclBuilder {
    result: ComponentDecl,
}

impl ComponentDeclBuilder {
    /// An empty ComponentDeclBuilder, with no program.
    pub fn new_empty_component() -> Self {
        ComponentDeclBuilder { result: Default::default() }
    }

    /// A ComponentDeclBuilder prefilled with a program and using a runner named "test_runner",
    /// which we assume is offered to us.
    pub fn new() -> Self {
        Self::new_empty_component().add_program(TEST_RUNNER_NAME)
    }

    /// Add a child element.
    pub fn child(mut self, decl: impl Into<cm_rust::ChildDecl>) -> Self {
        self.result.children.push(decl.into());
        self
    }

    /// Add a child with default properties.
    pub fn child_default(self, name: &str) -> Self {
        self.child(ChildBuilder::new().name(name))
    }

    // Add a collection element.
    pub fn collection(mut self, decl: impl Into<cm_rust::CollectionDecl>) -> Self {
        self.result.collections.push(decl.into());
        self
    }

    /// Add a collection with default properties.
    pub fn collection_default(self, name: &str) -> Self {
        self.collection(CollectionBuilder::new().name(name))
    }

    /// Add a "program" clause, using the given runner.
    pub fn add_program(mut self, runner: &str) -> Self {
        assert!(self.result.program.is_none(), "tried to add program twice");
        self.result.program = Some(cm_rust::ProgramDecl {
            runner: Some(runner.parse().unwrap()),
            info: fdata::Dictionary { entries: Some(vec![]), ..Default::default() },
        });
        self
    }

    /// Add a custom offer.
    pub fn offer(mut self, offer: cm_rust::OfferDecl) -> Self {
        self.result.offers.push(offer);
        self
    }

    /// Add a custom expose.
    pub fn expose(mut self, expose: cm_rust::ExposeDecl) -> Self {
        self.result.exposes.push(expose);
        self
    }

    /// Add a custom use decl.
    pub fn use_(mut self, use_: impl Into<cm_rust::UseDecl>) -> Self {
        self.result.uses.push(use_.into());
        self
    }

    // Add a use decl for fuchsia.component.Realm.
    pub fn use_realm(mut self) -> Self {
        let use_ = cm_rust::UseDecl::Protocol(cm_rust::UseProtocolDecl {
            dependency_type: cm_rust::DependencyType::Strong,
            source: cm_rust::UseSource::Framework,
            source_name: "fuchsia.component.Realm".parse().unwrap(),
            source_dictionary: None,
            target_path: "/svc/fuchsia.component.Realm".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        });
        self.result.uses.push(use_);
        self
    }

    /// Add a capability declaration.
    pub fn capability(mut self, capability: impl Into<cm_rust::CapabilityDecl>) -> Self {
        self.result.capabilities.push(capability.into());
        self
    }

    /// Add a default protocol declaration.
    pub fn protocol_default(self, name: &str) -> Self {
        self.capability(CapabilityBuilder::protocol().name(name))
    }

    /// Add a default dictionary declaration.
    pub fn dictionary_default(self, name: &str) -> Self {
        self.capability(CapabilityBuilder::dictionary().name(name))
    }

    /// Add a default runner declaration.
    pub fn runner_default(self, name: &str) -> Self {
        self.capability(CapabilityBuilder::runner().name(name))
    }

    /// Add a default resolver declaration.
    pub fn resolver_default(self, name: &str) -> Self {
        self.capability(CapabilityBuilder::resolver().name(name))
    }

    /// Add a default service declaration.
    pub fn service_default(self, name: &str) -> Self {
        self.capability(CapabilityBuilder::service().name(name))
    }

    /// Add an environment declaration.
    pub fn environment(mut self, environment: impl Into<cm_rust::EnvironmentDecl>) -> Self {
        self.result.environments.push(environment.into());
        self
    }

    /// Add a config declaration.
    pub fn config(mut self, config: cm_rust::ConfigDecl) -> Self {
        self.result.config = Some(config);
        self
    }

    /// Generate the final ComponentDecl.
    pub fn build(self) -> ComponentDecl {
        self.result
    }
}

/// A convenience builder for constructing ChildDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct ChildBuilder {
    name: Option<String>,
    url: Option<String>,
    #[derivative(Default(value = "fdecl::StartupMode::Lazy"))]
    startup: fdecl::StartupMode,
    on_terminate: Option<fdecl::OnTerminate>,
    environment: Option<String>,
}

impl ChildBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Defaults url to `"test:///{name}"`.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.into());
        if self.url.is_none() {
            self.url = Some(format!("test:///{name}"));
        }
        self
    }

    pub fn url(mut self, url: &str) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn startup(mut self, startup: fdecl::StartupMode) -> Self {
        self.startup = startup;
        self
    }

    pub fn eager(self) -> Self {
        self.startup(fdecl::StartupMode::Eager)
    }

    pub fn on_terminate(mut self, on_terminate: fdecl::OnTerminate) -> Self {
        self.on_terminate = Some(on_terminate);
        self
    }

    pub fn environment(mut self, environment: &str) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn build(self) -> cm_rust::ChildDecl {
        cm_rust::ChildDecl {
            name: self.name.expect("name not set"),
            url: self.url.expect("url not set"),
            startup: self.startup,
            on_terminate: self.on_terminate,
            environment: self.environment,
            config_overrides: None,
        }
    }
}

impl From<ChildBuilder> for cm_rust::ChildDecl {
    fn from(builder: ChildBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing CollectionDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct CollectionBuilder {
    name: Option<Name>,
    #[derivative(Default(value = "fdecl::Durability::Transient"))]
    durability: fdecl::Durability,
    environment: Option<String>,
    #[derivative(Default(value = "cm_types::AllowedOffers::StaticOnly"))]
    allowed_offers: cm_types::AllowedOffers,
    allow_long_names: bool,
    persistent_storage: Option<bool>,
}

impl CollectionBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        self
    }

    pub fn durability(mut self, durability: fdecl::Durability) -> Self {
        self.durability = durability;
        self
    }

    pub fn environment(mut self, environment: &str) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn allowed_offers(mut self, allowed_offers: cm_types::AllowedOffers) -> Self {
        self.allowed_offers = allowed_offers;
        self
    }

    pub fn allow_long_names(mut self) -> Self {
        self.allow_long_names = true;
        self
    }

    pub fn persistent_storage(mut self, persistent_storage: bool) -> Self {
        self.persistent_storage = Some(persistent_storage);
        self
    }

    pub fn build(self) -> cm_rust::CollectionDecl {
        cm_rust::CollectionDecl {
            name: self.name.expect("name not set"),
            durability: self.durability,
            environment: self.environment,
            allowed_offers: self.allowed_offers,
            allow_long_names: self.allow_long_names,
            persistent_storage: self.persistent_storage,
        }
    }
}

impl From<CollectionBuilder> for cm_rust::CollectionDecl {
    fn from(builder: CollectionBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing EnvironmentDecls.
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct EnvironmentBuilder {
    name: Option<String>,
    #[derivative(Default(value = "fdecl::EnvironmentExtends::Realm"))]
    extends: fdecl::EnvironmentExtends,
    runners: Vec<cm_rust::RunnerRegistration>,
    resolvers: Vec<cm_rust::ResolverRegistration>,
    debug_capabilities: Vec<cm_rust::DebugRegistration>,
    stop_timeout_ms: Option<u32>,
}

impl EnvironmentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn extends(mut self, extends: fdecl::EnvironmentExtends) -> Self {
        self.extends = extends;
        self
    }

    pub fn runner(mut self, runner: cm_rust::RunnerRegistration) -> Self {
        self.runners.push(runner);
        self
    }

    pub fn resolver(mut self, resolver: cm_rust::ResolverRegistration) -> Self {
        self.resolvers.push(resolver);
        self
    }

    pub fn debug(mut self, debug: cm_rust::DebugRegistration) -> Self {
        self.debug_capabilities.push(debug);
        self
    }

    pub fn stop_timeout(mut self, timeout_ms: u32) -> Self {
        self.stop_timeout_ms = Some(timeout_ms);
        self
    }

    pub fn build(self) -> cm_rust::EnvironmentDecl {
        cm_rust::EnvironmentDecl {
            name: self.name.expect("name not set"),
            extends: self.extends,
            runners: self.runners,
            resolvers: self.resolvers,
            debug_capabilities: self.debug_capabilities,
            stop_timeout_ms: self.stop_timeout_ms,
        }
    }
}

impl From<EnvironmentBuilder> for cm_rust::EnvironmentDecl {
    fn from(builder: EnvironmentBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing [CapabilityDecl]s.
///
/// To use, call the constructor matching their capability type ([CapabilityBuilder::protocol],
/// [CapabilityBuilder::directory], etc., and then call methods to set properties. When done,
/// call [CapabilityBuilder::build] (or [Into::into]) to generate the [CapabilityDecl].
#[derive(Debug)]
pub struct CapabilityBuilder {
    name: Option<Name>,
    type_: CapabilityTypeName,
    path: Option<Path>,
    dictionary_source: Option<cm_rust::DictionarySource>,
    source_dictionary: Option<RelativePath>,
    rights: fio::Operations,
    subdir: Option<PathBuf>,
    backing_dir: Option<Name>,
    storage_source: Option<cm_rust::StorageDirectorySource>,
    storage_id: fdecl::StorageId,
    value: Option<cm_rust::ConfigValue>,
}

impl CapabilityBuilder {
    pub fn protocol() -> Self {
        Self::new(CapabilityTypeName::Protocol)
    }

    pub fn service() -> Self {
        Self::new(CapabilityTypeName::Service)
    }

    pub fn directory() -> Self {
        Self::new(CapabilityTypeName::Directory)
    }

    pub fn storage() -> Self {
        Self::new(CapabilityTypeName::Storage)
    }

    pub fn runner() -> Self {
        Self::new(CapabilityTypeName::Runner)
    }

    pub fn resolver() -> Self {
        Self::new(CapabilityTypeName::Resolver)
    }

    pub fn dictionary() -> Self {
        Self::new(CapabilityTypeName::Dictionary)
    }

    pub fn config() -> Self {
        Self::new(CapabilityTypeName::Config)
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.parse().unwrap());
        if self.path.is_some() {
            return self;
        }
        match self.type_ {
            CapabilityTypeName::Protocol => {
                self.path = Some(format!("/svc/{name}").parse().unwrap());
            }
            CapabilityTypeName::Service => {
                self.path = Some(format!("/svc/{name}").parse().unwrap());
            }
            CapabilityTypeName::Runner => {
                self.path = Some("/svc/fuchsia.component.runner.ComponentRunner".parse().unwrap());
            }
            CapabilityTypeName::Resolver => {
                self.path = Some("/svc/fuchsia.component.resolution.Resolver".parse().unwrap());
            }
            CapabilityTypeName::Dictionary
            | CapabilityTypeName::Storage
            | CapabilityTypeName::Config
            | CapabilityTypeName::Directory => {}
            CapabilityTypeName::EventStream => unreachable!(),
        }
        self
    }

    fn new(type_: CapabilityTypeName) -> Self {
        Self {
            type_,
            name: None,
            path: None,
            dictionary_source: None,
            source_dictionary: None,
            rights: fio::R_STAR_DIR,
            subdir: None,
            backing_dir: None,
            storage_source: None,
            storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
            value: None,
        }
    }

    pub fn path(mut self, path: &str) -> Self {
        assert_matches!(
            self.type_,
            CapabilityTypeName::Protocol
                | CapabilityTypeName::Service
                | CapabilityTypeName::Directory
                | CapabilityTypeName::Runner
                | CapabilityTypeName::Resolver
        );
        self.path = Some(path.parse().unwrap());
        self
    }

    pub fn rights(mut self, rights: fio::Operations) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Directory);
        self.rights = rights;
        self
    }

    pub fn source_dictionary(
        mut self,
        source: cm_rust::DictionarySource,
        source_dictionary: &str,
    ) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Dictionary);
        self.dictionary_source = Some(source);
        self.source_dictionary = Some(source_dictionary.parse().unwrap());
        self
    }

    pub fn backing_dir(mut self, backing_dir: &str) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Storage);
        self.backing_dir = Some(backing_dir.parse().unwrap());
        self
    }

    pub fn value(mut self, value: cm_rust::ConfigValue) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Config);
        self.value = Some(value);
        self
    }

    pub fn source(mut self, source: cm_rust::StorageDirectorySource) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Storage);
        self.storage_source = Some(source);
        self
    }

    pub fn subdir(mut self, subdir: &str) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Storage);
        self.subdir = Some(subdir.parse().unwrap());
        self
    }

    pub fn storage_id(mut self, storage_id: fdecl::StorageId) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Storage);
        self.storage_id = storage_id;
        self
    }

    pub fn build(self) -> cm_rust::CapabilityDecl {
        match self.type_ {
            CapabilityTypeName::Protocol => {
                cm_rust::CapabilityDecl::Protocol(cm_rust::ProtocolDecl {
                    name: self.name.expect("name not set"),
                    source_path: Some(self.path.expect("path not set")),
                })
            }
            CapabilityTypeName::Service => cm_rust::CapabilityDecl::Service(cm_rust::ServiceDecl {
                name: self.name.expect("name not set"),
                source_path: Some(self.path.expect("path not set")),
            }),
            CapabilityTypeName::Runner => cm_rust::CapabilityDecl::Runner(cm_rust::RunnerDecl {
                name: self.name.expect("name not set"),
                source_path: Some(self.path.expect("path not set")),
            }),
            CapabilityTypeName::Resolver => {
                cm_rust::CapabilityDecl::Resolver(cm_rust::ResolverDecl {
                    name: self.name.expect("name not set"),
                    source_path: Some(self.path.expect("path not set")),
                })
            }
            CapabilityTypeName::Dictionary => {
                cm_rust::CapabilityDecl::Dictionary(cm_rust::DictionaryDecl {
                    name: self.name.expect("name not set"),
                    source: self.dictionary_source,
                    source_dictionary: self.source_dictionary,
                })
            }
            CapabilityTypeName::Storage => cm_rust::CapabilityDecl::Storage(cm_rust::StorageDecl {
                name: self.name.expect("name not set"),
                backing_dir: self.backing_dir.expect("backing_dir not set"),
                source: self.storage_source.expect("source not set"),
                subdir: self.subdir,
                storage_id: self.storage_id,
            }),
            CapabilityTypeName::Directory => {
                cm_rust::CapabilityDecl::Directory(cm_rust::DirectoryDecl {
                    name: self.name.expect("name not set"),
                    source_path: Some(self.path.expect("path not set")),
                    rights: self.rights,
                })
            }
            CapabilityTypeName::Config => {
                cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: self.name.expect("name not set"),
                    value: self.value.expect("value not set"),
                })
            }
            CapabilityTypeName::EventStream => unreachable!(),
        }
    }
}

impl From<CapabilityBuilder> for cm_rust::CapabilityDecl {
    fn from(builder: CapabilityBuilder) -> Self {
        builder.build()
    }
}

/// A convenience builder for constructing [UseDecl]s.
///
/// To use, call the constructor matching their capability type ([UseBuilder::protocol],
/// [UseBuilder::directory], etc., and then call methods to set properties. When done,
/// call [UseBuilder::build] (or [Into::into]) to generate the [UseDecl].
#[derive(Debug)]
pub struct UseBuilder {
    source_name: Option<Name>,
    type_: CapabilityTypeName,
    source_dictionary: Option<RelativePath>,
    source: cm_rust::UseSource,
    target_name: Option<Name>,
    target_path: Option<Path>,
    dependency_type: cm_rust::DependencyType,
    availability: cm_rust::Availability,
    rights: fio::Operations,
    subdir: Option<PathBuf>,
    scope: Option<Vec<cm_rust::EventScope>>,
    filter: Option<BTreeMap<String, cm_rust::DictionaryValue>>,
}

impl UseBuilder {
    pub fn protocol() -> Self {
        Self::new(CapabilityTypeName::Protocol)
    }

    pub fn service() -> Self {
        Self::new(CapabilityTypeName::Service)
    }

    pub fn directory() -> Self {
        Self::new(CapabilityTypeName::Directory)
    }

    pub fn storage() -> Self {
        Self::new(CapabilityTypeName::Storage)
    }

    pub fn runner() -> Self {
        Self::new(CapabilityTypeName::Runner)
    }

    pub fn event_stream() -> Self {
        Self::new(CapabilityTypeName::EventStream)
    }

    pub fn config() -> Self {
        Self::new(CapabilityTypeName::Config)
    }

    fn new(type_: CapabilityTypeName) -> Self {
        Self {
            type_,
            source: cm_rust::UseSource::Parent,
            source_name: None,
            target_name: None,
            target_path: None,
            source_dictionary: None,
            rights: fio::R_STAR_DIR,
            subdir: None,
            dependency_type: cm_rust::DependencyType::Strong,
            availability: cm_rust::Availability::Required,
            scope: None,
            filter: None,
        }
    }

    pub fn name(mut self, name: &str) -> Self {
        self.source_name = Some(name.parse().unwrap());
        if self.target_path.is_some() || self.target_name.is_some() {
            return self;
        }
        match self.type_ {
            CapabilityTypeName::Protocol | CapabilityTypeName::Service => {
                self.target_path = Some(format!("/svc/{name}").parse().unwrap());
            }
            CapabilityTypeName::EventStream => {
                self.target_path = Some("/svc/fuchsia.component.EventStream".parse().unwrap());
            }
            CapabilityTypeName::Runner | CapabilityTypeName::Config => {
                self.target_name = self.source_name.clone();
            }
            CapabilityTypeName::Storage | CapabilityTypeName::Directory => {}
            CapabilityTypeName::Dictionary | CapabilityTypeName::Resolver => unreachable!(),
        }
        self
    }

    pub fn path(mut self, path: &str) -> Self {
        assert_matches!(
            self.type_,
            CapabilityTypeName::Protocol
                | CapabilityTypeName::Service
                | CapabilityTypeName::Directory
                | CapabilityTypeName::EventStream
                | CapabilityTypeName::Storage
        );
        self.target_path = Some(path.parse().unwrap());
        self
    }

    pub fn target_name(mut self, name: &str) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Runner | CapabilityTypeName::Config);
        self.target_name = Some(name.parse().unwrap());
        self
    }

    pub fn from_dictionary(mut self, dictionary: &str) -> Self {
        assert_matches!(
            self.type_,
            CapabilityTypeName::Service
                | CapabilityTypeName::Protocol
                | CapabilityTypeName::Directory
                | CapabilityTypeName::Runner
        );
        self.source_dictionary = Some(dictionary.parse().unwrap());
        self
    }

    pub fn source(mut self, source: cm_rust::UseSource) -> Self {
        assert_matches!(self.type_, t if t != CapabilityTypeName::Storage);
        self.source = source;
        self
    }

    pub fn availability(mut self, availability: cm_rust::Availability) -> Self {
        assert_matches!(
            self.type_,
            CapabilityTypeName::Protocol
                | CapabilityTypeName::Service
                | CapabilityTypeName::Directory
                | CapabilityTypeName::EventStream
                | CapabilityTypeName::Storage
                | CapabilityTypeName::Config
        );
        self.availability = availability;
        self
    }

    pub fn dependency(mut self, dependency: cm_rust::DependencyType) -> Self {
        assert_matches!(
            self.type_,
            CapabilityTypeName::Protocol
                | CapabilityTypeName::Service
                | CapabilityTypeName::Directory
        );
        self.dependency_type = dependency;
        self
    }

    pub fn rights(mut self, rights: fio::Operations) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Directory);
        self.rights = rights;
        self
    }

    pub fn subdir(mut self, subdir: &str) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::Directory);
        self.subdir = Some(subdir.parse().unwrap());
        self
    }

    pub fn scope(mut self, scope: Vec<cm_rust::EventScope>) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::EventStream);
        self.scope = Some(scope);
        self
    }

    pub fn filter(mut self, filter: BTreeMap<String, cm_rust::DictionaryValue>) -> Self {
        assert_matches!(self.type_, CapabilityTypeName::EventStream);
        self.filter = Some(filter);
        self
    }

    pub fn build(self) -> cm_rust::UseDecl {
        match self.type_ {
            CapabilityTypeName::Protocol => cm_rust::UseDecl::Protocol(cm_rust::UseProtocolDecl {
                source: self.source,
                source_name: self.source_name.expect("name not set"),
                source_dictionary: self.source_dictionary,
                target_path: self.target_path.expect("path not set"),
                dependency_type: self.dependency_type,
                availability: self.availability,
            }),
            CapabilityTypeName::Service => cm_rust::UseDecl::Service(cm_rust::UseServiceDecl {
                source: self.source,
                source_name: self.source_name.expect("name not set"),
                source_dictionary: self.source_dictionary,
                target_path: self.target_path.expect("path not set"),
                dependency_type: self.dependency_type,
                availability: self.availability,
            }),
            CapabilityTypeName::Directory => {
                cm_rust::UseDecl::Directory(cm_rust::UseDirectoryDecl {
                    source: self.source,
                    source_name: self.source_name.expect("name not set"),
                    source_dictionary: self.source_dictionary,
                    target_path: self.target_path.expect("path not set"),
                    rights: self.rights,
                    subdir: self.subdir,
                    dependency_type: self.dependency_type,
                    availability: self.availability,
                })
            }
            CapabilityTypeName::Storage => cm_rust::UseDecl::Storage(cm_rust::UseStorageDecl {
                source_name: self.source_name.expect("name not set"),
                target_path: self.target_path.expect("path not set"),
                availability: self.availability,
            }),
            CapabilityTypeName::EventStream => {
                cm_rust::UseDecl::EventStream(cm_rust::UseEventStreamDecl {
                    source: self.source,
                    source_name: self.source_name.expect("name not set"),
                    target_path: self.target_path.expect("path not set"),
                    availability: self.availability,
                    scope: self.scope,
                    filter: self.filter,
                })
            }
            CapabilityTypeName::Runner => cm_rust::UseDecl::Runner(cm_rust::UseRunnerDecl {
                source: self.source,
                source_name: self.source_name.expect("name not set"),
                source_dictionary: self.source_dictionary,
            }),
            CapabilityTypeName::Config => cm_rust::UseDecl::Config(cm_rust::UseConfigurationDecl {
                source: self.source,
                source_name: self.source_name.expect("name not set"),
                target_name: self.target_name.expect("target name not set"),
                availability: self.availability,
            }),
            CapabilityTypeName::Resolver | CapabilityTypeName::Dictionary => unreachable!(),
        }
    }
}

impl From<UseBuilder> for cm_rust::UseDecl {
    fn from(builder: UseBuilder) -> Self {
        builder.build()
    }
}
