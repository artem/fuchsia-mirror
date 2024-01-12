// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    target::{
        self, DiscoveredTarget, Identity, IdentityCmp, SharedIdentity, Target, TargetAddrEntry,
        TargetAddrStatus, TargetUpdate, WeakIdentity,
    },
    MDNS_MAX_AGE,
};
use addr::TargetAddr;
use anyhow::Result;
use async_trait::async_trait;
use async_utils::event::Event;
use chrono::Utc;
use ffx_daemon_core::events::{self, EventSynthesizer};
use ffx_daemon_events::{DaemonEvent, TargetEvent, TargetEventInfo};
use fidl_fuchsia_developer_ffx as ffx;
use netext::IsLocalAddr;
use std::{
    borrow::Borrow,
    cell::RefCell,
    collections::{hash_map::Entry, HashMap},
    fmt::Debug,
    net::IpAddr,
    net::SocketAddr,
    ops::ControlFlow,
    rc::Rc,
    sync::Arc,
};

pub struct TargetCollection {
    targets: RefCell<HashMap<u64, Rc<Target>>>,
    // This list is always small so O(n) lookups do not matter.
    // Dangling references removed on iteration & use.
    identities: RefCell<Vec<target::WeakIdentity>>,
    events: RefCell<Option<events::Queue<DaemonEvent>>>,
    // Unlike the daemon event, these also fire for discovered targets.
    // TODO(b/303144137): Stop publishing events to the daemon-wide event queue.
    on_targets_changed: RefCell<Event>,
}

#[async_trait(?Send)]
impl EventSynthesizer<DaemonEvent> for TargetCollection {
    async fn synthesize_events(&self) -> Vec<DaemonEvent> {
        // TODO(awdavies): This won't be accurate once a target is able to create
        // more than one event at a time.
        let mut res = Vec::with_capacity(self.targets.borrow().len());
        let targets = self.targets.borrow().values().cloned().collect::<Vec<_>>();
        for target in targets.into_iter() {
            if target.is_enabled() {
                res.push(DaemonEvent::NewTarget(target.target_info()));
            }
        }
        res
    }
}

impl TargetCollection {
    pub fn new() -> Self {
        Self {
            targets: RefCell::new(HashMap::new()),
            identities: vec![].into(),
            events: RefCell::new(None),
            on_targets_changed: RefCell::new(Event::new()),
        }
    }

    #[cfg(test)]
    fn new_with_queue() -> Rc<Self> {
        let target_collection = Rc::new(Self::new());
        let queue = events::Queue::new(&target_collection);
        target_collection.set_event_queue(queue);
        target_collection
    }

    pub fn set_event_queue(&self, q: events::Queue<DaemonEvent>) {
        self.events.replace(Some(q));
    }

    // TODO(b/297896647): Filter discovered targets and introduce `discover_targets` as the new
    // multi-target discovery method.
    pub fn targets(&self, query: Option<&TargetQuery>) -> Vec<ffx::TargetInfo> {
        // Merge targets by shared identity.

        #[derive(Clone, Debug, PartialEq)]
        struct AddrKey(ffx::TargetAddrInfo);

        impl Eq for AddrKey {}

        impl std::hash::Hash for AddrKey {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                let (ip, port, scope_id) = match &self.0 {
                    ffx::TargetAddrInfo::Ip(ip) => (ip.ip, 0, ip.scope_id),
                    ffx::TargetAddrInfo::IpPort(ip) => (ip.ip, ip.port, ip.scope_id),
                };

                ip.hash(state);
                state.write_u16(port);
                state.write_u32(scope_id);
            }
        }

        struct MergeState {
            info: ffx::TargetInfo,
            // Address -> Age mapping.
            // Determines how addresses are sorted.
            addrs: HashMap<AddrKey, u64>,
        }

        fn merge<T>(lhs: &mut Option<T>, rhs: Option<T>, f: impl FnOnce(&mut T, T)) {
            let Some(rhs) = rhs else { return };
            match lhs {
                Some(lhs) => f(lhs, rhs),
                None => {}
            }
        }

        fn check_eq<T: PartialEq + Debug>(lhs: &Option<T>, rhs: &Option<T>, name: &str) {
            let Some(lhs) = lhs else { return };
            let Some(rhs) = rhs else { return };
            if lhs != rhs {
                tracing::warn!("Identical target with mismatching {name}: {lhs:?} vs {rhs:?}");
            }
        }

        fn addr_age(addr: &ffx::TargetAddrInfo, target: &Target, age_ms: Option<u64>) -> u64 {
            if target.rcs_address() == Some(SocketAddr::from(TargetAddr::from(addr.clone()))) {
                // Give the RCS address the highest priority possible.
                u64::MAX
            } else {
                age_ms.unwrap_or(0)
            }
        }

        // One invariant that `Self::merge_insert_identity` maintains is that identities will always
        // be equal by pointer. This makes merging _significantly_ faster.
        let mut merged = HashMap::<*const Identity, MergeState>::new();
        let mut unidentified = Vec::new();

        for target in self.targets.borrow().values() {
            if let Some(query) = query {
                if !query.matches(target) {
                    continue;
                }
            }

            let info = ffx::TargetInfo::from(&**target);

            let Some(key) = target.identity() else {
                unidentified.push(info);
                continue;
            };

            match merged.entry(&*key) {
                Entry::Occupied(mut other) => {
                    let MergeState { info: prev, addrs } = other.get_mut();

                    // Previously this was an info message, moving to debug as this is internal
                    // state of target collection.
                    tracing::debug!("Merging {prev:?} + {info:?}");

                    let ffx::TargetInfo {
                        nodename: _,
                        addresses,
                        age_ms,
                        rcs_state,
                        target_state,
                        product_config,
                        board_config,
                        serial_number: _,
                        ssh_address,
                        fastboot_interface,
                        ssh_host_address,
                        compatibility,
                        // Intentionally opt-in to compilation failures when new fields are added.
                        __source_breaking: _,
                    } = info;

                    // Nodename and Serial already match.

                    // Addresses are sorted and added to the merged target later.
                    addresses.into_iter().flatten().for_each(|addr| {
                        let age_ms = addr_age(&addr, &target, age_ms);

                        addrs
                            .entry(AddrKey(addr))
                            .and_modify(|addr_age_ms| {
                                if *addr_age_ms < age_ms {
                                    *addr_age_ms = age_ms;
                                }
                            })
                            .or_insert(age_ms);
                    });

                    merge(&mut prev.rcs_state, rcs_state, |old, new| {
                        *old = match (*old, new) {
                            (ffx::RemoteControlState::Up, _) | (_, ffx::RemoteControlState::Up) => {
                                ffx::RemoteControlState::Up
                            }
                            (ffx::RemoteControlState::Down, _)
                            | (_, ffx::RemoteControlState::Down) => ffx::RemoteControlState::Down,
                            _ => return,
                        };
                    });

                    if prev.rcs_state == Some(ffx::RemoteControlState::Up) {
                        check_eq(
                            &prev.target_state,
                            &Some(ffx::TargetState::Product),
                            "target_state",
                        );

                        // Always override state if RCS is up.
                        prev.target_state = Some(ffx::TargetState::Product);
                    } else if target_state.is_some() {
                        // Override state by age.
                        match (prev.age_ms, age_ms) {
                            (Some(prev_age), Some(cur_age)) if cur_age >= prev_age => {
                                prev.target_state = target_state;
                            }
                            (None, Some(_)) => {
                                prev.target_state = target_state;
                            }
                            _ => {
                                merge(&mut prev.target_state, target_state, |old, new| {
                                    if *old == ffx::TargetState::Disconnected {
                                        *old = new;
                                    } else {
                                        tracing::warn!("Conflicting state: {old:?} vs {new:?}");
                                    }
                                });
                            }
                        }
                    }

                    check_eq(&prev.product_config, &product_config, "product_config");
                    check_eq(&prev.board_config, &board_config, "board_config");
                    check_eq(&prev.ssh_address, &ssh_address, "ssh_address");
                    check_eq(&prev.fastboot_interface, &fastboot_interface, "fastboot_interface");
                    check_eq(&prev.ssh_host_address, &ssh_host_address, "ssh_host_address");
                    check_eq(&prev.compatibility, &compatibility, "compatibility");

                    merge(&mut prev.age_ms, age_ms, |old, new| {
                        if *old < new {
                            *old = new;
                        }
                    });
                }
                Entry::Vacant(vacant) => {
                    let addrs =
                        HashMap::from_iter(info.addresses.as_deref().into_iter().flatten().map(
                            |addr| (AddrKey(addr.clone()), addr_age(&addr, &target, info.age_ms)),
                        ));
                    vacant.insert(MergeState { info, addrs });
                }
            }
        }

        for merge_state in merged.values_mut() {
            if merge_state.addrs.is_empty() {
                continue;
            }

            let addrs = &mut merge_state.info.addresses;
            let addrs = addrs.get_or_insert(Vec::new());
            addrs.clear();
            addrs.extend(merge_state.addrs.keys().map(|key| key.0.clone()));
            addrs.sort_by(|addr_a, addr_b| {
                let a = merge_state.addrs.get(&AddrKey(addr_a.clone())).copied().unwrap_or(0);
                let b = merge_state.addrs.get(&AddrKey(addr_b.clone())).copied().unwrap_or(0);
                b.cmp(&a).then_with(|| {
                    // If the ages are equal, compare addresses to maintain stability.
                    SocketAddr::from(TargetAddr::from(addr_b.clone()))
                        .cmp(&SocketAddr::from(TargetAddr::from(addr_a.clone())))
                })
            });
        }

