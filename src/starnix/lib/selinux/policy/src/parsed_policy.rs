// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::arrays::{RoleAllow, RoleTransition};

use super::{
    arrays::{
        AccessVectors, ConditionalNodes, Context, DeprecatedFilenameTransitions,
        FilenameTransitionList, FilenameTransitions, FsUses, GenericFsContexts, IPv6Nodes,
        InfinitiBandEndPorts, InfinitiBandPartitionKeys, InitialSids, NamedContextPairs, Nodes,
        Ports, RangeTransitions, RoleAllows, RoleTransitions, SimpleArray,
        MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY,
    },
    error::{ParseError, QueryError},
    extensible_bitmap::ExtensibleBitmap,
    metadata::{Config, Counts, HandleUnknown, Magic, PolicyVersion, Signature},
    parser::ParseStrategy,
    symbols::{
        find_class_by_name, find_class_permission_by_name, Category, Class, Classes, CommonSymbol,
        CommonSymbols, ConditionalBoolean, Permission, Role, Sensitivity, SymbolList, Type, User,
    },
    AccessVector, CategoryId, Parse, RoleId, SensitivityId, TypeId, UserId, Validate,
};

use anyhow::Context as _;
use std::fmt::Debug;
use zerocopy::little_endian as le;

/// A parsed binary policy.
#[derive(Debug)]
pub struct ParsedPolicy<PS: ParseStrategy> {
    /// A distinctive number that acts as a binary format-specific header for SELinux binary policy
    /// files.
    magic: PS::Output<Magic>,
    /// A length-encoded string, "SE Linux", which identifies this policy as an SE Linux policy.
    signature: Signature<PS>,
    /// The policy format version number. Different version may support different policy features.
    policy_version: PS::Output<PolicyVersion>,
    /// Whole-policy configuration, such as how to handle queries against unknown classes.
    config: Config<PS>,
    /// High-level counts of subsequent policy elements.
    counts: PS::Output<Counts>,
    policy_capabilities: ExtensibleBitmap<PS>,
    permissive_map: ExtensibleBitmap<PS>,
    /// Common permissions that can be mixed in to classes.
    common_symbols: SymbolList<PS, CommonSymbol<PS>>,
    /// The set of classes referenced by this policy.
    classes: SymbolList<PS, Class<PS>>,
    /// The set of roles referenced by this policy.
    roles: SymbolList<PS, Role<PS>>,
    /// The set of types referenced by this policy.
    types: SymbolList<PS, Type<PS>>,
    /// The set of users referenced by this policy.
    users: SymbolList<PS, User<PS>>,
    /// The set of dynamically adjustable booleans referenced by this policy.
    conditional_booleans: SymbolList<PS, ConditionalBoolean<PS>>,
    /// The set of sensitivity levels referenced by this policy.
    sensitivities: SymbolList<PS, Sensitivity<PS>>,
    /// The set of categories referenced by this policy.
    categories: SymbolList<PS, Category<PS>>,
    /// The set of access vectors referenced by this policy.
    access_vectors: SimpleArray<PS, AccessVectors<PS>>,
    conditional_lists: SimpleArray<PS, ConditionalNodes<PS>>,
    /// The set of role transitions to apply when instantiating new objects.
    role_transitions: RoleTransitions<PS>,
    /// The set of role transitions allowed by policy.
    role_allowlist: RoleAllows<PS>,
    filename_transition_list: FilenameTransitionList<PS>,
    initial_sids: SimpleArray<PS, InitialSids<PS>>,
    filesystems: SimpleArray<PS, NamedContextPairs<PS>>,
    ports: SimpleArray<PS, Ports<PS>>,
    network_interfaces: SimpleArray<PS, NamedContextPairs<PS>>,
    nodes: SimpleArray<PS, Nodes<PS>>,
    fs_uses: SimpleArray<PS, FsUses<PS>>,
    ipv6_nodes: SimpleArray<PS, IPv6Nodes<PS>>,
    infinitiband_partition_keys: Option<SimpleArray<PS, InfinitiBandPartitionKeys<PS>>>,
    infinitiband_end_ports: Option<SimpleArray<PS, InfinitiBandEndPorts<PS>>>,
    generic_fs_contexts: SimpleArray<PS, GenericFsContexts<PS>>,
    range_transitions: SimpleArray<PS, RangeTransitions<PS>>,
    /// Extensible bitmaps that encode associations between types and attributes.
    attribute_maps: Vec<ExtensibleBitmap<PS>>,
}

