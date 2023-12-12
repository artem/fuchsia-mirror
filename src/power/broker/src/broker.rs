// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, LeaseStatus, Permissions, PowerLevel,
};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use itertools::Itertools;
use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::Hash;
use uuid::Uuid;

use crate::credentials::*;
use crate::topology::*;

pub struct Broker {
    catalog: Catalog,
    credentials: Registry,
    // The current level for each element, as reported to the broker.
    current: SubscribeMap<ElementID, PowerLevel>,
    // The level for each element required by the topology.
    required: SubscribeMap<ElementID, PowerLevel>,
}

impl Broker {
    pub fn new() -> Self {
        Broker {
            catalog: Catalog::new(),
            credentials: Registry::new(),
            current: SubscribeMap::new(),
            required: SubscribeMap::new(),
        }
    }

    fn lookup_credentials(&self, token: Token) -> Option<Credential> {
        self.credentials.lookup(token)
    }

    fn unregister_all_credentials_for_element(&mut self, element_id: &ElementID) {
        self.credentials.unregister_all_for_element(element_id)
    }

    fn register_credential(
        &mut self,
        element_id: &ElementID,
        credential_to_register: CredentialToRegister,
    ) -> Result<(), RegisterCredentialsError> {
        self.credentials.register(element_id, credential_to_register)?;
        Ok(())
    }

    fn unregister_credential(&mut self, credential: &Credential) -> Option<Credential> {
        self.credentials.unregister(credential)
    }