        unidentified.extend(merged.into_iter().map(|(_, v)| v.info));
        unidentified
    }

    pub fn is_empty(&self) -> bool {
        self.targets.borrow().len() == 0
    }

    pub fn remove_target(&self, target_id: String) -> bool {
        if let Ok(Some(t)) = self.query_any_single_target(&target_id.into(), |_| true) {
            let target = self.targets.borrow_mut().remove(&t.id());
            if let Some(target) = target {
                target.disable();
            }
            true
        } else {
            false
        }
    }

    pub fn remove_ephemeral_target(&self, target: Rc<Target>) -> bool {
        let Some(target) = self.targets.borrow_mut().remove(&target.id()) else { return false };
        target.disable();
        true
    }

    /// Checks and expires stale targets.
    ///
    /// Returns the list of expired manual addresses.
    pub fn expire_targets(&self, overnet_node: &Arc<overnet_core::Router>) -> Vec<SocketAddr> {
        let mut expired_manual_addrs = Vec::new();
        let mut expired = Vec::new();
        for target in Vec::from_iter(self.targets.borrow().values()) {
            target.expire_state();
            if target.is_manual() && !target.is_connected() {
                // If a manual target has been allowed to transition to the
                // "disconnected" state, it should be removed from the collection.
                let ssh_port = target.ssh_port();
                for addr in target.manual_addrs() {
                    let mut sockaddr = SocketAddr::from(addr);
                    ssh_port.map(|p| sockaddr.set_port(p));
                    expired_manual_addrs.push(sockaddr);
                }
                expired.push(target.clone());
            } else if target.is_manual() {
                // Manually-added remote targets will not be discovered by mDNS,
                // and as a result will not have host-pipe triggered automatically
                // by the mDNS event handler.
                target.run_host_pipe(overnet_node);
            }
        }

        for target in expired {
            self.remove_ephemeral_target(target);
        }

        if !expired_manual_addrs.is_empty() {
            tracing::debug!("Expired manual addresses: {expired_manual_addrs:?}");
        }
        expired_manual_addrs
    }

    // Merge intersecting identities.
    // If multiple identities intersect then update targets that use them.
    // e.g. [(nodename: foo), (serial: bar)] + (nodename: foo, serial: bar)
    fn merge_insert_identity(&self, mut identity: Identity) -> Rc<Identity> {
        let mut idents = self.identities.borrow_mut();

        let mut merge = false;
        let mut duplicate = None;

        // Merge overlapping identities into the incoming identity
        for id in idents.iter_mut() {
            let Some(id) = id.upgrade() else { continue };

            let Some(cmp) = id.cmp_to(&identity) else { continue };

            if cmp == IdentityCmp::Intersects {
                // Incoming identity has more knowledge than this identity.
                // (nodename, serial) > (nodename)

                // Previously this was an info message, moving to debug as this is internal state
                // of target collection.
                tracing::debug!("Merge identity {:?} with {:?}", identity, id);
                identity.join((*id).clone());
                merge = true;
            } else {
                // This identity has the same or more knowledge than the incoming identity.
                duplicate = Some(id);
            }
        }

        if let Some(duplicate) = duplicate.filter(|_| !merge) {
            // No need to update anything, reusing an existing one.
            return duplicate;
        }

        let identity = SharedIdentity::new(identity);
        let mut targets_changed = false;

        idents.retain_mut(|id| {
            let Some(id) = id.upgrade() else { return false };
            if !identity.is_same(&id) {
                return true;
            }

            // Update all targets _specifically_ using the old identity to the new one.
            for target in self.targets.borrow().values() {
                let is_ptr_eq =
                    target.try_with_identity(|ident| Rc::ptr_eq(ident, &id)).unwrap_or(false);

                if is_ptr_eq {
                    // This was previously info, moving to debug as this is the internal state of
                    // ffx target collection.
                    tracing::debug!("Updating identity of {:?}", target);
                    target.replace_shared_identity(Rc::clone(&identity));
                    targets_changed |= true;
                }
            }

            false
        });

        idents.push(Rc::downgrade(&identity));

        if targets_changed {
            self.signal_targets_changed();
        }

        identity
    }

    fn find_matching_target(&self, new_target: &Target) -> (Option<Rc<Target>>, bool) {
        // Look for a target by primary ID first
        let new_ids = new_target.ids();
        let mut network_changed = false;
        let mut to_update =
            new_ids.iter().find_map(|id| self.targets.borrow().get(id).map(|t| t.clone()));

        // If we haven't yet found a target, try to find one by all IDs, nodename, serial, or address.
        if to_update.is_none() {
            let new_ips =
                new_target.addrs().iter().map(|addr| addr.ip().clone()).collect::<Vec<IpAddr>>();
            let new_port = new_target.ssh_port();
            let new_identity = new_target.identity();

            for target in self.targets.borrow().values() {
                let identity_match = || {
                    let Some(new_id) = &new_identity else { return false };
                    target.try_with_identity(|old_id| new_id.is_same(old_id)).unwrap_or(false)
                };

                // If there is a port and addr in both the new and target,
                // they must match if the nodename or the serial match.
                //
                // WARNING (wilkinsonclay): This matching is only considering
                // the separate ssh_port and the IP addresses. This may not
                // be correct when considering fastboot or other connections
                // where the port field in the address should be considered.
                let address_match = || {
                    target.addrs().iter().any(|addr| new_ips.contains(&addr.ip()))
                        && match target.ssh_port() {
                            Some(port) => {
                                if let Some(new) = new_port {
                                    port == new
                                } else {
                                    false
                                }
                            }
                            None if new_port.is_none() => true,
                            None => false,
                        }
                };

                // If we get here, there was no match on id, so perform a loose
                // match if the serial or the nodename or the address are the same.
                // At somepoint it might be a good idea to prioritize these matches
                // for example, a match on ip and port might be more authoritative
                // than matching on nodename, more analysis is needed.
                if target.has_id(new_ids.iter()) || identity_match() || address_match() {
                    tracing::debug!(
                        "Matched target has_id: {id} identity:\
                     {identity} address: {addr}\
                     for {new_nodename:?} {new_ips:?}\
                     ssh: {new_port:?}",
                        new_nodename = target.nodename(),
                        id = target.has_id(new_ids.iter()),
                        identity = identity_match(),
                        addr = address_match()
                    );
                    to_update.replace(target.clone());

                    // The effect of returning true for network_changed
                    // is to trigger reconnecting the host_pipe to the target.
                    // The main reason to do this is for user mode networking
                    // with an emulator, where the port mapped to SSH changes,
                    // but the host pipe is using the old port mapped to an old
                    // instance.
                    //
                    // A side-effect of this physical targets that respond to
                    // mDNS on IPv4 and IPv6, the address will change quickly
                    // as the target is coming up, this can cause a lot of
                    // confusion and race conditions.
                    //
                    // To avoid that, only return network changed if the port
                    // is specified as well as a change.
                    network_changed = if let Some(target_ssh_port) = target.ssh_port() {
                        new_port.unwrap_or_default() != target_ssh_port
                    } else {
                        false
                    };
                    break;
                }
            }
        } else {
            tracing::debug!("Matched target by id: {to_update:?}");
            network_changed = false
        }

        (to_update, network_changed)
    }

    fn try_push_new_target_event(&self, target: &Target) {
        // Only send event if it is considered in-use.
        // Discovered targets do not post events.
        if target.is_enabled() {
            if let Some(event_queue) = self.events.borrow().as_ref() {
                event_queue
                    .push(DaemonEvent::NewTarget(target.target_info()))
                    .unwrap_or_else(|e| tracing::warn!("unable to push new target event: {}", e));
            }
        }
    }

    // Wait for targets until the predicate exits the loop.
    //
    // Panics if the predicate attempts to mutate the target collection.
    async fn wait_for_change<B>(&self, mut predicate: impl FnMut() -> ControlFlow<B>) -> B {
        // A timeout is not needed here as timeouts are handled downstream and may be
        // undesired. Futures can be dropped before completion, so the loop will always exit.
        loop {
            // Guard against modification while running the predicate.
            // It is also important that the predicate is synchronous as we do not want to race
            // with merge_insert.
            let guard = self.targets.borrow();
            if let ControlFlow::Break(b) = predicate() {
                return b;
            }
            drop(guard);

            tracing::debug!("Waiting for next change...");

            // TODO(b/297896647): Move synchronous polled discovery here. Make sure to clone the
            // event before doing sync polling.

            // TODO(b/297896647): Kick off discovery advertisement every 10 seconds when not found.
            // Benign discovery types (USB) can poll here. Just make it explicit that we're waiting
            // on discovery.

            self.on_targets_changed.clone().into_inner().wait().await;
        }
    }

    fn do_enable_target(&self, target: &Target) {
        let was_disabled = !target.is_enabled();
        target.enable();

        if was_disabled {
            tracing::info!(
                "Enabling ['Discovered'] target:['{}']@[{}]",
                target.nodename_str(),
                target.id()
            );

            // Discovered target went from unused to used.
            // Broadcast NewTarget event since we do not broadcast events for discovered
            // targets.
            self.try_push_new_target_event(target);
        }
    }

    fn signal_targets_changed(&self) {
        self.on_targets_changed.replace(Event::new()).signal();
    }

    // TODO(b/304312166): Test-only now.
    // Will be removed once "targets" are associated with a single address.
    #[doc(hidden)]
    #[tracing::instrument(skip(self))]
    pub fn merge_insert(&self, new_target: Rc<Target>) -> Rc<Target> {
        // Drop non-manual loopback address entries, as matching against
        // them could otherwise match every target in the collection.
        new_target.drop_loopback_addrs();

        // Canonicalize new_target's identity.
        if let Some(new_identity) = new_target.take_identity() {
            let ident = if self.identities.borrow().iter().any(|rc| {
                // Shortcut if the identity is already known:
                WeakIdentity::ptr_eq(rc, &SharedIdentity::downgrade(&new_identity))
            }) {
                new_identity
            } else {
                self.merge_insert_identity(
                    Rc::try_unwrap(new_identity).unwrap_or_else(|id| (*id).clone()),
                )
            };

            new_target.replace_shared_identity(ident);
        }

        let (to_update, network_changed) = self.find_matching_target(&new_target);

        tracing::trace!("Merging target {:?} into {:?}", new_target, to_update);

        // Do not merge unscoped link-local addresses into the target
        // collection, as they are not routable and therefore not safe
        // for connecting to the remote, and may collide with other
        // scopes.
        new_target.drop_unscoped_link_local_addrs();

        let Some(to_update) = to_update else {
            // The target was not matched in the collection, so insert it and return.

            tracing::info!(
                "Adding new ['{:?}'] target: [{}]",
                new_target.get_connection_state(),
                new_target.id()
            );
            tracing::debug!("{:#?}", new_target);
            self.targets.borrow_mut().insert(new_target.id(), new_target.clone());

            self.try_push_new_target_event(&*new_target);
            self.signal_targets_changed();
            return new_target;
        };

        if let Some(config) = new_target.build_config() {
            to_update.build_config.borrow_mut().replace(config);
        }

        if new_target.has_identity() && !to_update.has_identity() {
            to_update.replace_shared_identity(new_target.identity().unwrap());
        } else if new_target.has_identity() && !new_target.identity_matches(&to_update) {
            let identity = new_target.identity().unwrap();
            tracing::info!("Changing identity of {:?} to {:?}", to_update, identity);
            to_update.replace_shared_identity(identity);
        }

        if let Some(ssh_port) = new_target.ssh_port() {
            to_update.set_ssh_port(Some(ssh_port));
        }
        to_update.update_last_response(new_target.last_response());
        let mut addrs = new_target.addrs.borrow().iter().cloned().collect::<Vec<_>>();
        addrs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        to_update.addrs_extend(addrs.into_iter());
        to_update.addrs.borrow_mut().retain(|t| {
            let is_too_old = Utc::now().signed_duration_since(t.timestamp).num_milliseconds()
                as i128
                > MDNS_MAX_AGE.as_millis() as i128;
            !is_too_old || t.is_manual()
        });
        to_update.update_boot_timestamp(new_target.boot_timestamp_nanos());

        // The network changed flag indicates the target being merged matched an
        // existing target in the collection, but the network address did not match.
        // One example of this happening is matching an emulator by name, but the
        // emulator has been stopped, and restarted, causing the ssh port to change.
        //
        // When this happens, clean up the host_pipe and reconnect, and clear
        // the ssh host_address.
        if network_changed {
            tracing::warn!("Network address changed for {to_update:?}");
            if to_update.is_connected() {
                to_update.disconnect();
                to_update.maybe_reconnect();
                *to_update.ssh_host_address.borrow_mut() = None;
            }
        } else {
            if to_update.ssh_host_address.borrow().is_none() {
                tracing::debug!(
                    "Setting ssh_host_address to {:?} for {}@{}",
                    new_target.ssh_host_address,
                    to_update.nodename_str(),
                    to_update.id()
                );
                *to_update.ssh_host_address.borrow_mut() =
                    new_target.ssh_host_address.borrow().clone();
            }
        }

        to_update.set_compatibility_status(&new_target.get_compatibility_status());

        if let Some(fastboot_interface) = new_target.fastboot_interface() {
            *to_update.fastboot_interface.borrow_mut() = Some(fastboot_interface);
        }

        to_update.update_connection_state(|_| new_target.get_connection_state());

        if new_target.is_transient() {
            to_update.mark_transient();
        }

        if new_target.is_enabled() {
            self.do_enable_target(&*to_update);
        }

        self.signal_targets_changed();

        // Misnomer. Event should be renamed to `Updated`
        to_update.events.push(TargetEvent::Rediscovered).unwrap_or_else(|err| {
            tracing::warn!("unable to enqueue rediscovered event: {:#}", err)
        });

        if to_update.is_enabled() {
            if let Some(event_queue) = self.events.borrow().as_ref() {
                event_queue
                    .push(DaemonEvent::UpdatedTarget(to_update.target_info()))
                    .unwrap_or_else(|e| {
                        tracing::warn!("unable to push target update event: {}", e)
                    });
            } else {
                tracing::debug!("No event queue for this target collection.");
            }
        }

        to_update
    }

    /// Updates targets matching any of the update filters, optionally creating one if it
    /// doesn't exist.
    ///
    /// Returns the number of updated targets.
    pub fn update_target<'a, F>(
        &self,
        filters: &'a [F],
        mut update: TargetUpdate<'a>,
        create_new: bool,
    ) -> usize
    where
        F: Borrow<TargetUpdateFilter<'a>> + Debug,
    {
        // For all matching targets, create a temporary target by id and update it.
        tracing::debug!("Updating targets matching {filters:?} with {update:?}");

        // Merge identities early so filters match on _merged_ identities.
        if let Some(identity) = update.identity.take() {
            update.identity = Some(self.merge_insert_identity(
                Rc::try_unwrap(identity).unwrap_or_else(|rc| (*rc).clone()),
            ));
        }

        let mut merge_targets = self
            .targets
            .borrow()
            .values()
            .filter(|target| filters.iter().any(|f| f.borrow().matches(target)))
            .map(|target| {
                // Create a target with the same ID so we can specifically update it.
                let target = Target::new_with_id(target.id());
                // Apply update to the newly created target since we are reusing merge_insert.
                target.apply_update(update.clone());
                target
            })
            .collect::<Vec<_>>();

        if create_new && merge_targets.is_empty() {
            // Insert a fresh & empty target to apply an update against.
            let target = self.merge_insert(Target::new());

            let target = Target::new_with_id(target.id());
            target.apply_update(update.clone());

            merge_targets.push(target);
        }

        let matches = merge_targets.len();

        for merge_target in merge_targets {
            // TODO(b/304312166): Stop depending on merge_insert here.
            // merge_insert is used to maintain previous behaviors. If direct usage of
            // Target::apply_update introduces a regression the corresponding revert will be much
            // smaller.
            self.merge_insert(merge_target);
        }

        matches
    }

    pub fn try_to_reconnect_target<'a, F>(
        &self,
        filters: &'a [F],
        overnet_node: &Arc<overnet_core::Router>,
    ) -> bool
    where
        F: Borrow<TargetUpdateFilter<'a>> + Debug,
    {
        let mut ret = None;
        for target in self.targets.borrow().values() {
            if !filters.iter().any(|f| f.borrow().matches(target)) {
                continue;
            }

            if let Some(prev) = ret.as_mut() {
                Self::select_preferred_target(target, prev)
            } else {
                ret = Some(target.clone());
            }
        }

        match ret {
            Some(target) if target.is_enabled() => {
                if !target.is_host_pipe_running() {
                    tracing::debug!("Reconnecting to {:?}", &target.addrs());
                    target.run_host_pipe(overnet_node);
                }
                true
            }
            _ => false,
        }
    }

    fn has_unidentified_target(&self) -> bool {
        // An unidentified target is a target without a nodename or serial. Manual targets will get
        // these when the RCS connection is established.
        // These could end up matching _any_ query, however we should be careful as some manual targets
        // may fail to connect to RCS.
        let mut found = false;
        for target in self.targets.borrow().values() {
            if target.is_waiting_for_rcs_identity() {
                tracing::info!(
                    "Unidentified Target waiting for RCS: {:?}@{}",
                    target.addrs(),
                    target.id()
                );
                found = true;
            }
        }
        found
    }

    // In typical (synchronous) usage, targets should not disappear between query -> enable/get info.

    /// With consent from the user (explicit ffx invocation), enable and use a discovered target.
    ///
    /// Using random targets will result in unintended behavior, as devices in multi-device
    /// environments may be in use by different ffx daemons.
    pub fn use_target(&self, discovered: DiscoveredTarget, user_reason: &str) -> Rc<Target> {
        let target = discovered.0;

        match self.targets.borrow().get(&target.id()) {
            Some(found) if Rc::ptr_eq(&target, found) => {
                // Exact target is still within collection.
            }
            _ => {
                // Not found/somehow replaced. This only happens if a discovered target is held for
                // a long period of time.
                // We simply close the target and return it. It is already invalid and should not be
                // used.
                tracing::warn!(
                    "Internal Inconsistency: Attempted to enable target not in collection"
                );

                if target.is_enabled() {
                    tracing::warn!("Disabling inconsistent target");

                    target.disable();
                }

                return target;
            }
        }

        if !target.is_enabled() {
            tracing::info!(
                "Using discovered target: {} (USER REASON: {user_reason})",
                target.nodename_str()
            );
        }
        self.do_enable_target(&target);
        target
    }

    fn query_any_target<B>(
        &self,
        query: &TargetQuery,
        mut f: impl FnMut(&Rc<Target>) -> ControlFlow<B>,
    ) -> Option<B> {
        for target in self.targets.borrow().values() {
            if !query.matches(target) {
                continue;
            }

            let cf = f(target);
            if let ControlFlow::Break(b) = cf {
                return Some(b);
            }
        }

        None
    }

    fn select_preferred_target(new: &Rc<Target>, current: &mut Rc<Target>) {
        'preferred: {
            if new.is_host_pipe_running() && !current.is_host_pipe_running() {
                tracing::info!("Prioritizing duplicate with established connection");
                break 'preferred;
            } else if new.rcs().is_some() && !current.rcs().is_some() {
                tracing::info!("Prioritizing duplicate with RCS connection");
                break 'preferred;
            }

            if new.is_connected() {
                if !current.is_connected() {
                    tracing::info!("Prioritizing duplicate, other one has expired");
                    break 'preferred;
                } else if new.last_response() > current.last_response() {
                    tracing::info!("Prioritizing recently seen duplicate");
                    break 'preferred;
                }
            }

            return;
        }

        *current = new.clone();
    }

    fn query_any_single_target(
        &self,
        query: &TargetQuery,
        predicate: impl Fn(&Target) -> bool,
    ) -> Result<Option<Rc<Target>>, ()> {
        let mut selected: Option<Rc<Target>> = None;
        let mut found_multiple = false;
        self.query_any_target::<()>(query, |target| {
            if !predicate(target) {
                return ControlFlow::Continue(());
            }

            if let Some(selected) = selected.as_mut() {
                // Do not mark the query as ambiguous when the targets share the same identity or
                // address. Instead, pick the most preferred one.
                if (query.is_query_on_identity() && selected.identity_matches(target))
                    || query.is_query_on_address()
                {
                    tracing::info!("Found duplicate target with matching identity: {target:?}");
                    Self::select_preferred_target(target, selected);
                } else {
                    if !found_multiple {
                        // Print out the header with the first selected target, then
                        // subsequent log lines print the rest of the matching targets.
                        tracing::error!("Target query {query:?} matched multiple targets:");
                        tracing::error!("{:?}", selected);
                        found_multiple = true;
                    }

                    tracing::error!("& {:?}", target);
                }
            } else {
                selected = Some(target.clone());
            }
            ControlFlow::Continue(())
        });

        if found_multiple {
            Err(())
        } else {
            Ok(selected)
        }
    }

    // Filters through targets, returning info for matching targets. Targets must be previously
    // enabled by `use_target`.
    pub fn query_enabled_targets<B>(
        &self,
        query: &TargetQuery,
        mut f: impl FnMut(&Rc<Target>) -> ControlFlow<B>,
    ) -> Option<B> {
        self.query_any_target(query, move |target| {
            if !target.is_enabled() {
                tracing::debug!("Skipping inactive target {target:?}");
                ControlFlow::Continue(())
            } else {
                f(target)
            }
        })
    }

    pub fn query_single_enabled_target(
        &self,
        query: &TargetQuery,
    ) -> Result<Option<Rc<Target>>, ()> {
        self.query_any_single_target(query, |target| {
            if !target.is_enabled() {
                tracing::debug!("Skipping inactive target {target:?}");
                false
            } else {
                true
            }
        })
    }

    /// Waits to find a single matching target, without filtering out discovered/disabled targets.
    ///
    /// Use `query_enabled_targets` to find a previously used target.
    ///
    /// Returns an error if multiple targets match. In an environment where targets are discovered
    /// asynchronously this error will not consistently fire.
    #[tracing::instrument(skip(self))]
    pub async fn discover_target(&self, query: &TargetQuery) -> Result<DiscoveredTarget, ()> {
        tracing::debug!("Using query: {:?}", query);

        // A timeout is not needed here as timeouts are handled downstream and may be
        // undesired. Futures can be dropped before completion, so the loop will always exit.
        let target = self
            .wait_for_change(|| {
                // Prefer enabled targets over discovered targets.
                // Functions as a way to silo usages of the "first" matcher.
                let Ok(enabled_target) = self.query_single_enabled_target(query) else {
                    return ControlFlow::Break(Err(()));
                };

                if let Some(found) = enabled_target {
                    return ControlFlow::Break(Ok(found));
                }

                // Otherwise match against all targets.
                ControlFlow::Break(match self.query_any_single_target(query, |_| true) {
                    Ok(Some(found)) => Ok(found),
                    Ok(None) => return ControlFlow::Continue(()),
                    Err(_) => {
                        tracing::warn!("Too many targets matched query");

                        // Manual targets may not have a nodename yet. The nodename is fetched
                        // once the RCS connection is established.
                        // NOTE: Only applies if the query is a wildcard matcher.
                        if self.has_unidentified_target() {
                            tracing::info!(
                                "Waiting for unidentified manual target(s) to finish identification"
                            );
                            return ControlFlow::Continue(());
                        }

                        Err(())
                    }
                })
            })
            .await?;

        tracing::debug!("Matched {target:?}");
        Ok(DiscoveredTarget(target))
    }
}