impl<PS: ParseStrategy> ParsedPolicy<PS> {
    /// The policy version stored in the underlying binary policy.
    pub fn policy_version(&self) -> u32 {
        PS::deref(&self.policy_version).policy_version()
    }

    /// The way "unknown" policy decisions should be handed according to the underlying binary
    /// policy.
    pub fn handle_unknown(&self) -> &HandleUnknown {
        self.config.handle_unknown()
    }

    /// Returns whether the input types are explicitly granted the permission named
    /// `permission_name` via an `allow [...];` policy statement, or an error if looking up the
    /// input types fails. This is the "custom" form of this API because `permission_name` is
    /// associated with a [`selinux_common::AbstractPermission::Custom::permission`] value.
    pub fn is_explicitly_allowed_custom(
        &self,
        source_type: &TypeId,
        target_type: &TypeId,
        target_class_name: &str,
        permission_name: &str,
    ) -> Result<bool, QueryError> {
        let target_class = find_class_by_name(self.classes(), target_class_name)
            .ok_or_else(|| QueryError::UnknownClass { class_name: target_class_name.to_owned() })?;
        let permission =
            find_class_permission_by_name(&self.common_symbols.data, target_class, permission_name)
                .ok_or_else(|| QueryError::UnknownPermission {
                    class_name: target_class_name.to_owned(),
                    permission_name: permission_name.to_owned(),
                })?;
        self.class_permission_is_explicitly_allowed(
            source_type,
            target_type,
            target_class,
            permission,
        )
    }

    /// Returns whether the input is explicitly allowed by some
    /// `allow [source_type_name] [target_type_name] : [target_class] [permission];` policy
    /// statement, or an error if lookups for input values fail.
    pub(crate) fn class_permission_is_explicitly_allowed(
        &self,
        source_type: &TypeId,
        target_type: &TypeId,
        target_class: &Class<PS>,
        permission: &Permission<PS>,
    ) -> Result<bool, QueryError> {
        let permission_id = permission.id();
        let permission_bit = (1 as u32) << (permission_id - 1);

        let target_class_id = target_class.id();

        for access_vector in self.access_vectors.data.iter() {
            // Concern ourselves only with explicit `allow [...];` policy statements.
            if !access_vector.is_allow() {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type]:[class] [...];`
            // policy statements where `[class]` matches `target_class_id`.
            if access_vector.target_class() != target_class_id {
                continue;
            }

            // Concern ourselves only with
            // `allow [source-type] [target-type]:[class] { [permissions] };` policy statements
            // where `permission_bit` refers to one of `[permissions]`.
            match access_vector.permission_mask() {
                Some(mask) => {
                    if (mask.get() & permission_bit) == 0 {
                        continue;
                    }
                }
                None => continue,
            }

            // Note: Perform bitmap lookups last: they are the most expensive comparison operation.

            // Note: Type ids start at 1, but are 0-indexed in bitmaps: hence the `type - 1` bitmap
            // lookups below.

            // Concern ourselves only with `allow [source-type] [...];` policy statements where
            // `[source-type]` is associated with `source_type_id`.
            let source_attribute_bitmap: &ExtensibleBitmap<PS> =
                &self.attribute_maps[(source_type.0.get() - 1) as usize];
            if !source_attribute_bitmap.is_set(access_vector.source_type().0.get() - 1) {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type][...];` policy
            // statements where `[target-type]` is associated with `target_type_id`.
            let target_attribute_bitmap: &ExtensibleBitmap<PS> =
                &self.attribute_maps[(target_type.0.get() - 1) as usize];
            if !target_attribute_bitmap.is_set(access_vector.target_type().0.get() - 1) {
                continue;
            }

            // `access_vector` explicitly allows the source, target, permission in this query.
            return Ok(true);
        }

        // Failed to find any explicit-allow access vector for this source, target, permission
        // query.
        Ok(false)
    }