    pub fn register_credentials(
        &mut self,
        token: Token,
        credentials_to_register: Vec<CredentialToRegister>,
    ) -> Result<(), RegisterCredentialsError> {
        let Some(credential) = self.lookup_credentials(token) else {
            tracing::debug!("register_credentials: no credential found matching token");
            return Err(RegisterCredentialsError::NotAuthorized);
        };
        if !credential.contains(Permissions::MODIFY_CREDENTIAL) {
            tracing::debug!(
                "register_credentials: credential missing MODIFY_CREDENTIAL: {:?}",
                &credential
            );
            return Err(RegisterCredentialsError::NotAuthorized);
        }
        for credential_to_register in credentials_to_register {
            if let Err(err) =
                self.register_credential(&credential.get_element(), credential_to_register)
            {
                tracing::debug!(
                    "register_credentials: register_credential({:?}) failed",
                    &credential.get_element(),
                );
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn unregister_credentials(
        &mut self,
        token: Token,
        tokens_to_unregister: Vec<Token>,
    ) -> Result<(), UnregisterCredentialsError> {
        let Some(credential) = self.lookup_credentials(token) else {
            tracing::debug!("unregister_credentials: no credential found matching token");
            return Err(UnregisterCredentialsError::NotAuthorized);
        };
        for token in tokens_to_unregister {
            let Some(credential_to_unregister) = self.lookup_credentials(token) else {
                continue;
            };
            if !credential
                .authorizes(credential_to_unregister.get_element(), &Permissions::MODIFY_CREDENTIAL)
            {
                tracing::debug!(
                    "unregister_credentials: credential({:?}) missing MODIFY_CREDENTIAL: {:?}",
                    &credential,
                    &credential_to_unregister,
                );
                return Err(UnregisterCredentialsError::NotAuthorized);
            }
            self.unregister_credential(&credential_to_unregister);
        }
        Ok(())
    }

    fn current_level_satisfies(&self, required: &ElementLevel) -> bool {
        self.current
            .get(&required.element_id)
            // If current level is unknown, required is not satisfied.
            .is_some_and(|current| current.satisfies(required.level))
    }

    pub fn update_current_level(
        &mut self,
        element_id: &ElementID,
        level: PowerLevel,
    ) -> Result<(), fpb::UpdateCurrentPowerLevelError> {
        tracing::debug!("update_current_level({:?}, {:?})", element_id, level);
        self.current.update(element_id, level);
        // Some previously pending claims may now be ready to be activated,
        // because the active claims upon which they depend may now be
        // satisfied.
        // Find active claims that are now satisfied by the new current level:
        let active_claims_for_required_element =
            self.catalog.active_claims.for_required_element(element_id);
        let claims_satisfied: Vec<&Claim> = active_claims_for_required_element
            .iter()
            .filter(|c| level.satisfies(c.requires().level))
            .collect();
        tracing::debug!("claims_satisfied = {:?})", &claims_satisfied);
        let mut leases_to_check_if_satisfied = HashSet::new();
        for claim in claims_satisfied {
            leases_to_check_if_satisfied.insert(&claim.lease_id);
            // Look for pending claims that were (at least partially) blocked
            // by a claim on this element level that is now satisfied.
            // (They may still have other blockers, though.)
            let pending_claims =
                self.catalog.pending_claims.for_required_element(&claim.dependent().element_id);
            tracing::debug!(
                "pending_claims.for_required_element({:?}) = {:?})",
                &claim.dependent().element_id,
                &pending_claims
            );
            let claims_activated = self.activate_claims_if_dependencies_satisfied(pending_claims);
            tracing::debug!("claims_activated = {:?})", &claims_activated);
            self.update_required_levels(element_ids_required_by_claims(&claims_activated));
        }
        for lease_id in leases_to_check_if_satisfied {
            self.update_lease_status(lease_id);
        }
        // Find claims to drop
        let claims_to_drop_for_element = self.catalog.active_claims.to_drop_for_element(element_id);
        let claims_dropped = self.drop_claims_with_no_dependents(&claims_to_drop_for_element);
        self.update_required_levels(element_ids_required_by_claims(&claims_dropped));
        Ok(())
    }

    pub fn watch_current_level(
        &mut self,
        element_id: &ElementID,
    ) -> UnboundedReceiver<Option<PowerLevel>> {
        self.current.subscribe(element_id)
    }

    pub fn watch_required_level(
        &mut self,
        element_id: &ElementID,
    ) -> UnboundedReceiver<Option<PowerLevel>> {
        self.required.subscribe(element_id)
    }

    pub fn acquire_lease(
        &mut self,
        element_id: &ElementID,
        level: PowerLevel,
    ) -> Result<Lease, fpb::LeaseError> {
        let (lease, claims) = self.catalog.create_lease_and_claims(element_id, level);
        let claims_activated = self.activate_claims_if_dependencies_satisfied(claims);
        self.update_required_levels(element_ids_required_by_claims(&claims_activated));
        self.update_lease_status(&lease.id);
        Ok(lease)
    }

    pub fn drop_lease(&mut self, lease_id: &LeaseID) -> Result<(), Error> {
        let (_, claims) = self.catalog.drop(lease_id)?;
        let claims_dropped = self.drop_claims_with_no_dependents(&claims);
        self.update_required_levels(element_ids_required_by_claims(&claims_dropped));
        self.catalog.lease_status.remove(lease_id);
        Ok(())
    }

    fn calculate_lease_status(&self, lease_id: &LeaseID) -> LeaseStatus {
        // If a lease has any Pending claims, it is Pending.
        if !self.catalog.pending_claims.for_lease(lease_id).is_empty() {
            return LeaseStatus::Pending;
        }
        // If a lease has any active claims that have not been satisfied
        // it is still Pending.
        for claim in self.catalog.active_claims.for_lease(lease_id) {
            if !self.current_level_satisfies(claim.requires()) {
                return LeaseStatus::Pending;
            }
        }
        // All claims are active and satisfied, so the lease is Satisfied.
        LeaseStatus::Satisfied
    }

    pub fn update_lease_status(&mut self, lease_id: &LeaseID) {
        let status = self.calculate_lease_status(lease_id);
        self.catalog.lease_status.update(lease_id, status);
    }

    pub fn watch_lease_status(
        &mut self,
        lease_id: &LeaseID,
    ) -> UnboundedReceiver<Option<LeaseStatus>> {
        self.catalog.watch_lease_status(lease_id)
    }

    fn update_required_levels(&mut self, element_ids: Vec<&ElementID>) {
        for element_id in element_ids {
            let new_required_level = self.catalog.calculate_required_level(element_id);
            tracing::debug!("update required level({:?}, {:?})", element_id, new_required_level);
            self.required.update(element_id, new_required_level);
        }
    }

    /// Examines a Vec of pending claims and activates each claim for which the
    /// required element is already at the required level (and thus the claim
    /// is already satisfied), or all of the dependencies of its required
    /// ElementLevel are met.
    /// For example, let us imagine elements A, B, C and D where A depends on B
    /// and B depends on C and D. In order to activate the A->B claim, all
    /// dependencies of B (i.e. B->C and B->D) must first be satisfied.
    /// Returns a Vec of activated claims.
    fn activate_claims_if_dependencies_satisfied(&mut self, claims: Vec<Claim>) -> Vec<Claim> {
        tracing::debug!("activate_claims_if_dependencies_satisfied: {:?}", claims);
        let claims_to_activate: Vec<Claim> = claims
            .into_iter()
            .filter(|c| {
                // If the required element is already at the required level,
                // then the claim can immediately be activated (and is
                // already satisfied).
                self.current_level_satisfies(c.requires())
                // Otherwise, it can only be activated if all of its
                // dependencies are satisfied.
                    || self.all_dependencies_satisfied(&c.requires())
            })
            .collect();
        for claim in &claims_to_activate {
            self.catalog.activate_pending_claim(&claim.id);
        }
        claims_to_activate
    }

    /// Examines the direct active and passive dependencies of an element level
    /// and returns true if they are all satisfied (current level >= required).
    fn all_dependencies_satisfied(&self, element_level: &ElementLevel) -> bool {
        let active_dependencies = self.catalog.topology.direct_active_dependencies(&element_level);
        let passive_dependencies =
            self.catalog.topology.direct_passive_dependencies(&element_level);
        active_dependencies.into_iter().chain(passive_dependencies).all(|dep| {
            if !self.current_level_satisfies(&dep.requires) {
                tracing::debug!(
                    "dependency {dep:?} of element_level {element_level:?} is not satisfied: \
                    current level of {:?} = {:?}, {:?} required",
                    &dep.requires.element_id,
                    self.current.get(&dep.requires.element_id),
                    &dep.requires.level
                );
                return false;
            }
            return true;
        })
    }

    /// Examines a Vec of claims and drops any that no longer have any
    /// dependents. Returns a Vec of dropped claims.
    fn drop_claims_with_no_dependents(&mut self, claims: &Vec<Claim>) -> Vec<Claim> {
        tracing::debug!("check_claims_to_drop: {:?}", claims);
        let mut claims_to_drop = Vec::new();
        for claim_to_check in claims {
            let mut has_dependents = false;
            // Only claims belonging to the same lease can be a dependent.
            for related_claim in self.catalog.active_claims.for_lease(&claim_to_check.lease_id) {
                if related_claim.requires() == claim_to_check.dependent() {
                    has_dependents = true;
                    break;
                }
            }
            if has_dependents {
                continue;
            }
            tracing::debug!("will drop claim: {:?}", claim_to_check);
            claims_to_drop.push(claim_to_check.clone());
        }
        for claim in &claims_to_drop {
            self.catalog.drop_active_claim(&claim.id);
        }
        claims_to_drop
    }

    pub fn add_element(
        &mut self,
        name: &str,
        default_level: PowerLevel,
        level_dependencies: Vec<fpb::LevelDependency>,
        credentials_to_register: Vec<CredentialToRegister>,
    ) -> Result<ElementID, AddElementError> {
        let id = self.catalog.topology.add_element(name, default_level)?;
        self.required.update(&id, default_level);
        for dependency in level_dependencies {
            let credential = self
                .lookup_credentials(dependency.requires.token.into())
                .ok_or(AddElementError::NotAuthorized)?;
            if !credential.contains(Permissions::MODIFY_DEPENDENT) {
                return Err(AddElementError::NotAuthorized);
            }
            if let Err(err) = self.catalog.topology.add_active_dependency(&Dependency {
                dependent: ElementLevel {
                    element_id: id.clone(),
                    level: dependency.dependent_level,
                },
                requires: ElementLevel {
                    element_id: credential.get_element().clone(),
                    level: dependency.requires.level,
                },
            }) {
                // Clean up by removing the element we just added.
                self.remove_element(&id);
                return Err(match err {
                    AddDependencyError::AlreadyExists => AddElementError::Invalid,
                    AddDependencyError::ElementNotFound(_) => AddElementError::Invalid,
                    AddDependencyError::RequiredElementNotFound(_) => AddElementError::Invalid,
                    // Shouldn't get NotAuthorized here.
                    AddDependencyError::NotAuthorized => AddElementError::Internal,
                });
            };
        }
        for credential_to_register in credentials_to_register {
            if let Err(err) = self.register_credential(&id, credential_to_register) {
                match err {
                    RegisterCredentialsError::Internal => {
                        tracing::error!(
                            "credentials.register returned an Internal error: {:?}",
                            id
                        );
                        return Err(AddElementError::Internal);
                    }
                    // Owner should be authorized for all credentials on its
                    // own element, so this is an Internal error:
                    RegisterCredentialsError::NotAuthorized => {
                        tracing::error!("credentials.register unexpectedly returned NotAuthorized in add_element: {:?}", id);
                        return Err(AddElementError::Internal);
                    }
                }
            }
        }
        Ok(id)
    }

    #[cfg(test)]
    fn element_exists(&self, element_id: &ElementID) -> bool {
        self.catalog.topology.element_exists(element_id)
    }

    pub fn remove_element(&mut self, element_id: &ElementID) {
        self.catalog.topology.remove_element(element_id);
        self.unregister_all_credentials_for_element(element_id);
        self.current.remove(element_id);
        self.required.remove(element_id);
    }

    /// Checks authorization from tokens, and if valid, adds a dependency to the Topology.
    pub fn add_dependency(
        &mut self,
        dependent_token: Token,
        dependent_level: PowerLevel,
        requires_token: Token,
        requires_level: PowerLevel,
    ) -> Result<(), AddDependencyError> {
        let Some(dependent_cred) = self.lookup_credentials(dependent_token) else {
            return Err(AddDependencyError::NotAuthorized);
        };
        if !dependent_cred.contains(Permissions::MODIFY_DEPENDENCY) {
            return Err(AddDependencyError::NotAuthorized);
        }
        let Some(requires_cred) = self.lookup_credentials(requires_token) else {
            return Err(AddDependencyError::NotAuthorized);
        };
        if !requires_cred.contains(Permissions::MODIFY_DEPENDENT) {
            return Err(AddDependencyError::NotAuthorized);
        }
        self.catalog.topology.add_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: dependent_cred.get_element().clone(),
                level: dependent_level,
            },
            requires: ElementLevel {
                element_id: requires_cred.get_element().clone(),
                level: requires_level,
            },
        })
    }

