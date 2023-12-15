// Copyright 2023 The Fuchsia Authors. All rights 1eserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Config;
use crate::ManualTargets;

use anyhow::{anyhow, Result};
use ffx_fastboot::common::fastboot::tcp_proxy;
use ffx_fastboot::common::fastboot_interface::Fastboot;
use fuchsia_async::{Task, Timer};
use std::collections::BTreeSet;
use std::fmt;
use std::fmt::Display;
use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6, TcpStream};
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum ManualTargetEvent {
    Discovered(ManualTarget, ManualTargetState),
    Lost(ManualTarget),
}

pub trait ManualTargetEventHandler: Send + 'static {
    /// Handles an event.
    fn handle_event(&mut self, event: Result<ManualTargetEvent>);
}

impl<F> ManualTargetEventHandler for F
where
    F: FnMut(Result<ManualTargetEvent>) -> () + Send + 'static,
{
    fn handle_event(&mut self, x: Result<ManualTargetEvent>) -> () {
        self(x)
    }
}

trait ManualTargetTester: Send + 'static {
    /// Gets what state the target is in.
    async fn target_state(&mut self, target: SocketAddr) -> ManualTargetState;
}

struct TcpOpenManualTargetTester;

impl ManualTargetTester for TcpOpenManualTargetTester {
    async fn target_state(&mut self, target: SocketAddr) -> ManualTargetState {
        // If we can open a TcpStream to the target then we consider it to be live
        match TcpStream::connect(target) {
            Ok(_) => {
                // If we can get a fastboot var then we're fastboot
                tracing::trace!("Could connect to SocketAddr: {}. Testing if Fastboot TCP", target);
                match tcp_proxy(&target).await {
                    Ok(mut fastboot_interface) => {
                        if fastboot_interface.get_var(&"version".to_string()).await.is_ok() {
                            tracing::trace!("SocketAddr: {}. Is in Fastboot TCP", target);
                            ManualTargetState::Fastboot
                        } else {
                            tracing::trace!("SocketAddr: {}. Could not get the version varaible. Is Product state.", target);
                            ManualTargetState::Product
                        }
                    }
                    _ => {
                        tracing::trace!("SocketAddr: {}. Could not create TCP Fastboot Proxy. Is Product state.", target);
                        ManualTargetState::Product
                    }
                }
            }
            Err(_) => {
                tracing::trace!(
                    "SocketAddr: {}. Could not Connect over TCP. Is Disconnected.",
                    target
                );
                ManualTargetState::Disconnected
            }
        }
    }
}

pub fn recommended_watcher<F>(event_handler: F) -> Result<ManualTargetWatcher>
where
    F: ManualTargetEventHandler,
{
    Ok(ManualTargetWatcher::new(
        event_handler,
        Config::default(),
        TcpOpenManualTargetTester {},
        Duration::from_secs(1),
    ))
}

pub struct ManualTargetWatcher {
    // Task for the discovery loop
    discovery_task: Option<Task<()>>,
    // Task for the drain loop
    drain_task: Option<Task<()>>,
}

impl ManualTargetWatcher {
    fn new<F, W, O>(event_handler: F, finder: W, opener: O, interval: Duration) -> Self
    where
        F: ManualTargetEventHandler,
        W: ManualTargets + 'static,
        O: ManualTargetTester,
    {
        let mut res = Self { discovery_task: None, drain_task: None };

        let (sender, receiver) = async_channel::bounded::<ManualTargetEvent>(1);

        res.discovery_task.replace(Task::local(discovery_loop(sender, finder, opener, interval)));
        res.drain_task.replace(Task::local(handle_events_loop(receiver, event_handler)));

        res
    }
}

#[derive(Debug, Copy, Clone, PartialOrd, PartialEq, Eq, Ord)]
pub enum ManualTargetState {
    Disconnected,
    Product,
    Fastboot,
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord)]
pub struct ManualTarget {
    addr: SocketAddr,
    lifetime: Option<Duration>,
}