    /// Computes the access vector that associates type `source_type_name` and `target_type_name`
    /// via an explicit `allow [...];` statement in the binary policy. Computes `AccessVector::NONE`
    /// if no such statement exists. This is the "custom" form of this API because
    /// `target_class_name` is associated with a [`selinux_common::AbstractObjectClass::Custom`]
    /// value.
    pub fn compute_explicitly_allowed_custom(
        &self,
        source_type_name: &TypeId,
        target_type_name: &TypeId,
        target_class_name: &str,
    ) -> Result<AccessVector, QueryError> {
        let target_class = find_class_by_name(self.classes(), target_class_name)
            .ok_or_else(|| QueryError::UnknownClass { class_name: target_class_name.to_owned() })?;
        self.compute_explicitly_allowed(source_type_name, target_type_name, target_class)
    }

    /// Computes the access vector that associates type `source_type_name` and `target_type_name`
    /// via an explicit `allow [...];` statement in the binary policy. Computes `AccessVector::NONE`
    /// if no such statement exists.
    pub(crate) fn compute_explicitly_allowed(
        &self,
        source_type: &TypeId,
        target_type: &TypeId,
        target_class: &Class<PS>,
    ) -> Result<AccessVector, QueryError> {
        let target_class_id = target_class.id();

        let mut computed_access_vector = AccessVector::NONE;
        for access_vector in self.access_vectors.data.iter() {
            // Concern ourselves only with explicit `allow [...];` policy statements.
            if !access_vector.is_allow() {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type]:[class] [...];`
            // policy statements where `[class]` matches `target_class_id`.
            if access_vector.target_class() != target_class_id {
                continue;
            }

            // Note: Perform bitmap lookups last: they are the most expensive comparison operation.

            // Note: Type ids start at 1, but are 0-indexed in bitmaps: hence the `type - 1` bitmap
            // lookups below.

            // Concern ourselves only with `allow [source-type] [...];` policy statements where
            // `[source-type]` is associated with `source_type_id`.
            let source_attribute_bitmap: &ExtensibleBitmap<PS> =
                &self.attribute_maps[(source_type.0.get() - 1) as usize];
            if !source_attribute_bitmap.is_set(access_vector.source_type().0.get() - 1) {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type][...];` policy
            // statements where `[target-type]` is associated with `target_type_id`.
            let target_attribute_bitmap: &ExtensibleBitmap<PS> =
                &self.attribute_maps[(target_type.0.get() - 1) as usize];
            if !target_attribute_bitmap.is_set(access_vector.target_type().0.get() - 1) {
                continue;
            }

            // Multiple attributes may be associated with source/target types. Accumulate
            // explicitly allowed permissions into `computed_access_vector`.
            if let Some(permission_mask) = access_vector.permission_mask() {
                computed_access_vector |= AccessVector::from_raw(permission_mask.get());
            }
        }

        // Failed to find any explicit-allow access vector for this source, target, class query.
        Ok(computed_access_vector)
    }

    /// Returns the policy entry for the specified initial Security Context.
    /// `id` must correspond to one of the `InitialSid` ids.
    pub(crate) fn initial_context(&self, id: le::U32) -> Option<&Context<PS>> {
        Some(&self.initial_sids.data.iter().find(|initial| initial.id() == id)?.context())
    }