#[derive(Debug, Clone)]
pub enum TargetQuery {
    /// Attempts to match the nodename, falling back to serial (in that order).
    /// TODO(b/299345828): Make this an exact match by default, fall back to substring matching
    NodenameOrSerial(String),
    AddrPort((TargetAddr, u16)),
    Addr(TargetAddr),
    First,
}

impl TargetQuery {
    fn is_query_on_identity(&self) -> bool {
        matches!(self, TargetQuery::NodenameOrSerial(..) | TargetQuery::First)
    }

    fn is_query_on_address(&self) -> bool {
        matches!(self, TargetQuery::AddrPort(..) | TargetQuery::Addr(..))
    }

    pub fn match_info(&self, t: &TargetEventInfo) -> bool {
        match self {
            Self::NodenameOrSerial(arg) => {
                if let Some(ref nodename) = t.nodename {
                    if nodename.contains(arg) {
                        return true;
                    }
                }
                if let Some(ref serial) = t.serial {
                    if serial.contains(arg) {
                        return true;
                    }
                }
                false
            }
            Self::AddrPort((addr, port)) => {
                let no_port_and_zero = *port == 0 && t.ssh_port.is_none();
                let ports_equal = t.ssh_port.unwrap_or(22) == *port;
                (no_port_and_zero || ports_equal) && Self::Addr(*addr).match_info(t)
            }
            Self::Addr(addr) => t.addresses.iter().any(|a| {
                // If the query does not contain a scope, allow a match against
                // only the IP.
                a == addr || addr.scope_id() == 0 && a.ip() == addr.ip()
            }),
            Self::First => true,
        }
    }