    /// Checks authorization from tokens, and if valid, removes a dependency from the Topology.
    pub fn remove_dependency(
        &mut self,
        dependent_token: Token,
        dependent_level: PowerLevel,
        requires_token: Token,
        requires_level: PowerLevel,
    ) -> Result<(), RemoveDependencyError> {
        let Some(dependent_cred) = self.lookup_credentials(dependent_token) else {
            return Err(RemoveDependencyError::NotAuthorized);
        };
        if !dependent_cred.contains(Permissions::MODIFY_DEPENDENCY) {
            return Err(RemoveDependencyError::NotAuthorized);
        }
        let Some(requires_cred) = self.lookup_credentials(requires_token) else {
            return Err(RemoveDependencyError::NotAuthorized);
        };
        if !requires_cred.contains(Permissions::MODIFY_DEPENDENT) {
            return Err(RemoveDependencyError::NotAuthorized);
        }
        self.catalog.topology.remove_active_dependency(&Dependency {
            dependent: ElementLevel {
                element_id: dependent_cred.get_element().clone(),
                level: dependent_level,
            },
            requires: ElementLevel {
                element_id: requires_cred.get_element().clone(),
                level: requires_level,
            },
        })
    }
}

pub type LeaseID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct Lease {
    pub id: LeaseID,
    pub element_id: ElementID,
    pub level: PowerLevel,
}

impl Lease {
    fn new(element_id: &ElementID, level: PowerLevel) -> Self {
        let id = LeaseID::from(Uuid::new_v4().as_simple().to_string());
        Lease { id: id.clone(), element_id: element_id.clone(), level: level.clone() }
    }
}

type ClaimID = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
struct Claim {
    pub id: ClaimID,
    dependency: Dependency,
    pub lease_id: LeaseID,
}

impl Claim {
    fn new(dependency: Dependency, lease_id: &LeaseID) -> Self {
        Claim {
            id: ClaimID::from(Uuid::new_v4().as_simple().to_string()),
            dependency,
            lease_id: lease_id.clone(),
        }
    }

    fn dependent(&self) -> &ElementLevel {
        &self.dependency.dependent
    }

    fn requires(&self) -> &ElementLevel {
        &self.dependency.requires
    }
}

/// Returns a Vec of unique ElementIDs required by claims.
fn element_ids_required_by_claims(claims: &Vec<Claim>) -> Vec<&ElementID> {
    claims.into_iter().map(|c| &c.requires().element_id).unique().collect()
}

#[derive(Debug)]
struct Catalog {
    topology: Topology,
    leases: HashMap<LeaseID, Lease>,
    lease_status: SubscribeMap<LeaseID, LeaseStatus>,
    /// Active claims affect the required level communicated by Power Broker.
    /// All dependencies of their required element are met.
    active_claims: ClaimTracker,
    /// Pending claims do not yet affect the required level communicated by
    /// Power Broker. Some dependencies of their required element are not met.
    /// Once all dependencies are met, they will be moved to active claims.
    pending_claims: ClaimTracker,
    /// Passive claims must be satisfied in order for their lease to be
    /// satisfied but since they do not affect the required levels of the
    /// element they depend upon, something else must raise the required
    /// element's level in order for a passive claim to be satisfied (either
    /// another active claim, or an unmanaged element raising its own level).
    passive_claims: ClaimTracker,
}

impl Catalog {
    fn new() -> Self {
        Catalog {
            topology: Topology::new(),
            leases: HashMap::new(),
            lease_status: SubscribeMap::new(),
            active_claims: ClaimTracker::new(),
            pending_claims: ClaimTracker::new(),
            passive_claims: ClaimTracker::new(),
        }
    }

    /// Calculates the required level for each element, according to the
    /// Minimum Power Level Policy. Only active claims are considered here.
    fn calculate_required_level(&self, element_id: &ElementID) -> PowerLevel {
        let no_claims = Vec::new();
        let element_claims = self
            .active_claims
            .claims_by_required_element_id
            .get(element_id)
            // Treat both missing key and empty vec as no claims.
            .unwrap_or(&no_claims)
            .iter()
            .filter_map(|id| self.active_claims.claims.get(id));
        // Return the maximum of all active claims:
        if let Some(required_level) = element_claims.map(|x| x.requires().level).max() {
            required_level
        } else {
            // No claims, return default level:
            if let Some(default_level) = self.topology.get_default_level(element_id) {
                default_level
            } else {
                tracing::error!(
                    "calculate_required_level: no default level set for {:?}",
                    element_id
                );
                PowerLevel::Binary(BinaryPowerLevel::Off)
            }
        }
    }

