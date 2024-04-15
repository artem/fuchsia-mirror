// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl_fuchsia_bluetooth_sys::{
        AccessConnectResult, AccessDisconnectResult, AccessMakeDiscoverableResult, AccessMarker,
        AccessProxy, AccessStartDiscoveryResult, ProcedureTokenMarker,
    },
    fuchsia_bluetooth::{
        expectation::asynchronous::{expectable, Expectable, ExpectableExt, ExpectableState},
        types::{Peer, PeerId},
    },
    futures::future::{self, BoxFuture, FutureExt},
    std::{
        collections::HashMap,
        ops::{Deref, DerefMut},
        sync::Arc,
    },
    test_harness::{SharedState, TestHarness},
    tracing::warn,
};

use crate::core_realm::{CoreRealm, SHARED_STATE_INDEX};

/// This wrapper class prevents test code from invoking WatchPeers, a hanging-get method which fails
/// if invoked again before the prior invocation has returned. The AccessHarness itself continuously
/// monitors WatchPeers, so if test code is also permitted to invoke WatchPeers, tests could fail
/// (or worse, flake, since the multiple-calls issue is timing-dependent). Instead, test code should
/// check peer state via AccessState.peers.
#[derive(Clone)]
pub struct AccessWrapper(AccessProxy);

impl AccessWrapper {
    pub fn start_discovery(
        &self,
        token: fidl::endpoints::ServerEnd<ProcedureTokenMarker>,
    ) -> fidl::client::QueryResponseFut<AccessStartDiscoveryResult> {
        self.0.start_discovery(token)
    }

    pub fn make_discoverable(
        &self,
        token: fidl::endpoints::ServerEnd<ProcedureTokenMarker>,
    ) -> fidl::client::QueryResponseFut<AccessMakeDiscoverableResult> {
        self.0.make_discoverable(token)
    }

    pub fn connect(
        &self,
        id: &mut fidl_fuchsia_bluetooth::PeerId,
    ) -> fidl::client::QueryResponseFut<AccessConnectResult> {
        self.0.connect(id)
    }

    pub fn disconnect(
        &self,
        id: &mut fidl_fuchsia_bluetooth::PeerId,
    ) -> fidl::client::QueryResponseFut<AccessDisconnectResult> {
        self.0.disconnect(id)
    }

    pub fn set_local_name(&self, name: &str) -> Result<(), fidl::Error> {
        self.0.set_local_name(name)
    }
}

#[derive(Clone, Default)]
pub struct AccessState {
    /// Remote Peers seen
    pub peers: HashMap<PeerId, Peer>,
}

#[derive(Clone)]
pub struct AccessHarness(Expectable<AccessState, AccessWrapper>);

impl Deref for AccessHarness {
    type Target = Expectable<AccessState, AccessWrapper>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for AccessHarness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

async fn update_peer_state(harness: &AccessHarness) -> Result<(), Error> {
    let access = harness.aux().clone();
    let (updated, removed) =
        access.0.watch_peers().await.context("Error calling Access.watch_peers()")?;
    for peer in updated.into_iter() {
        let peer: Peer = peer.try_into().context("Invalid peer received from WatchPeers()")?;
        let _ = harness.write_state().peers.insert(peer.id, peer);
    }
    for id in removed.into_iter() {
        let id = id.into();
        if harness.write_state().peers.remove(&id).is_none() {
            warn!(%id, "Unknown peer removed from peer state");
        }
    }
    harness.notify_state_changed();
    Ok(())
}

async fn run_peer_watcher(harness: AccessHarness) -> Result<(), Error> {
    loop {
        update_peer_state(&harness).await?;
    }
}

impl TestHarness for AccessHarness {
    type Env = Arc<CoreRealm>;
    type Runner = BoxFuture<'static, Result<(), Error>>;

    fn init(
        shared_state: &Arc<SharedState>,
    ) -> BoxFuture<'static, Result<(Self, Self::Env, Self::Runner), Error>> {
        let shared_state = shared_state.clone();
        async move {
            let realm =
                shared_state.get_or_insert_with(SHARED_STATE_INDEX, CoreRealm::create).await?;
            let access = realm
                .instance()
                .connect_to_protocol_at_exposed_dir::<AccessMarker>()
                .context("Failed to connect to access service")?;

            let harness = AccessHarness(expectable(Default::default(), AccessWrapper(access)));

            // Ensure that the harness' peer state is accurate when initialization is finished.
            update_peer_state(&harness).await?;
            let run_access = run_peer_watcher(harness.clone()).boxed();
            Ok((harness, realm, run_access))
        }
        .boxed()
    }
    fn terminate(_env: Self::Env) -> BoxFuture<'static, Result<(), Error>> {
        future::ok(()).boxed()
    }
}