    pub fn matches(&self, t: &Target) -> bool {
        self.match_info(&t.target_info())
    }
}

impl<T> From<Option<T>> for TargetQuery
where
    T: Into<TargetQuery>,
{
    fn from(o: Option<T>) -> Self {
        o.map(Into::into).unwrap_or(Self::First)
    }
}

impl From<&str> for TargetQuery {
    fn from(s: &str) -> Self {
        String::from(s).into()
    }
}

impl From<String> for TargetQuery {
    /// If the string can be parsed as some kind of IP address, will attempt to
    /// match based on that, else fall back to the nodename or serial matches.
    #[tracing::instrument]
    fn from(s: String) -> Self {
        if s == "" {
            return Self::First;
        }
        let (addr, scope, port) = match netext::parse_address_parts(s.as_str()) {
            Ok(r) => r,
            Err(e) => {
                tracing::trace!(
                    "Failed to parse address from '{s}'. Interpreting as nodename: {:?}",
                    e
                );
                return Self::NodenameOrSerial(s);
            }
        };
        // If no such interface exists, just return 0 for a best effort search.
        // This does mean it might be possible to include arbitrary inaccurate scope names for
        // looking up a target, however (like `fe80::1%nonsense`).
        let scope = scope.map(|s| netext::get_verified_scope_id(s).unwrap_or(0)).unwrap_or(0);
        match port {
            Some(p) => Self::AddrPort((TargetAddr::new(addr, scope, 0), p)),
            None => Self::Addr(TargetAddr::new(addr, scope, 0)),
        }
    }
}

impl From<TargetAddr> for TargetQuery {
    fn from(t: TargetAddr) -> Self {
        Self::Addr(t)
    }
}