    /// Creates a new lease for the given element and level along with all
    /// claims necessary to satisfy this lease and adds them to pending_claims.
    /// Returns the new lease, and a Vec of pending claims created.
    fn create_lease_and_claims(
        &mut self,
        element_id: &ElementID,
        level: PowerLevel,
    ) -> (Lease, Vec<Claim>) {
        tracing::debug!("acquire({:?}, {:?})", &element_id, &level);
        // TODO: Add lease validation and control.
        let lease = Lease::new(&element_id, level);
        self.leases.insert(lease.id.clone(), lease.clone());
        let mut pending_claims = Vec::new();
        let element_level = ElementLevel { element_id: element_id.clone(), level: level.clone() };
        let (active_dependencies, passive_dependencies) =
            self.topology.all_active_and_passive_dependencies(&element_level);
        // Create pending claims for each of the active dependencies.
        for dependency in active_dependencies {
            let pending_claim = Claim::new(dependency, &lease.id);
            pending_claims.push(pending_claim);
        }
        for claim in pending_claims.iter() {
            tracing::debug!("adding pending claim: {:?}", &claim);
            self.pending_claims.add(claim.clone());
        }
        // Create passive claims for each of the passive dependencies.
        for dependency in passive_dependencies {
            let passive_claim = Claim::new(dependency, &lease.id);
            self.passive_claims.add(passive_claim);
        }
        (lease, pending_claims)
    }

    /// Drops an existing lease, and initiates process of releasing all
    /// associated claims.
    /// Returns the dropped lease, and a Vec of all active claims that have
    /// been marked to drop.
    fn drop(&mut self, lease_id: &LeaseID) -> Result<(Lease, Vec<Claim>), Error> {
        tracing::debug!("drop({:?})", &lease_id);
        let lease = self.leases.remove(lease_id).ok_or(anyhow!("{lease_id} not found"))?;
        let active_claims = self.active_claims.for_lease(&lease.id);
        tracing::debug!("active_claim_ids: {:?}", &active_claims);
        let pending_claims = self.pending_claims.for_lease(&lease.id);
        tracing::debug!("pending_claim_ids: {:?}", &pending_claims);
        let passive_claims = self.passive_claims.for_lease(&lease.id);
        tracing::debug!("pending_claim_ids: {:?}", &passive_claims);
        // Pending claims should be dropped immediately.
        for claim in pending_claims {
            if let Some(removed) = self.pending_claims.remove(&claim.id) {
                tracing::debug!("removing pending claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove pending claim: not found: {}", claim.id);
            }
        }
        // Passive claims should be dropped immediately.
        for claim in passive_claims {
            if let Some(removed) = self.passive_claims.remove(&claim.id) {
                tracing::debug!("removing passive claim: {:?}", &removed);
            } else {
                tracing::error!("cannot remove passive claim: not found: {}", claim.id);
            }
        }
        // Active claims should be marked to drop in an orderly sequence.
        let claims_marked_to_drop = self.active_claims.mark_lease_claims_to_drop(&lease.id);
        Ok((lease, claims_marked_to_drop))
    }

    /// Activates a pending claim, moving it to active_claims.
    fn activate_pending_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("activate_claim: {:?}", claim_id);
        self.pending_claims.move_to(&claim_id, &mut self.active_claims);
    }

    /// Drops an active claim, removing it from active_claims.
    fn drop_active_claim(&mut self, claim_id: &ClaimID) {
        tracing::debug!("drop_claim: {:?}", claim_id);
        self.active_claims.remove(&claim_id);
    }

    fn watch_lease_status(&mut self, lease_id: &LeaseID) -> UnboundedReceiver<Option<LeaseStatus>> {
        self.lease_status.subscribe(lease_id)
    }
}

#[derive(Debug)]
struct ClaimTracker {
    claims: HashMap<ClaimID, Claim>,
    claims_by_required_element_id: HashMap<ElementID, Vec<ClaimID>>,
    claims_by_lease: HashMap<LeaseID, Vec<ClaimID>>,
    claims_to_drop_by_element_id: HashMap<ElementID, Vec<ClaimID>>,
}

impl ClaimTracker {
    fn new() -> Self {
        ClaimTracker {
            claims: HashMap::new(),
            claims_by_required_element_id: HashMap::new(),
            claims_by_lease: HashMap::new(),
            claims_to_drop_by_element_id: HashMap::new(),
        }
    }

    fn add(&mut self, claim: Claim) {
        self.claims_by_required_element_id
            .entry(claim.requires().element_id.clone())
            .or_insert(Vec::new())
            .push(claim.id.clone());
        self.claims_by_lease
            .entry(claim.lease_id.clone())
            .or_insert(Vec::new())
            .push(claim.id.clone());
        self.claims.insert(claim.id.clone(), claim);
    }