pub mod expectation {
    use super::*;
    use crate::host_watcher::HostWatcherState;
    use fuchsia_bluetooth::{
        expectation::Predicate,
        types::{Address, HostId, HostInfo, Peer, PeerId, Uuid},
    };

    mod peer {
        use super::*;

        pub(crate) fn exists(p: Predicate<Peer>) -> Predicate<AccessState> {
            let msg = format!("peer exists satisfying {:?}", p);
            Predicate::predicate(
                move |state: &AccessState| state.peers.iter().any(|(_, d)| p.satisfied(d)),
                &msg,
            )
        }

        pub(crate) fn with_identifier(id: PeerId) -> Predicate<Peer> {
            Predicate::<Peer>::predicate(move |d| d.id == id, &format!("identifier == {}", id))
        }

        pub(crate) fn with_address(address: Address) -> Predicate<Peer> {
            Predicate::<Peer>::predicate(
                move |d| d.address == address,
                &format!("address == {}", address),
            )
        }

        pub(crate) fn connected(connected: bool) -> Predicate<Peer> {
            Predicate::<Peer>::predicate(
                move |d| d.connected == connected,
                &format!("connected == {}", connected),
            )
        }

        pub(crate) fn with_bredr_service(service_uuid: Uuid) -> Predicate<Peer> {
            let msg = format!("bredr_services.contains({})", service_uuid.to_string());
            Predicate::<Peer>::predicate(move |d| d.bredr_services.contains(&service_uuid), &msg)
        }
    }

    mod host {
        use super::*;

        pub(crate) fn with_name<S: ToString>(name: S) -> Predicate<HostInfo> {
            let name = name.to_string();
            let msg = format!("name == {}", name);
            Predicate::<HostInfo>::predicate(move |h| h.local_name.as_ref() == Some(&name), &msg)
        }

        pub(crate) fn with_id(id: HostId) -> Predicate<HostInfo> {
            let msg = format!("id == {}", id);
            Predicate::<HostInfo>::predicate(move |h| h.id == id, &msg)
        }

        pub(crate) fn discovering(is_discovering: bool) -> Predicate<HostInfo> {
            let msg = format!("discovering == {}", is_discovering);
            Predicate::<HostInfo>::predicate(move |h| h.discovering == is_discovering, &msg)
        }

        pub(crate) fn discoverable(is_discoverable: bool) -> Predicate<HostInfo> {
            let msg = format!("discoverable == {}", is_discoverable);
            Predicate::<HostInfo>::predicate(move |h| h.discoverable == is_discoverable, &msg)
        }

        pub(crate) fn exists(p: Predicate<HostInfo>) -> Predicate<HostWatcherState> {
            let msg = format!("Host exists satisfying {:?}", p);
            Predicate::predicate(
                move |state: &HostWatcherState| state.hosts.values().any(|h| p.satisfied(h)),
                &msg,
            )
        }
    }

    pub fn peer_connected(id: PeerId, connected: bool) -> Predicate<AccessState> {
        peer::exists(peer::with_identifier(id).and(peer::connected(connected)))
    }

    pub fn peer_with_address(address: Address) -> Predicate<AccessState> {
        peer::exists(peer::with_address(address))
    }

    pub fn host_with_name<S: ToString>(name: S) -> Predicate<HostWatcherState> {
        host::exists(host::with_name(name))
    }

    pub fn peer_bredr_service_discovered(id: PeerId, service_uuid: Uuid) -> Predicate<AccessState> {
        peer::exists(peer::with_identifier(id).and(peer::with_bredr_service(service_uuid)))
    }

    pub fn host_discovering(id: HostId, is_discovering: bool) -> Predicate<HostWatcherState> {
        host::exists(host::with_id(id).and(host::discovering(is_discovering)))
    }
    pub fn host_discoverable(id: HostId, is_discoverable: bool) -> Predicate<HostWatcherState> {
        host::exists(host::with_id(id).and(host::discoverable(is_discoverable)))
    }
}