/// A filter to select targets to update. Unlike `TargetQuery`, this cannot be parsed from a string.
#[derive(Clone, Debug)]
pub enum TargetUpdateFilter<'a> {
    /// Update a target by ID.
    Ids(&'a [u64]),
    /// Update a target by Overnet Node ID.
    OvernetNodeId(u64),
    /// Update a target by (authoritative USB) serial.
    Serial(&'a str),
    /// Update a target by nodename.
    /// For backwards compatibility with mDNS discovery.
    LegacyNodeName(&'a str),
    /// Update a target by net address.
    NetAddrs(&'a [SocketAddr]),
}

impl<'a> TargetUpdateFilter<'a> {
    fn matches(&self, target: &Target) -> bool {
        match *self {
            Self::Ids(ids) => target.has_id(ids.iter()),
            Self::OvernetNodeId(id) => target.overnet_node_id() == Some(id),
            Self::Serial(serial) => {
                Some(serial) == target.identity().as_ref().map(|i| i.serial()).flatten()
            }
            Self::LegacyNodeName(name) => {
                Some(name) == target.identity().as_ref().map(|i| i.name()).flatten()
            }
            Self::NetAddrs(addrs) => {
                let target_addrs = target.addrs.borrow();
                addrs.iter().any(|addr| {
                    // Because of Rust's strange handling of scoped IPv6 addresses, link-local IPv6
                    // address filtering requires special logic to match addresses correctly.
                    match addr {
                        SocketAddr::V6(addr)
                            if addr.ip().is_link_local_addr() && addr.scope_id() == 0 =>
                        {
                            // Wildcard scope
                            target_addrs.iter().any(|entry| {
                                (entry.addr.ip(), entry.addr.port())
                                    == (IpAddr::from(*addr.ip()), addr.port())
                            })
                        }
                        _ => target_addrs.contains(&TargetAddrEntry::new(
                            (*addr).into(),
                            // NOTE: The following fields do not matter for address lookups
                            chrono::MIN_DATETIME,
                            TargetAddrStatus::ssh(),
                        )),
                    }
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{TargetAddrEntry, TargetProtocol, TargetTransport, TargetUpdateBuilder};
    use chrono::{TimeZone, Utc};
    use ffx_daemon_events::TargetConnectionState;
    use fuchsia_async::Task;
    use futures::prelude::*;
    use std::{
        collections::BTreeSet,
        net::{Ipv4Addr, Ipv6Addr},
        pin::Pin,
        task::{Context, Poll},
        time::Instant,
    };

    mod update;

    #[track_caller]
    fn expect_target(tc: &TargetCollection, query: &TargetQuery) -> Rc<Target> {
        tc.query_any_single_target(query, |_| true)
            .expect("Multiple targets found")
            .expect("No target found")
    }

    #[track_caller]
    fn expect_enabled_target(tc: &TargetCollection, query: &TargetQuery) -> Rc<Target> {
        tc.query_single_enabled_target(query)
            .expect("Multiple targets found")
            .expect("No target found")
    }

    #[track_caller]
    fn expect_no_target(tc: &TargetCollection, query: &TargetQuery) {
        let opt = tc.query_any_single_target(query, |_| true).expect("Multiple targets found");
        assert!(opt.is_none(), "Target found")
    }

    #[track_caller]
    fn expect_no_enabled_target(tc: &TargetCollection, query: &TargetQuery) {
        let opt = tc.query_single_enabled_target(query).expect("Multiple targets found");
        assert!(opt.is_none(), "Target found")
    }

    #[track_caller]
    fn expect_ambiguous_target(tc: &TargetCollection, query: &TargetQuery) {
        tc.query_single_enabled_target(query).expect_err("Query not ambiguous");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_insert_new_disabled() {
        let tc = TargetCollection::new_with_queue();
        let nodename = String::from("what");
        let query = nodename.clone().into();
        let t = Target::new_with_time(&nodename, Utc.ymd(2014, 10, 31).and_hms(9, 10, 12));
        tc.merge_insert(t.clone());
        expect_no_enabled_target(&tc, &query);
        assert_eq!(expect_target(&tc, &query), t);
        tc.merge_insert(Target::new_autoconnected(&nodename));
        assert_eq!(expect_enabled_target(&tc, &query), t);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_insert_new() {
        let tc = TargetCollection::new_with_queue();
        let nodename = String::from("what");
        let query = nodename.clone().into();
        let t = Target::new_with_time(&nodename, Utc.ymd(2014, 10, 31).and_hms(9, 10, 12));
        tc.merge_insert(t.clone());
        assert_eq!(expect_target(&tc, &query), t);
        expect_no_target(&tc, &"oihaoih".into())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_merge_evict_old_addresses() {
        let tc = TargetCollection::new_with_queue();
        let nodename = String::from("schplew");
        let t = Target::new_with_time(&nodename, Utc.ymd(2014, 10, 31).and_hms(9, 10, 12));
        let a1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let a2 = IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0xcafe, 0xf00d, 0xf000, 0xb412, 0xb455, 0x1337, 0xfeed,
        ));
        let a3 = IpAddr::V6(Ipv6Addr::new(0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1));
        let tae1 = TargetAddrEntry {
            addr: TargetAddr::new(a1, 1, 0),
            timestamp: Utc.ymd(2014, 10, 31).and_hms(9, 10, 12),
            status: TargetAddrStatus::ssh(),
        };
        let tae2 = TargetAddrEntry {
            addr: TargetAddr::new(a2, 1, 0),
            timestamp: Utc.ymd(2014, 10, 31).and_hms(9, 10, 12),
            status: TargetAddrStatus::ssh(),
        };
        let tae3 = TargetAddrEntry {
            addr: TargetAddr::new(a3, 1, 0),
            timestamp: Utc.ymd(2014, 10, 31).and_hms(9, 10, 12),
            status: TargetAddrStatus::ssh().manually_added(),
        };
        t.addrs.borrow_mut().insert(tae1);
        t.addrs.borrow_mut().insert(tae2);
        t.addrs.borrow_mut().insert(tae3);
        tc.merge_insert(t.clone());
        let t2 = Target::new_with_time(&nodename, Utc.ymd(2014, 11, 2).and_hms(13, 2, 1));
        let a1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
        t2.addrs_insert(TargetAddr::new(a1.clone(), 1, 0));
        let merged_target = tc.merge_insert(t2);
        assert_eq!(merged_target.nodename(), Some(nodename));
        assert_eq!(merged_target.addrs().len(), 2);
        assert!(merged_target.addrs().contains(&TargetAddr::new(a1, 1, 0)));
        assert!(merged_target.addrs().contains(&TargetAddr::new(a3, 1, 0)));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_merge() {
        let tc = TargetCollection::new_with_queue();
        let nodename = String::from("bananas");
        let t1 = Target::new_with_time(&nodename, Utc.ymd(2014, 10, 31).and_hms(9, 10, 12));
        let t2 = Target::new_with_time(&nodename, Utc.ymd(2014, 11, 2).and_hms(13, 2, 1));
        let a1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let a2 = IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0x0000, 0x0000, 0x0000, 0xb412, 0xb455, 0x1337, 0xfeed,
        ));
        t1.addrs_insert(TargetAddr::new(a1.clone(), 1, 0));
        t2.addrs_insert(TargetAddr::new(a2.clone(), 1, 0));
        tc.merge_insert(t2);
        let merged_target = tc.merge_insert(t1);
        assert_eq!(merged_target.addrs().len(), 2);
        assert_eq!(*merged_target.last_response.borrow(), Utc.ymd(2014, 11, 2).and_hms(13, 2, 1));
        assert!(merged_target.addrs().contains(&TargetAddr::new(a1, 1, 0)));
        assert!(merged_target.addrs().contains(&TargetAddr::new(a2, 1, 0)));

        // Insert another instance of the a2 address, but with a missing
        // scope_id, and ensure that the new address does not affect the address
        // collection.
        let t3 = Target::new_with_time(&nodename, Utc.ymd(2014, 10, 31).and_hms(9, 10, 12));
        t3.addrs_insert(TargetAddr::new(a2.clone(), 0, 0));
        let merged_target = tc.merge_insert(t3);
        let addrs = merged_target.addrs();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.contains(&TargetAddr::new(a1, 1, 0)));
        assert!(addrs.contains(&TargetAddr::new(a2, 1, 0)), "does not contain addr: {addrs:?}");

        // Insert another instance of the a2 address, but with a new scope_id, and ensure that the new scope is used.
        let t3 = Target::new_with_time(&nodename, Utc.ymd(2014, 10, 31).and_hms(9, 10, 12));
        t3.addrs_insert(TargetAddr::new(a2.clone(), 3, 0));
        let merged_target = tc.merge_insert(t3);
        assert_eq!(merged_target.addrs().len(), 2);
        assert!(merged_target.addrs().contains(&TargetAddr::new(a1, 1, 0)));
        assert!(merged_target.addrs().contains(&TargetAddr::new(a2, 3, 0)));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_merge_disjointed() {
        let tc = TargetCollection::new_with_queue();

        const NODENAME: &str = "bananas";
        const SERIAL: &str = "pears";

        let t1 = tc.merge_insert(Target::new_named(NODENAME));
        let t2 = tc.merge_insert(Target::new_for_usb(SERIAL));

        // Disjointed, cannot compare
        assert_eq!(
            t1.identity().unwrap().cmp_to(&t2.identity().unwrap()),
            None,
            "{:?} != {:?}",
            t1.identity(),
            t2.identity()
        );

        let addr = "192.0.2.0:55556".parse().unwrap();

        tc.update_target(
            &[TargetUpdateFilter::Serial(SERIAL)],
            TargetUpdateBuilder::new()
                .identity(Identity::try_from_name_serial(Some(NODENAME), Some(SERIAL)).unwrap())
                .discovered(TargetProtocol::Fastboot, TargetTransport::Network)
                .net_addresses(&[addr])
                .build(),
            true,
        );

        assert_eq!(
            t1.identity().unwrap().cmp_to(&t2.identity().unwrap()),
            Some(IdentityCmp::Eq),
            "{:?} != {:?}",
            t1.identity(),
            t2.identity()
        );

        // The most recent matching target should be returned for both queries.
        let query_name = expect_target(&tc, &TargetQuery::NodenameOrSerial(NODENAME.into()));
        let query_serial = expect_target(&tc, &TargetQuery::NodenameOrSerial(SERIAL.into()));
        // Both targets have updated
        assert!(Rc::ptr_eq(&query_name, &t1) || Rc::ptr_eq(&query_name, &t2));
        // Target returned from queries should be consistent.
        assert!(Rc::ptr_eq(&query_name, &query_serial));

        // The state of both targets are merged together when requesting TargetEventInfo:
        let targets = tc.targets(None);
        let [target_info] = &targets[..] else {
            panic!("Too many target info structs: {targets:?}");
        };

        println!("{target_info:?}");
        println!("{:?}", ffx::TargetInfo::from(&*t1));
        println!("{:?}", ffx::TargetInfo::from(&*t2));

        assert_eq!(target_info.nodename.as_deref(), Some(NODENAME));
        assert_eq!(target_info.serial_number.as_deref(), Some(SERIAL));
        assert_eq!(target_info.addresses.as_deref(), Some(&[TargetAddr::from(addr).into()][..]));
        assert_eq!(target_info.target_state, Some(ffx::TargetState::Fastboot));
        assert_eq!(target_info.fastboot_interface, Some(ffx::FastbootInterface::Tcp));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_no_scopeless_ipv6() {
        let tc = TargetCollection::new_with_queue();
        let nodename = String::from("bananas");
        let t1 = Target::new_with_time(&nodename, Utc.ymd(2014, 10, 31).and_hms(9, 10, 12));
        let t2 = Target::new_with_time(&nodename, Utc.ymd(2014, 11, 2).and_hms(13, 2, 1));
        let a1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let a2 = IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0x0000, 0x0000, 0x0000, 0xb412, 0xb455, 0x1337, 0xfeed,
        ));
        t1.addrs_insert(TargetAddr::new(a1.clone(), 0, 0));
        t2.addrs_insert(TargetAddr::new(a2.clone(), 0, 0));
        tc.merge_insert(t2);
        let merged_target = tc.merge_insert(t1);
        assert_eq!(merged_target.addrs().len(), 1);
        assert_eq!(*merged_target.last_response.borrow(), Utc.ymd(2014, 11, 2).and_hms(13, 2, 1));
        assert!(merged_target.addrs().contains(&TargetAddr::new(a1, 0, 0)));
        assert!(!merged_target.addrs().contains(&TargetAddr::new(a2, 0, 0)));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_query_target_by_addr() {
        let ipv4_addr: TargetAddr = TargetAddr::new(IpAddr::from([192, 168, 0, 1]), 0, 0);

        let ipv6_addr: TargetAddr = TargetAddr::new(
            IpAddr::from([0xfe80, 0x0, 0x0, 0x0, 0xdead, 0xbeef, 0xbeef, 0xbeef]),
            3,
            0,
        );

        let t = Target::new_named("foo");
        t.addrs_insert(ipv4_addr);
        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t.clone());

        let ipv4_query = TargetQuery::Addr(ipv4_addr);
        let ipv6_query = TargetQuery::Addr(ipv6_addr);

        assert_eq!(expect_target(&tc, &ipv4_query), t);
        expect_no_target(&tc, &ipv6_query);

        let t = Target::new_named("fooberdoober");
        t.addrs_insert(ipv6_addr);
        tc.merge_insert(t.clone());

        assert_eq!(expect_target(&tc, &ipv6_query), t);
        assert_ne!(expect_target(&tc, &ipv4_query), t);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_new_target_event_synthesis() {
        let t = Target::new_named("clopperdoop");
        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t.clone());
        let vec = tc.synthesize_events().await;
        assert_eq!(vec.len(), 0);
        tc.use_target(t.into(), "test");
        let vec = tc.synthesize_events().await;
        assert_eq!(vec.len(), 1);
        assert_eq!(
            vec.iter().next().expect("events empty"),
            &DaemonEvent::NewTarget(TargetEventInfo {
                nodename: Some("clopperdoop".to_owned()),
                ..Default::default()
            })
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_event_synthesis_all_connected() {
        let t = Target::new_autoconnected("clam-chowder-is-tasty");
        let t2 = Target::new_autoconnected("this-is-a-crunchy-falafel");
        let t3 = Target::new_autoconnected("i-should-probably-eat-lunch");
        let t4 = Target::new_autoconnected("i-should-probably-eat-lunch");
        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t);
        tc.merge_insert(t2);
        tc.merge_insert(t3);
        tc.merge_insert(t4);

        let events = tc.synthesize_events().await;
        assert_eq!(events.len(), 3);
        assert!(events.iter().any(|e| e
            == &DaemonEvent::NewTarget(TargetEventInfo {
                nodename: Some("clam-chowder-is-tasty".to_owned()),
                ..Default::default()
            })));
        assert!(events.iter().any(|e| e
            == &DaemonEvent::NewTarget(TargetEventInfo {
                nodename: Some("this-is-a-crunchy-falafel".to_owned()),
                ..Default::default()
            })));
        assert!(events.iter().any(|e| e
            == &DaemonEvent::NewTarget(TargetEventInfo {
                nodename: Some("i-should-probably-eat-lunch".to_owned()),
                ..Default::default()
            })));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_event_synthesis_none_connected() {
        let t = Target::new_named("clam-chowder-is-tasty");
        let t2 = Target::new_named("this-is-a-crunchy-falafel");
        let t3 = Target::new_named("i-should-probably-eat-lunch");
        let t4 = Target::new_named("i-should-probably-eat-lunch");

        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t);
        tc.merge_insert(t2);
        tc.merge_insert(t3);
        tc.merge_insert(t4);

        let events = tc.synthesize_events().await;
        assert_eq!(events.len(), 0);
    }

    struct EventPusher {
        got: async_channel::Sender<String>,
    }

    impl EventPusher {
        fn new() -> (Self, async_channel::Receiver<String>) {
            let (got, rx) = async_channel::unbounded::<String>();
            (Self { got }, rx)
        }
    }

    #[async_trait(?Send)]
    impl events::EventHandler<DaemonEvent> for EventPusher {
        async fn on_event(&self, event: DaemonEvent) -> Result<events::Status> {
            if let DaemonEvent::NewTarget(TargetEventInfo { nodename: Some(s), .. }) = event {
                self.got.send(s).await.unwrap();
                Ok(events::Status::Waiting)
            } else {
                panic!("this should never receive any other kind of event");
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_collection_events() {
        let t = Target::new_autoconnected("clam-chowder-is-tasty");
        let t2 = Target::new_autoconnected("this-is-a-crunchy-falafel");
        let t3 = Target::new_autoconnected("i-should-probably-eat-lunch");

        let tc = Rc::new(TargetCollection::new());
        let queue = events::Queue::new(&tc);
        let (handler, rx) = EventPusher::new();
        queue.add_handler(handler).await;
        tc.set_event_queue(queue);
        tc.merge_insert(t);
        tc.merge_insert(t2);
        tc.merge_insert(t3);
        let results = rx.take(3).collect::<Vec<_>>().await;
        assert!(results.iter().any(|e| e == &"clam-chowder-is-tasty".to_owned()));
        assert!(results.iter().any(|e| e == &"this-is-a-crunchy-falafel".to_owned()));
        assert!(results.iter().any(|e| e == &"i-should-probably-eat-lunch".to_owned()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_discover_target() {
        let default = "clam-chowder-is-tasty";
        let t = Target::new_autoconnected(default);
        let t2 = Target::new_named("this-is-a-crunchy-falafel");
        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t.clone());

        let query = TargetQuery::from(default.to_owned());
        assert_eq!(tc.discover_target(&query).await.unwrap(), t);
        assert_eq!(tc.discover_target(&TargetQuery::First).await.unwrap(), t);

        let query2 = TargetQuery::from(t2.nodename());
        tc.merge_insert(t2.clone());

        assert_eq!(tc.discover_target(&query).await.unwrap(), t);
        assert_eq!(tc.discover_target(&query2).await.unwrap(), t2);

        // Targets in use are preferred
        assert_eq!(tc.discover_target(&TargetQuery::First).await.unwrap(), t);

        // Find by partial match
        assert_eq!(tc.discover_target(&TargetQuery::from("clam".to_owned())).await.unwrap(), t);

        tc.merge_insert(Target::new_autoconnected("this-is-a-crunchy-falafel"));
        tc.discover_target(&TargetQuery::First).await.unwrap_err(); // Too many targets found
    }

    struct TargetUpdatedFut<F> {
        target_wait_fut: F,
        target_to_add: Rc<Target>,
        collection: Rc<TargetCollection>,
        target_wait_pending: bool,
    }

    /// This is a very specific future that does some things to force a specific state in the
    /// target collection.
    ///
    /// See the test below for the setup as an example.
    ///
    /// The preconditions are:
    /// 1. There is a target with a given address but no nodename in the target collection.
    /// 2. There is a future awaiting a target whose nodename will be added to the collection at a
    ///    later time.
    /// 3. The target we're going to add has the same address as the target already in the target
    ///    collection.
    ///
    /// The execution details are as follows when awaiting this future.
    /// 1. We poll the waiting for the target future until it is pending (flushing the NewTarget
    ///    events out of the event queue).
    /// 2. We add the new target with the matching addresses and nodename.
    /// 3. We await the future passed to this struct which was awaiting said nodename.
    ///
    /// This will succeed iff an UpdatedTarget event is pushed. Without this event this will hang
    /// indefinitely, because when we await a target by its nodename and we encounter the
    /// out-of-date target, we assume the match will never happen, and we wait for a new target
    /// event. The UpdatedTarget event forces the wait_for_target future to re-examine this updated
    /// target to see if it matches.
    impl<F> TargetUpdatedFut<F>
    where
        F: Future<Output = Rc<Target>> + std::marker::Unpin,
    {
        fn new(target_to_add: Rc<Target>, collection: Rc<TargetCollection>, fut: F) -> Self {
            Self { target_wait_fut: fut, target_to_add, collection, target_wait_pending: false }
        }
    }

    impl<F> Future for TargetUpdatedFut<F>
    where
        F: Future<Output = Rc<Target>> + std::marker::Unpin,
    {
        type Output = Rc<Target>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let target_wait_pending = self.target_wait_pending;
            let target_wait_fut = Pin::new(&mut self.target_wait_fut);
            if !target_wait_pending {
                // Flushes the NewTarget event here. Should panic if the target is found.
                match target_wait_fut.poll(cx) {
                    Poll::Ready(target) => {
                        panic!("Found named target when no nodename was included. This should not happen: {:?}", target);
                    }
                    Poll::Pending => {
                        // Once the event has been flushed, inserting a new target will queue up
                        // the UpdatedTarget event.
                        self.target_wait_pending = true;
                        self.collection.merge_insert(self.target_to_add.clone());
                    }
                }
                Poll::Pending
            } else {
                target_wait_fut.poll(cx)
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_discover_target_updated_target() {
        let address = "f111::1";
        let ip = address.parse().unwrap();
        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip, 0, 0));
        let t = Target::new_with_addrs(Option::<String>::None, addr_set);
        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t);
        let target_name = "fesenjoon-is-my-jam";
        let wait_fut = Box::pin(async {
            tc.use_target(
                tc.discover_target(&TargetQuery::from(target_name.to_owned())).await.unwrap(),
                "test",
            )
        });
        // Now we will update the target with a nodename. This should merge into
        // the collection and create an updated target event.
        let t2 = Target::new_autoconnected(target_name);
        t2.addrs.borrow_mut().replace(TargetAddrEntry::new(
            TargetAddr::new(ip, 0, 0),
            Utc::now(),
            TargetAddrStatus::ssh(),
        ));
        let fut = TargetUpdatedFut::new(t2, tc.clone(), wait_fut);
        assert_eq!(fut.await.nodename().unwrap(), target_name);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_merge_no_name() {
        let ip = "f111::3".parse().unwrap();

        // t1 is a target as we would naturally discover it via mdns, or from a
        // user adding it explicitly. That is, the target has a correctly scoped
        // link-local address.
        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip, 0xbadf00d, 0));
        let t1 = Target::new_with_addrs(Option::<String>::None, addr_set);

        // t2 is an incoming target that has the same address, but, it is
        // missing scope information, this is essentially what occurs when we
        // ask the target for its addresses.
        let t2 = Target::new_named("this-is-a-crunchy-falafel");
        t2.addrs.borrow_mut().replace(TargetAddrEntry::new(
            TargetAddr::new(ip, 0xbadf00d, 0),
            Utc::now(),
            TargetAddrStatus::ssh(),
        ));

        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t1);
        tc.merge_insert(t2);
        let targets = tc.targets.borrow();
        let mut targets = targets.values();
        let target = targets.next().expect("Merging resulted in no targets.");
        assert!(targets.next().is_none());
        assert_eq!(target.nodename_str(), "this-is-a-crunchy-falafel");
        let mut addrs = target.addrs().into_iter();
        let addr = addrs.next().expect("Merged target has no address.");
        assert!(addrs.next().is_none());
        assert_eq!(addr, TargetAddr::new(ip, 0xbadf00d, 0));
        assert_eq!(addr.ip(), ip);
        assert_eq!(addr.scope_id(), 0xbadf00d);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_does_not_merge_different_ports_with_no_name() {
        let ip = "fe80::1".parse().unwrap();

        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip, 1, 0));
        let t1 = Target::new_with_addrs(Option::<String>::None, addr_set.clone());
        t1.set_ssh_port(Some(8022));
        let t2 = Target::new_with_addrs(Option::<String>::None, addr_set.clone());
        t2.set_ssh_port(Some(8023));

        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t1);
        tc.merge_insert(t2);

        let targets = tc.targets.borrow();
        let mut targets = Vec::from_iter(targets.values());

        assert_eq!(targets.len(), 2);

        targets.sort_by(|a, b| a.ssh_port().cmp(&b.ssh_port()));
        let mut iter = targets.into_iter();
        let mut found1 = iter.next().expect("must have target one");
        let mut found2 = iter.next().expect("must have target two");

        // Avoid iterator order dependency
        if found1.ssh_port() == Some(8023) {
            std::mem::swap(&mut found1, &mut found2)
        }

        assert_eq!(found1.addrs().into_iter().next().unwrap().ip(), ip);
        assert_eq!(found1.ssh_port(), Some(8022));

        assert_eq!(found2.addrs().into_iter().next().unwrap().ip(), ip);
        assert_eq!(found2.ssh_port(), Some(8023));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_does_not_merge_different_ports() {
        let ip = "fe80::1".parse().unwrap();

        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip, 1, 0));
        let t1 = Target::new_with_addrs(Some("t1"), addr_set.clone());
        t1.set_ssh_port(Some(8022));
        let t2 = Target::new_with_addrs(Some("t2"), addr_set.clone());
        t2.set_ssh_port(Some(8023));

        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t1);
        tc.merge_insert(t2);

        let targets = tc.targets.borrow();
        let mut targets = Vec::from_iter(targets.values());

        assert_eq!(targets.len(), 2);

        targets.sort_by(|a, b| a.ssh_port().cmp(&b.ssh_port()));
        let mut iter = targets.into_iter();
        let found1 = iter.next().expect("must have target one");
        let found2 = iter.next().expect("must have target two");

        assert_eq!(found1.addrs().into_iter().next().unwrap().ip(), ip);
        assert_eq!(found1.ssh_port(), Some(8022));
        assert_eq!(found1.nodename(), Some("t1".to_owned()));

        assert_eq!(found2.addrs().into_iter().next().unwrap().ip(), ip);
        assert_eq!(found2.ssh_port(), Some(8023));
        assert_eq!(found2.nodename(), Some("t2".to_string()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_merge_enabled_and_transient() {
        let tc = TargetCollection::new();

        const NAME: &str = "foo";

        let target = tc.merge_insert(Target::new_named(NAME));

        assert!(!target.is_enabled());
        assert!(!target.is_transient());

        tc.merge_insert({
            let target = Target::new_named(NAME);
            target.enable();
            target
        });

        assert!(target.is_enabled());
        assert!(!target.is_transient());

        tc.merge_insert({
            let target = Target::new_named(NAME);
            target.mark_transient();
            target
        });

        assert!(target.is_enabled());
        assert!(target.is_transient());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_remove_unnamed_by_addr() {
        let ip1 = "f111::3".parse().unwrap();
        let ip2 = "f111::4".parse().unwrap();
        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip1, 0xbadf00d, 0));
        let t1 = Target::new_with_addrs::<String>(None, addr_set);
        let t2 = Target::new_named("this-is-a-crunchy-falafel");
        let tc = TargetCollection::new_with_queue();
        t2.addrs.borrow_mut().replace(TargetAddr::new(ip2, 0, 0).into());
        tc.merge_insert(t1);
        tc.merge_insert(t2);

        {
            let targets = tc.targets.borrow();
            let mut targets = targets.values();
            let mut target1 = targets.next().expect("Merging resulted in no targets.");
            let mut target2 = targets.next().expect("Merging resulted in only one target.");

            if target1.nodename().is_none() {
                std::mem::swap(&mut target1, &mut target2)
            }

            assert!(targets.next().is_none());
            assert_eq!(target1.nodename_str(), "this-is-a-crunchy-falafel");
            assert_eq!(target2.nodename(), None);
        }

        assert!(tc.remove_target("f111::3".to_owned()));

        {
            let targets = tc.targets.borrow();
            let mut targets = targets.values();
            let target = targets.next().expect("Merging resulted in no targets.");
            assert!(targets.next().is_none());
            assert_eq!(target.nodename_str(), "this-is-a-crunchy-falafel");
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_remove_named_by_addr() {
        let ip1 = "f111::3".parse().unwrap();
        let ip2 = "f111::4".parse().unwrap();
        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip1, 0xbadf00d, 0));
        let t1 = Target::new_with_addrs::<String>(None, addr_set);
        let t2 = Target::new_named("this-is-a-crunchy-falafel");
        let tc = TargetCollection::new_with_queue();
        t2.addrs.borrow_mut().replace(TargetAddr::new(ip2, 0, 0).into());
        tc.merge_insert(t1);
        tc.merge_insert(t2);

        {
            let targets = tc.targets.borrow();
            let mut targets = targets.values();
            let mut target1 = targets.next().expect("Merging resulted in no targets.");
            let mut target2 = targets.next().expect("Merging resulted in only one target.");

            if target1.nodename().is_none() {
                std::mem::swap(&mut target1, &mut target2);
            }
            assert!(targets.next().is_none());
            assert_eq!(target1.nodename_str(), "this-is-a-crunchy-falafel");
            assert_eq!(target2.nodename(), None);
        }

        assert!(tc.remove_target("f111::4".to_owned()));

        {
            let targets = tc.targets.borrow();
            let mut targets = targets.values();
            let target = targets.next().expect("Merging resulted in no targets.");
            assert!(targets.next().is_none());
            assert_eq!(target.nodename(), None);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_remove_by_name() {
        let ip1 = "f111::3".parse().unwrap();
        let ip2 = "f111::4".parse().unwrap();
        let mut addr_set = BTreeSet::new();
        addr_set.replace(TargetAddr::new(ip1, 0xbadf00d, 0));
        let t1 = Target::new_with_addrs::<String>(None, addr_set);
        let t2 = Target::new_named("this-is-a-crunchy-falafel");
        let tc = TargetCollection::new_with_queue();
        t2.addrs.borrow_mut().replace(TargetAddr::new(ip2, 0, 0).into());
        tc.merge_insert(t1);
        tc.merge_insert(t2);

        {
            let targets = tc.targets.borrow();
            let mut targets = targets.values();
            let mut target1 = targets.next().expect("Merging resulted in no targets.");
            let mut target2 = targets.next().expect("Merging resulted in only one target.");

            if target1.nodename().is_none() {
                std::mem::swap(&mut target1, &mut target2);
            }

            assert!(targets.next().is_none());
            assert_eq!(target1.nodename_str(), "this-is-a-crunchy-falafel");
            assert_eq!(target2.nodename(), None);
        }

        assert!(tc.remove_target("this-is-a-crunchy-falafel".to_owned()));

        {
            let targets = tc.targets.borrow();
            let mut targets = targets.values();
            let target = targets.next().expect("Merging resulted in no targets.");
            assert!(targets.next().is_none());
            assert_eq!(target.nodename(), None);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_collection_removal_disconnects_target() {
        use crate::target::HostPipeState;
        let local_node = overnet_core::Router::new(None).unwrap();
        let target = Target::new_named("soggy-falafel");
        target.set_state(TargetConnectionState::Mdns(Instant::now()));
        target.host_pipe.borrow_mut().replace(HostPipeState {
            task: Task::local(future::pending()),
            overnet_node: local_node,
            ssh_addr: None,
        });

        let collection = TargetCollection::new();
        collection.merge_insert(target.clone());
        collection.remove_target("soggy-falafel".to_owned());

        assert_eq!(target.get_connection_state(), TargetConnectionState::Disconnected);
        assert!(target.host_pipe.borrow().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_match_serial() {
        let string = "turritopsis-dohrnii-is-an-immortal-jellyfish";
        let t = Target::new_for_usb(string);
        let tc = TargetCollection::new_with_queue();
        tc.merge_insert(t.clone());
        let found_target = expect_target(&tc, &TargetQuery::NodenameOrSerial(string.to_owned()));
        assert_eq!(string, found_target.serial().expect("target should have serial number"));
        assert!(found_target.nodename().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_no_ambiguous_target_when_disabled() {
        let tc = TargetCollection::new_with_queue();

        tc.merge_insert(Target::new_named("this-is-not-connected"));
        tc.merge_insert(Target::new_autoconnected("this-is-connected"));

        let found_target = expect_enabled_target(&tc, &TargetQuery::First);
        assert_eq!(
            "this-is-connected",
            found_target.nodename().expect("target should have nodename")
        );

        let found_target = expect_enabled_target(&tc, &TargetQuery::from("connected".to_owned()));
        assert_eq!(
            "this-is-connected",
            found_target.nodename().expect("target should have nodename")
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_no_ambiguous_target_when_matching_identity() {
        let tc = TargetCollection::new_with_queue();

        let anonymous = tc.merge_insert(Target::new());
        let connected = tc.merge_insert(Target::new_autoconnected("this-is-connected"));
        let other = tc.merge_insert(Target::new_named("this-is-not-connected"));

        tc.merge_insert({
            let updated = Target::new_with_id(anonymous.id());
            updated.replace_identity(Identity::from_name("this-is-connected"));
            updated.enable();
            updated
        });

        for query in [TargetQuery::from("this-is-connected".to_owned()), TargetQuery::First] {
            let found_target = expect_enabled_target(&tc, &query);
            assert!(
                Rc::ptr_eq(&connected, &found_target),
                "expected connected target to be preferred, got {found_target:?}"
            );
        }

        tc.merge_insert({
            let updated = Target::new_with_id(other.id());
            updated.enable();
            updated
        });

        for query in [TargetQuery::from("connected".to_owned()), TargetQuery::First] {
            expect_ambiguous_target(&tc, &query);
        }
    }

    /* TARGET QUERIES */

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_target_query_matches_nodename() {
        let query = TargetQuery::from("foo");
        let target = Rc::new(Target::new_named("foo"));
        assert!(query.matches(&target));
    }

    #[test]
    fn test_target_query_from_socketaddr_both_zero_port() {
        let tq = TargetQuery::from("127.0.0.1:0");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: None,
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && None == ti.ssh_port && port == 0)
        );
        assert!(tq.match_info(&ti));
    }

    #[test]
    fn test_target_query_from_socketaddr_zero_port_to_standard_ssh_port_fails() {
        let tq = TargetQuery::from("127.0.0.1:0");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: Some(22),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(22) == ti.ssh_port && port == 0)
        );
        assert!(!tq.match_info(&ti));
    }

    #[test]
    fn test_target_query_from_socketaddr_standard_port_to_no_port() {
        let tq = TargetQuery::from("127.0.0.1:22");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: None,
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && None == ti.ssh_port && port == 22)
        );
        assert!(tq.match_info(&ti));
    }

    #[test]
    fn test_target_query_from_socketaddr_both_standard_port() {
        let tq = TargetQuery::from("127.0.0.1:22");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: Some(22),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(22) == ti.ssh_port && port == 22)
        );
        assert!(tq.match_info(&ti));
    }

    #[test]
    fn test_target_query_from_socketaddr_random_port_no_target_port_fails() {
        let tq = TargetQuery::from("127.0.0.1:2342");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: None,
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && None == ti.ssh_port && port == 2342)
        );
        assert!(!tq.match_info(&ti));
    }

    #[test]
    fn test_target_query_from_socketaddr_zero_port_to_random_target_port_fails() {
        let tq = TargetQuery::from("127.0.0.1:0");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: Some(2223),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(2223) == ti.ssh_port && port == 0)
        );
        assert!(!tq.match_info(&ti));
    }

    #[test]
    fn test_target_query_from_sockaddr() {
        let tq = TargetQuery::from("127.0.0.1:8022");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: Some(8022),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(port) == ti.ssh_port)
        );
        assert!(tq.match_info(&ti));

        let tq = TargetQuery::from("[::1]:8022");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("::1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: Some(8022),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(port) == ti.ssh_port)
        );
        assert!(tq.match_info(&ti));

        let tq = TargetQuery::from("[::1]");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("::1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: None,
            ..Default::default()
        };
        assert!(matches!(tq, TargetQuery::Addr(addr) if addr == ti.addresses[0]));
        assert!(tq.match_info(&ti));

        let tq = TargetQuery::from("[fe80::1]:22");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("fe80::1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: Some(22),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(port) == ti.ssh_port)
        );
        assert!(tq.match_info(&ti));

        let tq = TargetQuery::from("192.168.0.1:22");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("192.168.0.1".parse::<IpAddr>().unwrap(), 0, 0)],
            ssh_port: Some(22),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(port) == ti.ssh_port)
        );
        assert!(tq.match_info(&ti));

        // Note: socketaddr only supports numeric scopes
        let tq = TargetQuery::from("[fe80::1%1]:22");
        let ti = TargetEventInfo {
            addresses: vec![TargetAddr::new("fe80::1".parse::<IpAddr>().unwrap(), 1, 0)],
            ssh_port: Some(22),
            ..Default::default()
        };
        assert!(
            matches!(tq, TargetQuery::AddrPort((addr, port)) if addr == ti.addresses[0] && Some(port) == ti.ssh_port)
        );
        assert!(tq.match_info(&ti));
    }

    #[test]
    fn test_target_query_from_empty_string() {
        let query = TargetQuery::from(Some(""));
        assert!(matches!(query, TargetQuery::First));
    }

    #[test]
    fn test_target_query_with_no_scope_matches_scoped_target_info() {
        let addr: TargetAddr = TargetAddr::new(
            IpAddr::from([0xfe80, 0x0, 0x0, 0x0, 0xdead, 0xbeef, 0xbeef, 0xbeef]),
            3,
            0,
        );
        let tq = TargetQuery::from("fe80::dead:beef:beef:beef");
        assert!(tq.match_info(&TargetEventInfo { addresses: vec![addr], ..Default::default() }))
    }
}
