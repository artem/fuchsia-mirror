// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Error};
use carnelian::input::consumer_control::Phase;
use fidl_fuchsia_input_report::ConsumerControlButton;
use fidl_fuchsia_recovery::FactoryResetMarker;
use fuchsia_component::client::connect_to_protocol;

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum FactoryResetState {
    Waiting,
    AwaitingPolicy(usize),
    StartCountdown,
    CancelCountdown,
    ExecuteReset,
    AwaitingReset,
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum ResetEvent {
    ButtonPress(ConsumerControlButton, Phase),
    AwaitPolicyResult(usize, bool),
    CountdownFinished,
    CountdownCancelled,
}

pub struct FactoryResetStateMachine {
    volume_up_phase: Phase,
    volume_down_phase: Phase,
    state: FactoryResetState,
    last_policy_check_id: usize,
}

impl FactoryResetStateMachine {
    pub fn new() -> FactoryResetStateMachine {
        FactoryResetStateMachine {
            volume_down_phase: Phase::Up,
            volume_up_phase: Phase::Up,
            state: FactoryResetState::Waiting,
            last_policy_check_id: 0,
        }
    }

    pub fn is_counting_down(&self) -> bool {
        self.state == FactoryResetState::StartCountdown
    }

    fn update_button_state(&mut self, button: ConsumerControlButton, phase: Phase) {
        match button {
            ConsumerControlButton::VolumeUp => self.volume_up_phase = phase,
            ConsumerControlButton::VolumeDown => self.volume_down_phase = phase,
            _ => panic!("Invalid button provided {:?}", button),
        };
    }

    fn check_buttons_pressed(&self) -> bool {
        match (self.volume_up_phase, self.volume_down_phase) {
            (Phase::Down, Phase::Down) => true,
            _ => false,
        }
    }

    /// Updates the state machine's button state on button presses and returns
    /// the new state and a boolean indicating if that new state is changed from
    /// the previous state.
    pub fn handle_event(&mut self, event: ResetEvent) -> FactoryResetState {
        if let ResetEvent::ButtonPress(button, phase) = event {
            // Handle all volume button events here to make sure we don't miss anything in default matches
            self.update_button_state(button, phase);
        }
        let new_state = match self.state {
            FactoryResetState::Waiting => match event {
                ResetEvent::ButtonPress(_, _) => {
                    if self.check_buttons_pressed() {
                        self.last_policy_check_id += 1;
                        FactoryResetState::AwaitingPolicy(self.last_policy_check_id)
                    } else {
                        FactoryResetState::Waiting
                    }
                }
                ResetEvent::CountdownFinished => {
                    panic!("Not expecting timer updates when in waiting state")
                }
                ResetEvent::CountdownCancelled | ResetEvent::AwaitPolicyResult(_, _) => {
                    FactoryResetState::Waiting
                }
            },
            FactoryResetState::AwaitingPolicy(check_id) => match event {
                ResetEvent::ButtonPress(_, _) => {
                    if !self.check_buttons_pressed() {
                        FactoryResetState::Waiting
                    } else {
                        FactoryResetState::AwaitingPolicy(check_id)
                    }
                }
                ResetEvent::AwaitPolicyResult(check_id, fdr_enabled)
                    if check_id == self.last_policy_check_id =>
                {
                    if fdr_enabled {
                        println!("recovery: start reset countdown");
                        FactoryResetState::StartCountdown
                    } else {
                        FactoryResetState::Waiting
                    }
                }
                _ => FactoryResetState::Waiting,
            },
            FactoryResetState::StartCountdown => match event {
                ResetEvent::ButtonPress(_, _) => {
                    if self.check_buttons_pressed() {
                        panic!(
                            "Not expecting both buttons to be pressed while in StartCountdown state"
                        );
                    } else {
                        println!("recovery: cancel reset countdown");
                        FactoryResetState::CancelCountdown
                    }
                }
                ResetEvent::CountdownCancelled => {
                    panic!(
                        "Not expecting CountdownCancelled here, expecting input event to \
                                move to CancelCountdown state first."
                    );
                }
                ResetEvent::CountdownFinished => {
                    println!("recovery: execute factory reset");
                    FactoryResetState::ExecuteReset
                }
                ResetEvent::AwaitPolicyResult(_, _) => FactoryResetState::StartCountdown,
            },
            FactoryResetState::CancelCountdown => match event {
                ResetEvent::CountdownCancelled => FactoryResetState::Waiting,
                ResetEvent::AwaitPolicyResult(_, _) | ResetEvent::ButtonPress(_, _) => {
                    FactoryResetState::CancelCountdown
                }
                _ => panic!("Only expecting CountdownCancelled event in CancelCountdown state."),
            },
            FactoryResetState::ExecuteReset => match event {
                ResetEvent::AwaitPolicyResult(_, _) => FactoryResetState::AwaitingReset,
                // Subsequent button presses should not trigger additional reset calls.
                ResetEvent::ButtonPress(_, _) => FactoryResetState::AwaitingReset,
                _ => {
                    panic!("Not expecting countdown events while in ExecuteReset state")
                }
            },
            FactoryResetState::AwaitingReset => match event {
                ResetEvent::AwaitPolicyResult(_, _) => FactoryResetState::AwaitingReset,
                ResetEvent::ButtonPress(_, _) => FactoryResetState::AwaitingReset,
                _ => panic!("Not expecting countdown events while in ExecuteReset state"),
            },
        };

        self.state = new_state;
        new_state
    }

    #[cfg(test)]
    pub fn get_state(&self) -> FactoryResetState {
        return self.state;
    }
}

pub async fn execute_reset() -> Result<(), Error> {
    let factory_reset_service = connect_to_protocol::<FactoryResetMarker>();
    let proxy = match factory_reset_service {
        Ok(marker) => marker.clone(),
        Err(error) => {
            bail!("Could not connect to factory_reset_service: {:?}", error);
        }
    };

    println!("recovery: Executing factory reset command");

    match proxy.reset().await {
        Ok(_) => {}
        Err(error) => {
            bail!("Error executing factory reset command : {:?}", error);
        }
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reset_complete() -> std::result::Result<(), anyhow::Error> {
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine.get_state();
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(1));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(1, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
        let state = state_machine.handle_event(ResetEvent::CountdownFinished);
        assert_eq!(state, FactoryResetState::ExecuteReset);
        Ok(())
    }

    #[test]
    fn test_reset_complete_reverse() -> std::result::Result<(), anyhow::Error> {
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine.get_state();
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(1));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(1, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
        let state = state_machine.handle_event(ResetEvent::CountdownFinished);
        assert_eq!(state, FactoryResetState::ExecuteReset);
        Ok(())
    }

    #[test]
    fn test_reset_cancelled() -> std::result::Result<(), anyhow::Error> {
        test_reset_cancelled_button(ConsumerControlButton::VolumeUp);
        test_reset_cancelled_button(ConsumerControlButton::VolumeDown);
        Ok(())
    }

    fn test_reset_cancelled_button(button: ConsumerControlButton) {
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(1));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(1, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
        let state = state_machine.handle_event(ResetEvent::ButtonPress(button, Phase::Up));
        assert_eq!(state, FactoryResetState::CancelCountdown);
        let state = state_machine.handle_event(ResetEvent::CountdownCancelled);
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine.handle_event(ResetEvent::ButtonPress(button, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(2));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(2, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
    }

    #[test]
    #[should_panic]
    fn test_early_complete_countdown() {
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let _state = state_machine.handle_event(ResetEvent::CountdownFinished);
    }

    #[test]
    #[should_panic]
    fn test_cancelled_countdown_not_complete() {
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(1));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(1, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Up));
        assert_eq!(state, FactoryResetState::CancelCountdown);
        let _state = state_machine.handle_event(ResetEvent::CountdownFinished);
    }

    #[test]
    fn test_cancelled_countdown_with_extra_button_press() {
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(1));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(1, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Up));
        assert_eq!(state, FactoryResetState::CancelCountdown);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Up));
        assert_eq!(state, FactoryResetState::CancelCountdown);
    }

    #[test]
    fn test_reset_complete_button_released() -> std::result::Result<(), anyhow::Error> {
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine.get_state();
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(1));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(1, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
        let state = state_machine.handle_event(ResetEvent::CountdownFinished);
        assert_eq!(state, FactoryResetState::ExecuteReset);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Up));
        assert_eq!(state, FactoryResetState::AwaitingReset);
        Ok(())
    }

    #[test]
    fn test_reset_complete_multiple_button_presses() -> std::result::Result<(), anyhow::Error> {
        // Multiple button presses should leave the state machine in AwaitingReset.
        let mut state_machine = FactoryResetStateMachine::new();
        let state = state_machine.get_state();
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::Waiting);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingPolicy(1));
        let state = state_machine.handle_event(ResetEvent::AwaitPolicyResult(1, true));
        assert_eq!(state, FactoryResetState::StartCountdown);
        let state = state_machine.handle_event(ResetEvent::CountdownFinished);
        assert_eq!(state, FactoryResetState::ExecuteReset);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Up));
        assert_eq!(state, FactoryResetState::AwaitingReset);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeDown, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingReset);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Up));
        assert_eq!(state, FactoryResetState::AwaitingReset);
        let state = state_machine
            .handle_event(ResetEvent::ButtonPress(ConsumerControlButton::VolumeUp, Phase::Down));
        assert_eq!(state, FactoryResetState::AwaitingReset);
        Ok(())
    }
}