impl ManualTarget {
    pub fn new(addr: SocketAddr, lifetime: Option<Duration>) -> Self {
        Self { addr, lifetime }
    }
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Display for ManualTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lifetime = match self.lifetime {
            Some(l) => format!("{} seconds", l.as_secs()),
            None => "forever".to_string(),
        };
        write!(f, "ManualTarget address: {} lifetime: {}", self.addr, lifetime)
    }
}

async fn parse_manual_targets<F>(finder: &F) -> Vec<ManualTarget>
where
    F: ManualTargets,
{
    let mut retval = vec![];
    for (unparsed_addr, val) in finder.get_or_default().await {
        let (addr, scope, port) = match netext::parse_address_parts(unparsed_addr.as_str()) {
            Ok(res) => res,
            Err(e) => {
                tracing::error!("Skipping load of manual target address due to parsing error '{unparsed_addr}': {e}");
                continue;
            }
        };
        let scope_id = if let Some(scope) = scope {
            match netext::get_verified_scope_id(scope) {
                Ok(res) => res,
                Err(e) => {
                    tracing::error!("Scope load of manual address '{unparsed_addr}', which had a scope ID of '{scope}', which was not verifiable: {e}");
                    continue;
                }
            }
        } else {
            0
        };
        let port = port.unwrap_or(0);
        let sa = match addr {
            IpAddr::V4(i) => SocketAddr::V4(SocketAddrV4::new(i, port)),
            IpAddr::V6(i) => SocketAddr::V6(SocketAddrV6::new(i, port, 0, scope_id)),
        };

        let lifetime = match val.as_u64() {
            Some(secs) => Some(Duration::from_secs(secs)),
            None => None,
        };
        retval.push(ManualTarget { addr: sa, lifetime });
    }
    retval
}

async fn discovery_loop<F, O>(
    events_out: async_channel::Sender<ManualTargetEvent>,
    finder: F,
    mut opener: O,
    discovery_interval: Duration,
) -> ()
where
    F: ManualTargets,
    O: ManualTargetTester,
{
    let mut targets = BTreeSet::<ManualTarget>::new();
    loop {
        // Enumerate interfaces
        let new_targets = parse_manual_targets(&finder).await;
        let new_targets = BTreeSet::from_iter(new_targets);
        tracing::trace!("found targets: {:#?}", new_targets);
        // Update Cache
        for target in &new_targets {
            // Just because the target is found doesnt mean that the target is ready
            let state = opener.target_state(target.addr).await;
            if state == ManualTargetState::Disconnected {
                tracing::debug!("Skipping adding target number: {target} as although it appears to be a product device it is not readily accepting connections");
                if targets.contains(target) {
                    targets.remove(&target);
                    tracing::trace!("Sening lost event for target: {}", target);
                    let _ = events_out.send(ManualTargetEvent::Lost(target.clone())).await;
                    tracing::trace!("Sent lost event for target: {}", target);
                }
                continue;
            }

            tracing::trace!("Inserting new target: {}", target);
            if targets.insert(target.clone()) {
                tracing::trace!("Sending discovered event for target: {}", target);
                let _ = events_out.send(ManualTargetEvent::Discovered(target.clone(), state)).await;
                tracing::trace!("Sent discovered event for target: {}", target);
            }
        }

        // Check for any missing targets
        let missing_targets: Vec<_> = targets.difference(&new_targets).cloned().collect();
        tracing::trace!("missing targets: {:#?}", missing_targets);
        for target in missing_targets {
            targets.remove(&target);
            tracing::trace!("Sening lost event for target: {}", target);
            let _ = events_out.send(ManualTargetEvent::Lost(target.clone())).await;
            tracing::trace!("Sent lost event for target: {}", target);
        }

        tracing::trace!("discovery loop... waiting for {:#?}", discovery_interval);
        Timer::new(discovery_interval).await;
    }
}