    fn remove(&mut self, id: &ClaimID) -> Option<Claim> {
        let Some(claim) = self.claims.remove(id) else {
            return None;
        };
        if let Some(claim_ids) =
            self.claims_by_required_element_id.get_mut(&claim.requires().element_id)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_by_required_element_id.remove(&claim.requires().element_id);
            }
        }
        if let Some(claim_ids) = self.claims_by_lease.get_mut(&claim.lease_id) {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_by_lease.remove(&claim.lease_id);
            }
        }
        if let Some(claim_ids) =
            self.claims_to_drop_by_element_id.get_mut(&claim.dependent().element_id)
        {
            claim_ids.retain(|x| x != id);
            if claim_ids.is_empty() {
                self.claims_to_drop_by_element_id.remove(&claim.dependent().element_id);
            }
        }
        Some(claim)
    }

    /// Marks all claims associated with this lease to drop. They will be
    /// removed in an orderly sequence (each claim will be removed only once
    /// all claims dependent on it have already been dropped).
    /// Returns a Vec of Claims marked to drop.
    fn mark_lease_claims_to_drop(&mut self, lease_id: &LeaseID) -> Vec<Claim> {
        let claims_marked = self.for_lease(lease_id);
        for claim in &claims_marked {
            self.claims_to_drop_by_element_id
                .entry(claim.dependent().element_id.clone())
                .or_insert(Vec::new())
                .push(claim.id.clone());
        }
        claims_marked
    }

    /// Removes claim from this tracker, and adds it to recipient.
    fn move_to(&mut self, id: &ClaimID, recipient: &mut ClaimTracker) {
        if let Some(claim) = self.remove(id) {
            recipient.add(claim);
        }
    }

    fn for_claim_ids(&self, claim_ids: &Vec<ClaimID>) -> Vec<Claim> {
        claim_ids.iter().map(|id| self.claims.get(id)).filter_map(|f| f).cloned().collect()
    }

    fn for_required_element(&self, element_id: &ElementID) -> Vec<Claim> {
        let Some(claim_ids) = self.claims_by_required_element_id.get(element_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }

    fn for_lease(&self, lease_id: &LeaseID) -> Vec<Claim> {
        let Some(claim_ids) = self.claims_by_lease.get(lease_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }

    fn to_drop_for_element(&self, element_id: &ElementID) -> Vec<Claim> {
        let Some(claim_ids) = self.claims_to_drop_by_element_id.get(element_id) else {
            return Vec::new();
        };
        self.for_claim_ids(claim_ids)
    }
}

/// SubscribeMap is a wrapper around a HashMap that stores values V by key K
/// and allows subscribers to register a channel on which they will receive
/// updates whenever the value stored changes.
#[derive(Debug)]
struct SubscribeMap<K: Clone + Hash + Eq, V: Clone + PartialEq> {
    values: HashMap<K, V>,
    senders: HashMap<K, Vec<UnboundedSender<Option<V>>>>,
}

impl<K: Clone + Hash + Eq, V: Clone + PartialEq> SubscribeMap<K, V> {
    fn new() -> Self {
        SubscribeMap { values: HashMap::new(), senders: HashMap::new() }
    }

    fn get(&self, key: &K) -> Option<V> {
        self.values.get(key).cloned()
    }

    fn update(&mut self, key: &K, value: V) {
        // If the value hasn't changed, this is a no-op.
        if let Some(current) = self.get(key) {
            if current == value {
                return;
            }
        }
        self.values.insert(key.clone(), value.clone());
        let mut senders_to_retain = Vec::new();
        if let Some(senders) = self.senders.remove(&key) {
            for sender in senders {
                if let Err(err) = sender.unbounded_send(Some(value.clone())) {
                    if err.is_disconnected() {
                        continue;
                    }
                }
                senders_to_retain.push(sender);
            }
        }
        // Prune invalid senders.
        self.senders.insert(key.clone(), senders_to_retain);
    }

    fn subscribe(&mut self, key: &K) -> UnboundedReceiver<Option<V>> {
        let (sender, receiver) = unbounded::<Option<V>>();
        sender.unbounded_send(self.get(key)).expect("initial send failed");
        self.senders.entry(key.clone()).or_insert(Vec::new()).push(sender);
        receiver
    }

    fn remove(&mut self, key: &K) {
        self.values.remove(key);
        self.senders.remove(key);
    }
}

/// A PowerLevel satisfies a required PowerLevel if it is
/// greater than or equal to it on the same scale.
trait SatisfyPowerLevel {
    fn satisfies(&self, required: PowerLevel) -> bool;
}

impl SatisfyPowerLevel for PowerLevel {
    fn satisfies(&self, required: PowerLevel) -> bool {
        match self {
            PowerLevel::Binary(self_binary) => match required {
                PowerLevel::Binary(required_binary) => {
                    required_binary == BinaryPowerLevel::Off || self_binary == &BinaryPowerLevel::On
                }
                _ => false,
            },
            PowerLevel::UserDefined(self_user_defined) => match required {
                PowerLevel::UserDefined(required_level) => {
                    self_user_defined.level >= required_level.level
                }
                _ => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_power_broker::{
        BinaryPowerLevel, Permissions, PowerLevel, UserDefinedPowerLevel,
    };
    use fuchsia_zircon::{self as zx, HandleBased};

    #[fuchsia::test]
    fn test_binary_satisfy_power_level() {
        for (level, required, want) in [
            (BinaryPowerLevel::Off, BinaryPowerLevel::On, false),
            (BinaryPowerLevel::Off, BinaryPowerLevel::Off, true),
            (BinaryPowerLevel::On, BinaryPowerLevel::Off, true),
            (BinaryPowerLevel::On, BinaryPowerLevel::On, true),
        ] {
            let got = PowerLevel::Binary(level).satisfies(PowerLevel::Binary(required));
            assert_eq!(
                got, want,
                "{:?}.satisfies({:?}) = {:?}, want {:?}",
                level, required, got, want
            );
        }
    }

    #[fuchsia::test]
    fn test_user_defined_satisfy_power_level() {
        for (level, required, want) in [
            (0, 1, false),
            (0, 0, true),
            (1, 0, true),
            (1, 1, true),
            (255, 0, true),
            (255, 1, true),
            (255, 255, true),
            (1, 255, false),
            (35, 36, false),
            (35, 35, true),
        ] {
            let got = PowerLevel::UserDefined(UserDefinedPowerLevel { level })
                .satisfies(PowerLevel::UserDefined(UserDefinedPowerLevel { level: required }));
            assert_eq!(
                got, want,
                "{:?}.satisfies({:?}) = {:?}, want {:?}",
                level, required, got, want
            );
        }
    }

    #[fuchsia::test]
    fn test_levels() {
        let mut levels = SubscribeMap::<ElementID, PowerLevel>::new();

        levels.update(&"A".into(), PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), PowerLevel::Binary(BinaryPowerLevel::Off));
        levels.update(&"B".into(), PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::Off)));
        assert_eq!(levels.get(&"B".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));

        levels.update(&"UD1".into(), PowerLevel::UserDefined(UserDefinedPowerLevel { level: 145 }));
        assert_eq!(
            levels.get(&"UD1".into()),
            Some(PowerLevel::UserDefined(UserDefinedPowerLevel { level: 145 }))
        );
        assert_eq!(levels.get(&"UD2".into()), None);
    }

    #[fuchsia::test]
    fn test_levels_subscribe() {
        let mut levels = SubscribeMap::<ElementID, PowerLevel>::new();

        let mut receiver_a = levels.subscribe(&"A".into());
        let mut receiver_b = levels.subscribe(&"B".into());

        levels.update(&"A".into(), PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));
        assert_eq!(levels.get(&"B".into()), None);

        levels.update(&"A".into(), PowerLevel::Binary(BinaryPowerLevel::Off));
        levels.update(&"B".into(), PowerLevel::Binary(BinaryPowerLevel::On));
        assert_eq!(levels.get(&"A".into()), Some(PowerLevel::Binary(BinaryPowerLevel::Off)));
        assert_eq!(levels.get(&"B".into()), Some(PowerLevel::Binary(BinaryPowerLevel::On)));

        let mut received_a = Vec::new();
        while let Ok(Some(level)) = receiver_a.try_next() {
            received_a.push(level)
        }
        assert_eq!(
            received_a,
            vec![
                None,
                Some(PowerLevel::Binary(BinaryPowerLevel::On)),
                Some(PowerLevel::Binary(BinaryPowerLevel::Off))
            ]
        );
        let mut received_b = Vec::new();
        while let Ok(Some(level)) = receiver_b.try_next() {
            received_b.push(level)
        }
        assert_eq!(received_b, vec![None, Some(PowerLevel::Binary(BinaryPowerLevel::On))]);
    }

    #[fuchsia::test]
    fn test_add_element_dependency_not_authorized() {
        let mut broker = Broker::new();
        let (token_mithril, token_mithril_broker) = zx::EventPair::create();
        let credential_mithril = CredentialToRegister {
            broker_token: token_mithril_broker.into(),
            permissions: Permissions::all(),
        };
        let (token_mithril_read_only, token_mithril_read_only_broker) = zx::EventPair::create();
        let credential_mithril_read_only = CredentialToRegister {
            broker_token: token_mithril_read_only_broker.into(),
            permissions: Permissions::READ_POWER_LEVEL,
        };
        let (_, token_silver_broker) = zx::EventPair::create();
        let credential_silver = CredentialToRegister {
            broker_token: token_silver_broker.into(),
            permissions: Permissions::all(),
        };
        broker
            .add_element(
                "Mithril",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![],
                vec![credential_mithril, credential_mithril_read_only],
            )
            .expect("add_element failed");

        // This should fail, because token_mithril_read_only does not have
        // Permissions::MODIFY_DEPENDENT
        let add_element_not_authorized_res = broker.add_element(
            "Silver",
            PowerLevel::Binary(BinaryPowerLevel::Off),
            vec![fpb::LevelDependency {
                dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                requires: fpb::ElementLevel {
                    token: token_mithril_read_only
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            }],
            vec![credential_silver],
        );
        assert!(matches!(add_element_not_authorized_res, Err(AddElementError::NotAuthorized)));

        // Add element with a valid dependency should succeed.
        let (_, token_silver_broker) = zx::EventPair::create();
        let credential_silver = CredentialToRegister {
            broker_token: token_silver_broker.into(),
            permissions: Permissions::all(),
        };
        broker
            .add_element(
                "Silver",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![fpb::LevelDependency {
                    dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                    requires: fpb::ElementLevel {
                        token: token_mithril
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        level: PowerLevel::Binary(BinaryPowerLevel::On),
                    },
                }],
                vec![credential_silver],
            )
            .expect("add_element failed");
    }

    #[fuchsia::test]
    fn test_remove_element() {
        let mut broker = Broker::new();
        let unobtanium = broker
            .add_element("Unobtainium", PowerLevel::Binary(BinaryPowerLevel::Off), vec![], vec![])
            .expect("add_element failed");
        assert_eq!(broker.element_exists(&unobtanium), true);

        broker.remove_element(&unobtanium);
        assert_eq!(broker.element_exists(&unobtanium), false);
    }

    #[fuchsia::test]
    async fn test_register_unregister_credentials() {
        let mut broker = Broker::new();
        let (token_element_owner, token_element_broker) = zx::EventPair::create();
        let broker_credential = CredentialToRegister {
            broker_token: token_element_broker.into(),
            permissions: Permissions::READ_POWER_LEVEL
                | Permissions::MODIFY_POWER_LEVEL
                | Permissions::MODIFY_DEPENDENT
                | Permissions::MODIFY_DEPENDENCY
                | Permissions::MODIFY_CREDENTIAL
                | Permissions::REMOVE_ELEMENT,
        };
        broker
            .add_element(
                "element",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![],
                vec![broker_credential],
            )
            .expect("add_element failed");
        let (token_new_owner, token_new_broker) = zx::EventPair::create();
        let credential_to_register = CredentialToRegister {
            broker_token: token_new_broker.into(),
            permissions: Permissions::READ_POWER_LEVEL | Permissions::MODIFY_POWER_LEVEL,
        };
        broker
            .register_credentials(
                token_element_owner
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_to_register],
            )
            .expect("register_credentials failed");

        let (_, token_not_authorized_broker) = zx::EventPair::create();
        let credential_not_authorized = CredentialToRegister {
            broker_token: token_not_authorized_broker.into(),
            permissions: Permissions::READ_POWER_LEVEL | Permissions::MODIFY_POWER_LEVEL,
        };
        let res_not_authorized = broker.register_credentials(
            token_new_owner
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("duplicate_handle failed")
                .into(),
            vec![credential_not_authorized],
        );
        assert!(matches!(res_not_authorized, Err(RegisterCredentialsError::NotAuthorized)));

        broker
            .unregister_credentials(token_element_owner.into(), vec![token_new_owner.into()])
            .expect("unregister_credentials failed");
    }

    #[fuchsia::test]
    fn test_broker_lease_direct() {
        // Create a topology of a child element with two direct dependencies.
        // P1 <- C -> P2
        let mut broker = Broker::new();
        let (parent1_token, parent1_broker_token) = zx::EventPair::create();
        let parent1_cred = CredentialToRegister {
            broker_token: parent1_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENCY | Permissions::MODIFY_DEPENDENT,
        };
        let parent1: ElementID = broker
            .add_element(
                "P1",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![],
                vec![parent1_cred],
            )
            .expect("add_element failed");
        let (parent2_token, parent2_broker_token) = zx::EventPair::create();
        let parent2_cred = CredentialToRegister {
            broker_token: parent2_broker_token.into(),
            permissions: Permissions::MODIFY_DEPENDENT,
        };
        let parent2: ElementID = broker
            .add_element(
                "P2",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![],
                vec![parent2_cred],
            )
            .expect("add_element failed");
        let child = broker
            .add_element(
                "C",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![
                    fpb::LevelDependency {
                        dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                        requires: fpb::ElementLevel {
                            token: parent1_token
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .expect("dup failed"),
                            level: PowerLevel::Binary(BinaryPowerLevel::On),
                        },
                    },
                    fpb::LevelDependency {
                        dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                        requires: fpb::ElementLevel {
                            token: parent2_token
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .expect("dup failed"),
                            level: PowerLevel::Binary(BinaryPowerLevel::On),
                        },
                    },
                ],
                vec![],
            )
            .expect("add_element failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent1),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 1 should start with required level OFF"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&parent2),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 2 should start with required level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for each dependency.
        let lease = broker
            .acquire_lease(&child, PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent1),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent 1 should now have required level ON from direct claim"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&parent2),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent 2 should now have required level ON from direct claim"
        );

        // Now drop the lease and verify both claims are also dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent1),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 1 should now have required level OFF from dropped claim"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&parent2),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent 2 should now have required level OFF from dropped claim"
        );

        // Try dropping the lease one more time, which should result in an error.
        let extra_drop = broker.drop_lease(&lease.id);
        assert!(extra_drop.is_err());
    }

    #[fuchsia::test]
    fn test_broker_lease_transitive() {
        // Create a topology of a child element with two chained transitive
        // dependencies.
        // C -> P -> GP
        let mut broker = Broker::new();
        let (grandparent_token, grandparent_broker_token) = zx::EventPair::create();
        let grandparent_cred = CredentialToRegister {
            broker_token: grandparent_broker_token.into(),
            permissions: Permissions::MODIFY_POWER_LEVEL | Permissions::MODIFY_DEPENDENT,
        };
        let grandparent: ElementID = broker
            .add_element(
                "GP",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![],
                vec![grandparent_cred],
            )
            .expect("add_element failed");
        let (parent_token, parent_broker_token) = zx::EventPair::create();
        let parent_cred = CredentialToRegister {
            broker_token: parent_broker_token.into(),
            permissions: Permissions::MODIFY_POWER_LEVEL
                | Permissions::MODIFY_DEPENDENCY
                | Permissions::MODIFY_DEPENDENT,
        };
        let parent: ElementID = broker
            .add_element("P", PowerLevel::Binary(BinaryPowerLevel::Off), vec![], vec![parent_cred])
            .expect("add_element failed");
        let child = broker
            .add_element(
                "C",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![fpb::LevelDependency {
                    dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                    requires: fpb::ElementLevel {
                        token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        level: PowerLevel::Binary(BinaryPowerLevel::On),
                    },
                }],
                vec![],
            )
            .expect("add_element failed");
        broker
            .add_dependency(
                parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed").into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_dependency failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should start with required level OFF"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should start with required level OFF"
        );

        // Acquire the lease, which should result in two claims, one
        // for the direct parent dependency, and one for the transitive
        // grandparent dependency.
        let lease = broker
            .acquire_lease(&child, PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have required level OFF, waiting on Grandparent to turn ON"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have required level ON because of no dependencies"
        );

        // Raise Grandparent power level to ON, now Parent claim should be active.
        broker
            .update_current_level(&grandparent, PowerLevel::Binary(BinaryPowerLevel::On))
            .expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Parent should now have required level ON"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should now have required level ON"
        );

        // Now drop the lease and verify Parent claim is dropped, but
        // Grandparent claim is not yet dropped.
        broker.drop_lease(&lease.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should now have required level OFF after lease drop"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::Binary(BinaryPowerLevel::On),
            "Grandparent should still have required level ON"
        );

        // Lower Parent power level to OFF, now Grandparent claim should be
        // dropped and should have required level OFF.
        broker
            .update_current_level(&parent, PowerLevel::Binary(BinaryPowerLevel::Off))
            .expect("update_current_level failed");
        tracing::info!("catalog after update_current_level: {:?}", &broker.catalog);
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Parent should have required level OFF"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::Binary(BinaryPowerLevel::Off),
            "Grandparent should now have required level OFF"
        );
    }

    #[fuchsia::test]
    fn test_broker_lease_shared() {
        // Create a topology of two child elements with a shared
        // parent and grandparent
        // C1 \
        //     > P -> GP
        // C2 /
        // Child 1 requires Parent at 50 to support its own level of 5.
        // Parent requires Grandparent at 200 to support its own level of 50.
        // C1 -> P -> GP
        //  5 -> 50 -> 200
        // Child 2 requires Parent at 30 to support its own level of 3.
        // Parent requires Grandparent at 90 to support its own level of 30.
        // C2 -> P -> GP
        //  3 -> 30 -> 90
        // Grandparent has a default required level of 10.
        // All other elements have a default of 0.
        let mut broker = Broker::new();
        let (grandparent_token, grandparent_broker_token) = zx::EventPair::create();
        let grandparent_cred = CredentialToRegister {
            broker_token: grandparent_broker_token.into(),
            permissions: Permissions::MODIFY_POWER_LEVEL | Permissions::MODIFY_DEPENDENT,
        };
        let grandparent: ElementID = broker
            .add_element(
                "GP",
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 }),
                vec![],
                vec![grandparent_cred],
            )
            .expect("add_element failed");
        let (parent_token, parent_broker_token) = zx::EventPair::create();
        let parent_cred = CredentialToRegister {
            broker_token: parent_broker_token.into(),
            permissions: Permissions::MODIFY_POWER_LEVEL
                | Permissions::MODIFY_DEPENDENCY
                | Permissions::MODIFY_DEPENDENT,
        };
        let parent: ElementID = broker
            .add_element(
                "P",
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
                vec![
                    fpb::LevelDependency {
                        dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel {
                            level: 50,
                        }),
                        requires: fpb::ElementLevel {
                            token: grandparent_token
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .expect("dup failed"),
                            level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
                        },
                    },
                    fpb::LevelDependency {
                        dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel {
                            level: 30,
                        }),
                        requires: fpb::ElementLevel {
                            token: grandparent_token
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .expect("dup failed"),
                            level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 90 }),
                        },
                    },
                ],
                vec![parent_cred],
            )
            .expect("add_element failed");
        let child1 = broker
            .add_element(
                "C1",
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
                vec![fpb::LevelDependency {
                    dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 5 }),
                    requires: fpb::ElementLevel {
                        token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
                    },
                }],
                vec![],
            )
            .expect("add_element failed");
        let child2 = broker
            .add_element(
                "C2",
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
                vec![fpb::LevelDependency {
                    dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 3 }),
                    requires: fpb::ElementLevel {
                        token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }),
                    },
                }],
                vec![],
            )
            .expect("add_element failed");

        // Initially, Grandparent should have a default required level of 10
        // and Parent should have a default required level of 0.
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            "Parent should start with required level 0"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 }),
            "Grandparent should start with required level at its default of 10"
        );

        // Acquire lease for Child 1. Initially, Grandparent should have
        // required level 200 and Parent should have required level 0
        // because Child 1 has a dependency on Parent and Parent has a
        // dependency on Grandparent. Grandparent has no dependencies so its
        // level should be raised first.
        let lease1 = broker
            .acquire_lease(&child1, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 5 }))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            "Parent should now have required level 0, waiting on Grandparent to reach required level"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
            "Grandparent should now have required level 100 because of parent dependency and it has no dependencies of its own"
        );

        // Raise Grandparent's current level to 200. Now Parent claim should
        // be active, because its dependency on Grandparent is unblocked
        // raising its required level to 50.
        broker
            .update_current_level(
                &grandparent,
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
            )
            .expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
            "Parent should now have required level 50"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
            "Grandparent should still have required level 200"
        );

        // Update Parent's current level to 50.
        // Parent and Grandparent should have required levels of 50 and 200.
        broker
            .update_current_level(
                &parent,
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
            )
            .expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
            "Parent should now have required level 50"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
            "Grandparent should still have required level 200"
        );

        // Acquire a lease for Child 2. Though Child 2 has nominal
        // requirements of Parent at 30 and Grandparent at 100, they are
        // superseded by Child 1's requirements of 50 and 200.
        let lease2 = broker
            .acquire_lease(&child2, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 3 }))
            .expect("acquire failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
            "Parent should still have required level 50"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
            "Grandparent should still have required level 100"
        );

        // Drop lease for Child 1. Parent's required level should immediately
        // drop to 30. Grandparent's required level will remain at 200 for now.
        broker.drop_lease(&lease1.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }),
            "Parent should still have required level 2 from the second claim"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
            "Grandparent should still have required level 100 from the second claim"
        );

        // Lower Parent's current level to 30. Now Grandparent's required level
        // should drop to 90.
        broker
            .update_current_level(
                &parent,
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }),
            )
            .expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }),
            "Parent should have required level 30"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 90 }),
            "Grandparent should now have required level 90"
        );

        // Drop lease for Child 2, Parent should have required level 0.
        // Grandparent should still have required level 90.
        broker.drop_lease(&lease2.id).expect("drop failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            "Parent should now have required level 0"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 90 }),
            "Grandparent should still have required level 90"
        );

        // Lower Parent's current level to 0. Grandparent claim should now be
        // dropped and have its default required level of 10.
        broker
            .update_current_level(
                &parent,
                PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            )
            .expect("update_current_level failed");
        assert_eq!(
            broker.catalog.calculate_required_level(&parent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            "Parent should have required level 0"
        );
        assert_eq!(
            broker.catalog.calculate_required_level(&grandparent),
            PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 }),
            "Grandparent should now have required level 10"
        );
    }

    #[fuchsia::test]
    fn test_add_remove_dependency() {
        let mut broker = Broker::new();
        let (token_adamantium_all, token_adamantium_all_broker) = zx::EventPair::create();
        let credential_adamantium_all = CredentialToRegister {
            broker_token: token_adamantium_all_broker.into(),
            permissions: Permissions::all(),
        };
        let (token_vibranium_all, token_vibranium_all_broker) = zx::EventPair::create();
        let credential_vibranium_all = CredentialToRegister {
            broker_token: token_vibranium_all_broker.into(),
            permissions: Permissions::all(),
        };
        broker
            .add_element(
                "Adamantium",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![],
                vec![credential_adamantium_all],
            )
            .expect("add_element failed");
        broker
            .add_element(
                "Vibranium",
                PowerLevel::Binary(BinaryPowerLevel::Off),
                vec![],
                vec![credential_vibranium_all],
            )
            .expect("add_element failed");

        // Only MODIFY_DEPENDENCY and MODIFY_DEPENDENT for the respective
        // elements should be required to add and remove a dependency:
        let (token_adamantium_mod_dependency_only, token_adamantium_mod_dependency_only_broker) =
            zx::EventPair::create();
        let credential_adamantium_mod_dependency_only = CredentialToRegister {
            broker_token: token_adamantium_mod_dependency_only_broker.into(),
            permissions: Permissions::MODIFY_DEPENDENCY,
        };
        broker
            .register_credentials(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_adamantium_mod_dependency_only],
            )
            .expect("register_credentials failed");
        let (token_vibranium_mod_dependent_only, token_vibranium_mod_dependent_only_broker) =
            zx::EventPair::create();
        let credential_vibranium_mod_dependent_only = CredentialToRegister {
            broker_token: token_vibranium_mod_dependent_only_broker.into(),
            permissions: Permissions::MODIFY_DEPENDENT,
        };
        broker
            .register_credentials(
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_vibranium_mod_dependent_only],
            )
            .expect("register_credentials failed");
        broker
            .add_dependency(
                token_adamantium_mod_dependency_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_mod_dependent_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_dependency failed");

        broker
            .remove_dependency(
                token_adamantium_mod_dependency_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_mod_dependent_only
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("remove_dependency failed");

        // Adding should return NotAuthorized if missing MODIFY_DEPENDENCY for
        // dependency.level
        let (token_adamantium_none, token_adamantium_none_broker) = zx::EventPair::create();
        let credential_adamantium_none = CredentialToRegister {
            broker_token: token_adamantium_none_broker.into(),
            permissions: Permissions::empty(),
        };
        broker
            .register_credentials(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_adamantium_none],
            )
            .expect("register_credentials failed");
        let res_add_missing_mod_dependency = broker.add_dependency(
            token_adamantium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_mod_dependent_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(res_add_missing_mod_dependency, Err(AddDependencyError::NotAuthorized)));

        // Adding should return NotAuthorized if missing MODIFY_DEPENDENT for
        // dependency.requires
        let (token_vibranium_none, token_vibranium_none_broker) = zx::EventPair::create();
        let credential_vibranium_none = CredentialToRegister {
            broker_token: token_vibranium_none_broker.into(),
            permissions: Permissions::empty(),
        };
        broker
            .register_credentials(
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("duplicate_handle failed")
                    .into(),
                vec![credential_vibranium_none],
            )
            .expect("register_credentials failed");
        let res_add_missing_mod_dependent = broker.add_dependency(
            token_adamantium_mod_dependency_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(res_add_missing_mod_dependent, Err(AddDependencyError::NotAuthorized)));

        // Adding with extra permissions should work.
        broker
            .add_dependency(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("add_dependency with extra permissions failed");

        // Removing should return NotAuthorized if missing MODIFY_DEPENDENCY for
        // dependency.level
        let res_remove_missing_mod_dependency = broker.remove_dependency(
            token_adamantium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_mod_dependent_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(
            res_remove_missing_mod_dependency,
            Err(RemoveDependencyError::NotAuthorized)
        ));

        // Removing should return NotAuthorized if missing MODIFY_DEPENDENT for
        // dependency.requires
        let res_remove_missing_mod_dependent = broker.remove_dependency(
            token_adamantium_mod_dependency_only
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
            token_vibranium_none
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("dup failed")
                .into(),
            PowerLevel::Binary(BinaryPowerLevel::On),
        );
        assert!(matches!(
            res_remove_missing_mod_dependent,
            Err(RemoveDependencyError::NotAuthorized)
        ));

        // Removing with extra permissions should work.
        broker
            .remove_dependency(
                token_adamantium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
                token_vibranium_all
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")
                    .into(),
                PowerLevel::Binary(BinaryPowerLevel::On),
            )
            .expect("remove_dependency with extra permissions failed");
    }
}