    /// Returns the `User` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(crate) fn user(&self, id: UserId) -> &User<PS> {
        self.users.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named user, if present in the policy.
    pub(crate) fn user_by_name(&self, name: &str) -> Option<&User<PS>> {
        self.users.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the `Role` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(crate) fn role(&self, id: RoleId) -> &Role<PS> {
        self.roles.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named role, if present in the policy.
    pub(crate) fn role_by_name(&self, name: &str) -> Option<&Role<PS>> {
        self.roles.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the `Type` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(crate) fn type_(&self, id: TypeId) -> &Type<PS> {
        self.types.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named type, if present in the policy.
    pub(crate) fn type_by_name(&self, name: &str) -> Option<&Type<PS>> {
        self.types.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the `Sensitivity` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(crate) fn sensitivity(&self, id: SensitivityId) -> &Sensitivity<PS> {
        self.sensitivities.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named sensitivity level, if present in the policy.
    pub(crate) fn sensitivity_by_name(&self, name: &str) -> Option<&Sensitivity<PS>> {
        self.sensitivities.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the `Category` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(crate) fn category(&self, id: CategoryId) -> &Category<PS> {
        self.categories.data.iter().find(|y| y.id() == id).unwrap()
    }

    /// Returns the named category, if present in the policy.
    pub(crate) fn category_by_name(&self, name: &str) -> Option<&Category<PS>> {
        self.categories.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    pub(crate) fn classes(&self) -> &Classes<PS> {
        &self.classes.data
    }

    pub(crate) fn common_symbols(&self) -> &CommonSymbols<PS> {
        &self.common_symbols.data
    }

    pub(crate) fn conditional_booleans(&self) -> &Vec<ConditionalBoolean<PS>> {
        &self.conditional_booleans.data
    }

    pub(crate) fn role_allowlist(&self) -> &[RoleAllow] {
        PS::deref_slice(&self.role_allowlist.data)
    }

    pub(crate) fn role_transitions(&self) -> &[RoleTransition] {
        PS::deref_slice(&self.role_transitions.data)
    }

    pub(crate) fn range_transitions(&self) -> &RangeTransitions<PS> {
        &self.range_transitions.data
    }

    pub(crate) fn access_vectors(&self) -> &AccessVectors<PS> {
        &self.access_vectors.data
    }

    #[cfg(feature = "selinux_policy_test_api")]
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        Validate::validate(self)
    }

    #[cfg(feature = "selinux_policy_test_api")]
    pub fn type_id_by_name(&self, name: &str) -> TypeId {
        self.type_by_name(name).unwrap().id()
    }
}

impl<PS: ParseStrategy> ParsedPolicy<PS>
where
    Self: Parse<PS>,
{
    /// Parses the binary policy stored in `bytes`. It is an error for `bytes` to have trailing
    /// bytes after policy parsing completes.
    pub(crate) fn parse(bytes: PS) -> Result<(Self, PS::Input), anyhow::Error> {
        let (policy, tail) =
            <ParsedPolicy<PS> as Parse<PS>>::parse(bytes).map_err(Into::<anyhow::Error>::into)?;
        let num_bytes = tail.len();
        if num_bytes > 0 {
            return Err(ParseError::TrailingBytes { num_bytes }.into());
        }
        Ok((policy, tail.into_inner()))
    }
}

/// Parse a data structure from a prefix of a [`ParseStrategy`].
impl<PS: ParseStrategy> Parse<PS> for ParsedPolicy<PS>
where
    Signature<PS>: Parse<PS>,
    ExtensibleBitmap<PS>: Parse<PS>,
    SymbolList<PS, CommonSymbol<PS>>: Parse<PS>,
    SymbolList<PS, Class<PS>>: Parse<PS>,
    SymbolList<PS, Role<PS>>: Parse<PS>,
    SymbolList<PS, Type<PS>>: Parse<PS>,
    SymbolList<PS, User<PS>>: Parse<PS>,
    SymbolList<PS, ConditionalBoolean<PS>>: Parse<PS>,
    SymbolList<PS, Sensitivity<PS>>: Parse<PS>,
    SymbolList<PS, Category<PS>>: Parse<PS>,
    SimpleArray<PS, AccessVectors<PS>>: Parse<PS>,
    SimpleArray<PS, ConditionalNodes<PS>>: Parse<PS>,
    RoleTransitions<PS>: Parse<PS>,
    RoleAllows<PS>: Parse<PS>,
    SimpleArray<PS, FilenameTransitions<PS>>: Parse<PS>,
    SimpleArray<PS, DeprecatedFilenameTransitions<PS>>: Parse<PS>,
    SimpleArray<PS, InitialSids<PS>>: Parse<PS>,
    SimpleArray<PS, NamedContextPairs<PS>>: Parse<PS>,
    SimpleArray<PS, Ports<PS>>: Parse<PS>,
    SimpleArray<PS, NamedContextPairs<PS>>: Parse<PS>,
    SimpleArray<PS, Nodes<PS>>: Parse<PS>,
    SimpleArray<PS, FsUses<PS>>: Parse<PS>,
    SimpleArray<PS, IPv6Nodes<PS>>: Parse<PS>,
    SimpleArray<PS, InfinitiBandPartitionKeys<PS>>: Parse<PS>,
    SimpleArray<PS, InfinitiBandEndPorts<PS>>: Parse<PS>,
    SimpleArray<PS, GenericFsContexts<PS>>: Parse<PS>,
    SimpleArray<PS, RangeTransitions<PS>>: Parse<PS>,
{
    /// A [`Policy`] may add context to underlying [`ParseError`] values.
    type Error = anyhow::Error;

    /// Parses an entire binary policy.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (magic, tail) = PS::parse::<Magic>(tail).context("parsing magic")?;

        let (signature, tail) = Signature::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing signature")?;

        let (policy_version, tail) =
            PS::parse::<PolicyVersion>(tail).context("parsing policy version")?;
        let policy_version_value = PS::deref(&policy_version).policy_version();

        let (config, tail) = Config::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing policy config")?;

        let (counts, tail) =
            PS::parse::<Counts>(tail).context("parsing high-level policy object counts")?;

        let (policy_capabilities, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing policy capabilities")?;

        let (permissive_map, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing permissive map")?;

        let (common_symbols, tail) = SymbolList::<PS, CommonSymbol<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing common symbols")?;

        let (classes, tail) = SymbolList::<PS, Class<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing classes")?;

        let (roles, tail) = SymbolList::<PS, Role<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing roles")?;

        let (types, tail) = SymbolList::<PS, Type<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing types")?;

        let (users, tail) = SymbolList::<PS, User<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing users")?;

        let (conditional_booleans, tail) = SymbolList::<PS, ConditionalBoolean<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional booleans")?;

        let (sensitivities, tail) = SymbolList::<PS, Sensitivity<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivites")?;

        let (categories, tail) = SymbolList::<PS, Category<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing categories")?;

        let (access_vectors, tail) = SimpleArray::<PS, AccessVectors<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing access vectors")?;

        let (conditional_lists, tail) = SimpleArray::<PS, ConditionalNodes<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional lists")?;

        let (role_transitions, tail) = RoleTransitions::<PS>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role transitions")?;

        let (role_allowlist, tail) = RoleAllows::<PS>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role allow rules")?;

        let (filename_transition_list, tail) = if policy_version_value >= 33 {
            let (filename_transition_list, tail) =
                SimpleArray::<PS, FilenameTransitions<PS>>::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing standard filename transitions")?;
            (FilenameTransitionList::PolicyVersionGeq33(filename_transition_list), tail)
        } else {
            let (filename_transition_list, tail) =
                SimpleArray::<PS, DeprecatedFilenameTransitions<PS>>::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing deprecated filename transitions")?;
            (FilenameTransitionList::PolicyVersionLeq32(filename_transition_list), tail)
        };

        let (initial_sids, tail) = SimpleArray::<PS, InitialSids<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing initial sids")?;

        let (filesystems, tail) = SimpleArray::<PS, NamedContextPairs<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filesystem contexts")?;

        let (ports, tail) = SimpleArray::<PS, Ports<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing ports")?;

        let (network_interfaces, tail) = SimpleArray::<PS, NamedContextPairs<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing network interfaces")?;

        let (nodes, tail) = SimpleArray::<PS, Nodes<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing nodes")?;

        let (fs_uses, tail) = SimpleArray::<PS, FsUses<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing fs uses")?;

        let (ipv6_nodes, tail) = SimpleArray::<PS, IPv6Nodes<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing ipv6 nodes")?;

        let (infinitiband_partition_keys, infinitiband_end_ports, tail) =
            if policy_version_value >= MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY {
                let (infinity_band_partition_keys, tail) =
                    SimpleArray::<PS, InfinitiBandPartitionKeys<PS>>::parse(tail)
                        .map_err(Into::<anyhow::Error>::into)
                        .context("parsing infiniti band partition keys")?;
                let (infinitiband_end_ports, tail) =
                    SimpleArray::<PS, InfinitiBandEndPorts<PS>>::parse(tail)
                        .map_err(Into::<anyhow::Error>::into)
                        .context("parsing infiniti band end ports")?;
                (Some(infinity_band_partition_keys), Some(infinitiband_end_ports), tail)
            } else {
                (None, None, tail)
            };

        let (generic_fs_contexts, tail) = SimpleArray::<PS, GenericFsContexts<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing generic filesystem contexts")?;

        let (range_transitions, tail) = SimpleArray::<PS, RangeTransitions<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing range transitions")?;

        let primary_names_count = PS::deref(&types.metadata).primary_names_count();
        let mut attribute_maps = Vec::with_capacity(primary_names_count as usize);
        let mut tail = tail;

        for i in 0..primary_names_count {
            let (item, next_tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .with_context(|| format!("parsing {}th attribtue map", i))?;
            attribute_maps.push(item);
            tail = next_tail;
        }
        let tail = tail;
        let attribute_maps = attribute_maps;

        Ok((
            Self {
                magic,
                signature,
                policy_version,
                config,
                counts,
                policy_capabilities,
                permissive_map,
                common_symbols,
                classes,
                roles,
                types,
                users,
                conditional_booleans,
                sensitivities,
                categories,
                access_vectors,
                conditional_lists,
                role_transitions,
                role_allowlist,
                filename_transition_list,
                initial_sids,
                filesystems,
                ports,
                network_interfaces,
                nodes,
                fs_uses,
                ipv6_nodes,
                infinitiband_partition_keys,
                infinitiband_end_ports,
                generic_fs_contexts,
                range_transitions,
                attribute_maps,
            },
            tail,
        ))
    }
}

impl<PS: ParseStrategy> Validate for ParsedPolicy<PS> {
    /// A [`Policy`] may add context to underlying [`ValidateError`] values.
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        PS::deref(&self.magic)
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating magic")?;
        self.signature
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating signature")?;
        PS::deref(&self.policy_version)
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating policy_version")?;
        self.config.validate().map_err(Into::<anyhow::Error>::into).context("validating config")?;
        PS::deref(&self.counts)
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating counts")?;
        self.policy_capabilities
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating policy_capabilities")?;
        self.permissive_map
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating permissive_map")?;
        self.common_symbols
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating common_symbols")?;
        self.classes
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating classes")?;
        self.roles.validate().map_err(Into::<anyhow::Error>::into).context("validating roles")?;
        self.types.validate().map_err(Into::<anyhow::Error>::into).context("validating types")?;
        self.users.validate().map_err(Into::<anyhow::Error>::into).context("validating users")?;
        self.conditional_booleans
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating conditional_booleans")?;
        self.sensitivities
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating sensitivities")?;
        self.categories
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating categories")?;
        self.access_vectors
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating access_vectors")?;
        self.conditional_lists
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating conditional_lists")?;
        self.role_transitions
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating role_transitions")?;
        self.role_allowlist
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating role_allowlist")?;
        self.filename_transition_list
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating filename_transition_list")?;
        self.initial_sids
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating initial_sids")?;
        self.filesystems
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating filesystems")?;
        self.ports.validate().map_err(Into::<anyhow::Error>::into).context("validating ports")?;
        self.network_interfaces
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating network_interfaces")?;
        self.nodes.validate().map_err(Into::<anyhow::Error>::into).context("validating nodes")?;
        self.fs_uses
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating fs_uses")?;
        self.ipv6_nodes
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating ipv6 nodes")?;
        self.infinitiband_partition_keys
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating infinitiband_partition_keys")?;
        self.infinitiband_end_ports
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating infinitiband_end_ports")?;
        self.generic_fs_contexts
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating generic_fs_contexts")?;
        self.range_transitions
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating range_transitions")?;
        self.attribute_maps
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating attribute_maps")?;

        Ok(())
    }
}