async fn handle_events_loop<F>(receiver: async_channel::Receiver<ManualTargetEvent>, mut handler: F)
where
    F: ManualTargetEventHandler,
{
    loop {
        let event = receiver.recv().await.map_err(|e| anyhow!(e));
        tracing::trace!("Event loop received event: {:#?}", event);
        handler.handle_event(event);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::Mock;

    use async_trait::async_trait;
    use futures::channel::mpsc::unbounded;
    use pretty_assertions::assert_eq;
    use serde_json::{json, Map, Value};
    use std::{
        collections::HashMap,
        net::{Ipv4Addr, Ipv6Addr},
        sync::{Arc, Mutex},
    };

    struct TestManualTargets {
        inner_mocks: Mutex<Vec<Mock>>,
        is_empty: Arc<Mutex<bool>>,
    }

    #[async_trait(?Send)]
    impl ManualTargets for TestManualTargets {
        async fn storage_set(&self, _targets: Value) -> Result<()> {
            unimplemented!()
        }

        async fn storage_get(&self) -> Result<Value> {
            unimplemented!()
        }

        async fn get(&self) -> Result<Value> {
            unimplemented!()
        }

        async fn get_or_default(&self) -> Map<String, Value> {
            let mut guard = self.inner_mocks.lock().expect("Get Inner mocks for TestManualTargets");
            if let Some(mock) = guard.pop() {
                mock.get_or_default().await
            } else {
                let mut lock = self.is_empty.lock().unwrap();
                *lock = true;
                Map::new()
            }
        }

        async fn add(&self, _target: String, _expiry: Option<u64>) -> Result<()> {
            unimplemented!()
        }

        async fn remove(&self, _target: String) -> Result<()> {
            unimplemented!()
        }
    }

    struct TestManualTargetTester {
        target_to_is_live: HashMap<SocketAddr, ManualTargetState>,
    }

    impl ManualTargetTester for TestManualTargetTester {
        async fn target_state(&mut self, target: SocketAddr) -> ManualTargetState {
            *self.target_to_is_live.get(&target).unwrap()
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_manual_target_watcher() -> Result<()> {
        let empty_signal = Arc::new(Mutex::new(false));

        let mut map_one = Map::new();
        map_one.insert("127.0.0.1:8022".to_string(), json!(null));
        map_one.insert("[::1]:8023".to_string(), json!(null));
        map_one.insert("127.0.0.1:8024".to_string(), json!(null));
        let target_finder_one = Mock::new(map_one);

        // We lose the ipv6 address
        let mut map_two = Map::new();
        map_two.insert("127.0.0.1:8022".to_string(), json!(null));
        map_two.insert("127.0.0.1:8024".to_string(), json!(null));
        let target_finder_two = Mock::new(map_two);

        // Get the ipv6 address back
        let mut map_three = Map::new();
        map_three.insert("127.0.0.1:8022".to_string(), json!(null));
        map_three.insert("[::1]:8023".to_string(), json!(null));
        map_three.insert("127.0.0.1:8024".to_string(), json!(null));
        let target_finder_three = Mock::new(map_three);

        let inner_mocks =
            Mutex::new(vec![target_finder_one, target_finder_two, target_finder_three]);

        let target_finder = TestManualTargets { inner_mocks, is_empty: empty_signal.clone() };

        let mut target_to_is_live = HashMap::new();
        target_to_is_live.insert(
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1), 8023, 0, 0)),
            ManualTargetState::Fastboot,
        );
        target_to_is_live.insert(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 8022)),
            ManualTargetState::Product,
        );
        // Since this is not in product then it should not appear in our results
        target_to_is_live.insert(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 8024)),
            ManualTargetState::Disconnected,
        );

        let product_tester = TestManualTargetTester { target_to_is_live };

        let (sender, mut queue) = unbounded();
        let watcher = ManualTargetWatcher::new(
            move |res: Result<ManualTargetEvent>| {
                let _ = sender.unbounded_send(res);
            },
            target_finder,
            product_tester,
            Duration::from_millis(1),
        );

        while !*empty_signal.lock().unwrap() {
            // Wait a tiny bit so the watcher can drain the finder queue
            Timer::new(Duration::from_millis(1)).await;
        }

        drop(watcher);
        let mut events = Vec::<ManualTargetEvent>::new();
        while let Ok(Some(event)) = queue.try_next() {
            events.push(event.unwrap());
        }

        let manual_target_v6 = ManualTarget {
            addr: SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1),
                8023,
                0,
                0,
            )),
            lifetime: None,
        };
        let manual_target_v4 = ManualTarget {
            addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 8022)),
            lifetime: None,
        };
        // Assert state of events
        assert_eq!(events.len(), 6);
        assert_eq!(
            &events,
            &vec![
                // First set of discovery events
                ManualTargetEvent::Discovered(manual_target_v4.clone(), ManualTargetState::Product),
                ManualTargetEvent::Discovered(
                    manual_target_v6.clone(),
                    ManualTargetState::Fastboot
                ),
                // Second set of discovery events
                ManualTargetEvent::Lost(manual_target_v6.clone()),
                // Third set of discovery events
                ManualTargetEvent::Discovered(
                    manual_target_v6.clone(),
                    ManualTargetState::Fastboot
                ),
                // Last set... there are no more items left in the queue
                // so we lose all targets.
                ManualTargetEvent::Lost(manual_target_v4.clone()),
                ManualTargetEvent::Lost(manual_target_v6.clone()),
            ]
        );
        // Reiterating... target 127.0.0.1:8024 was not in product so it should not appear in our results
        Ok(())
    }

    struct AlternatingManualTargetTester {
        target_call_count: HashMap<SocketAddr, u32>,
    }

    impl ManualTargetTester for AlternatingManualTargetTester {
        async fn target_state(&mut self, target: SocketAddr) -> ManualTargetState {
            let state = match self.target_call_count.get(&target).unwrap() % 2 {
                0 => ManualTargetState::Disconnected,
                _ => ManualTargetState::Product,
            };

            let val = self.target_call_count.get_mut(&target).unwrap();
            *val = *val + 1;
            state
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_manual_target_change_state() -> Result<()> {
        let empty_signal = Arc::new(Mutex::new(false));

        let mut map_one = Map::new();
        map_one.insert("127.0.0.1:8022".to_string(), json!(null));
        let target_finder_one = Mock::new(map_one);

        let mut map_two = Map::new();
        map_two.insert("127.0.0.1:8022".to_string(), json!(null));
        let target_finder_two = Mock::new(map_two);

        let mut map_three = Map::new();
        map_three.insert("127.0.0.1:8022".to_string(), json!(null));
        let target_finder_three = Mock::new(map_three);

        let inner_mocks =
            Mutex::new(vec![target_finder_one, target_finder_two, target_finder_three]);

        let target_finder = TestManualTargets { inner_mocks, is_empty: empty_signal.clone() };

        let mut target_call_count = HashMap::new();
        target_call_count
            .insert(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 8022)), 1);
        let product_tester = AlternatingManualTargetTester { target_call_count };

        let (sender, mut queue) = unbounded();
        let watcher = ManualTargetWatcher::new(
            move |res: Result<ManualTargetEvent>| {
                let _ = sender.unbounded_send(res);
            },
            target_finder,
            product_tester,
            Duration::from_millis(1),
        );

        while !*empty_signal.lock().unwrap() {
            // Wait a tiny bit so the watcher can drain the finder queue
            Timer::new(Duration::from_millis(1)).await;
        }

        drop(watcher);
        let mut events = Vec::<ManualTargetEvent>::new();
        while let Ok(Some(event)) = queue.try_next() {
            events.push(event.unwrap());
        }

        let manual_target_v4 = ManualTarget {
            addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 8022)),
            lifetime: None,
        };
        // Assert state of events
        assert_eq!(events.len(), 4);
        assert_eq!(
            &events,
            &vec![
                // First set of discovery events
                ManualTargetEvent::Discovered(manual_target_v4.clone(), ManualTargetState::Product),
                // Second time it is discovered (via the config) it is Disconnected
                // so we send a Lost event
                ManualTargetEvent::Lost(manual_target_v4.clone()),
                // Third time it is discovered (via the config) it is Product
                // so we send a Discovered event
                ManualTargetEvent::Discovered(manual_target_v4.clone(), ManualTargetState::Product),
                // The queue is drained all targets disappear. It is lost
                ManualTargetEvent::Lost(manual_target_v4.clone()),
            ]
        );

        Ok(())
    }
}
