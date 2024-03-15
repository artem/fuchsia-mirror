// Copyright 2019 The Fuchsia Authors
//
// Licensed under a BSD-style license <LICENSE-BSD>, Apache License, Version 2.0
// <LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0>, or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to
// those terms.

use crate::{
    common::{App, CheckOptions, ProtocolState, UpdateCheckSchedule},
    installer::stub::StubPlan,
    policy::{CheckDecision, CheckTiming, PolicyEngine, UpdateDecision},
    time::MockTimeSource,
};
use futures::future::BoxFuture;
use futures::prelude::*;
use std::{cell::RefCell, rc::Rc};

/// A mock PolicyEngine that returns mocked data.
#[derive(Debug)]
pub struct MockPolicyEngine {
    pub check_timing: Option<CheckTiming>,
    pub check_decision: CheckDecision,
    pub update_decision: UpdateDecision,
    pub time_source: MockTimeSource,
    pub reboot_allowed: Rc<RefCell<bool>>,
    pub reboot_needed: Rc<RefCell<bool>>,
    pub reboot_check_options_received: Rc<RefCell<Vec<CheckOptions>>>,
}

impl Default for MockPolicyEngine {
    fn default() -> Self {
        Self {
            check_timing: Default::default(),
            check_decision: Default::default(),
            update_decision: Default::default(),
            time_source: MockTimeSource::new_from_now(),
            reboot_allowed: Rc::new(RefCell::new(true)),
            reboot_needed: Rc::new(RefCell::new(true)),
            reboot_check_options_received: Rc::new(RefCell::new(vec![])),
        }
    }
}

impl PolicyEngine for MockPolicyEngine {
    type TimeSource = MockTimeSource;
    type InstallResult = ();
    type InstallPlan = StubPlan;

    fn time_source(&self) -> &Self::TimeSource {
        &self.time_source
    }

    fn compute_next_update_time(
        &mut self,
        _apps: &[App],
        _scheduling: &UpdateCheckSchedule,
        _protocol_state: &ProtocolState,
    ) -> BoxFuture<'_, CheckTiming> {
        future::ready(self.check_timing.unwrap()).boxed()
    }

    fn update_check_allowed(
        &mut self,
        _apps: &[App],
        _scheduling: &UpdateCheckSchedule,
        _protocol_state: &ProtocolState,
        _check_options: &CheckOptions,
    ) -> BoxFuture<'_, CheckDecision> {
        future::ready(self.check_decision.clone()).boxed()
    }

    fn update_can_start<'p>(
        &mut self,
        _proposed_install_plan: &'p Self::InstallPlan,
    ) -> BoxFuture<'p, UpdateDecision> {
        future::ready(self.update_decision.clone()).boxed()
    }

    fn reboot_allowed(
        &mut self,
        check_options: &CheckOptions,
        _install_result: &Self::InstallResult,
    ) -> BoxFuture<'_, bool> {
        (*self.reboot_check_options_received.borrow_mut()).push(check_options.clone());
        future::ready(*self.reboot_allowed.borrow()).boxed()
    }

    fn reboot_needed(&mut self, _install_plan: &Self::InstallPlan) -> BoxFuture<'_, bool> {
        future::ready(*self.reboot_needed.borrow()).boxed()
    }
}
