// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use at_commands as at;
use tracing::warn;

use super::at_resp;
use super::{Procedure, ProcedureInput, ProcedureOutput};

use crate::peer::procedure_manipulated_state::ProcedureManipulatedState;

/// This implementation supports the 7 indicators defined in HFP v1.8 Section 4.35.
/// The indices of these indicators are fixed.
const SERVICE_INDICATOR_INDEX: i64 = 1;
const CALL_INDICATOR_INDEX: i64 = 2;
const CALL_SETUP_INDICATOR_INDEX: i64 = 3;
const CALL_HELD_INDICATOR_INDEX: i64 = 4;
const SIGNAL_INDICATOR_INDEX: i64 = 5;
const ROAM_INDICATOR_INDEX: i64 = 6;
const BATT_CHG_INDICATOR_INDEX: i64 = 7;

#[derive(Debug)]
pub struct PhoneStatusProcedure {
    // Whether the procedure has sent the phone status to the HF.
    terminated: bool,
}

impl Procedure<ProcedureInput, ProcedureOutput> for PhoneStatusProcedure {
    fn new() -> Self {
        Self { terminated: false }
    }

    fn name(&self) -> &str {
        "Phone Status"
    }

    fn transition(
        &mut self,
        state: &mut ProcedureManipulatedState,
        input: ProcedureInput,
    ) -> Result<Vec<ProcedureOutput>, Error> {
        match input {
            at_resp!(Ciev { ind, value }) if !state.indicators_update_enabled => {
                warn!(
                    "Received indicator {:} with value {:} when indicator update is disabled.",
                    ind, value
                )
            }
            at_resp!(Ciev { ind: SERVICE_INDICATOR_INDEX, value }) => {
                state.ag_indicators.service.set_if_enabled(value != 0);
            }
            at_resp!(Ciev { ind: CALL_INDICATOR_INDEX, value }) => {
                state.ag_indicators.call.set_if_enabled(value != 0);
            }
            at_resp!(Ciev { ind: CALL_SETUP_INDICATOR_INDEX, value }) => {
                state.ag_indicators.callsetup.set_if_enabled(value as u8);
            }
            at_resp!(Ciev { ind: CALL_HELD_INDICATOR_INDEX, value }) => {
                state.ag_indicators.callheld.set_if_enabled(value as u8);
            }
            at_resp!(Ciev { ind: SIGNAL_INDICATOR_INDEX, value }) => {
                state.ag_indicators.signal.set_if_enabled(value as u8);
            }
            at_resp!(Ciev { ind: ROAM_INDICATOR_INDEX, value }) => {
                state.ag_indicators.roam.set_if_enabled(value != 0);
            }
            at_resp!(Ciev { ind: BATT_CHG_INDICATOR_INDEX, value }) => {
                state.ag_indicators.battchg.set_if_enabled(value as u8);
            }
            _ => {
                return Err(format_err!(
                    "Received invalid response during a phone status update procedure: {:?}",
                    input
                ));
            }
        }
        self.terminated = true;
        Ok(vec![])
    }

    fn is_terminated(&self) -> bool {
        self.terminated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;

    use crate::config::HandsFreeFeatureSupport;
    use crate::peer::procedure::at_ok;

    #[fuchsia::test]
    fn update_with_invalid_response_returns_error() {
        let mut procedure = PhoneStatusProcedure::new();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);
        let response = at_ok!();

        assert!(!procedure.is_terminated());

        assert_matches!(procedure.transition(&mut state, response), Err(_));

        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn update_properly_changes_value() {
        let mut procedure = PhoneStatusProcedure::new();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        state.ag_indicators.set_default_values();

        assert!(!procedure.is_terminated());

        assert_eq!(state.ag_indicators.service.value.unwrap(), false);
        assert_eq!(state.ag_indicators.call.value.unwrap(), false);
        assert_eq!(state.ag_indicators.callsetup.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.callheld.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.signal.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.roam.value.unwrap(), false);
        assert_eq!(state.ag_indicators.battchg.value.unwrap(), 0);

        let response = at_resp!(Ciev { ind: SERVICE_INDICATOR_INDEX, value: 1 });
        assert_matches!(procedure.transition(&mut state, response), Ok(_));
        assert_eq!(state.ag_indicators.service.value.unwrap(), true);

        let response = at_resp!(Ciev { ind: CALL_INDICATOR_INDEX, value: 1 });
        assert_matches!(procedure.transition(&mut state, response), Ok(_));
        assert_eq!(state.ag_indicators.call.value.unwrap(), true);

        let response = at_resp!(Ciev { ind: CALL_SETUP_INDICATOR_INDEX, value: 1 });
        assert_matches!(procedure.transition(&mut state, response), Ok(_));
        assert_eq!(state.ag_indicators.callsetup.value.unwrap(), 1);

        let response = at_resp!(Ciev { ind: CALL_HELD_INDICATOR_INDEX, value: 1 });
        assert_matches!(procedure.transition(&mut state, response), Ok(_));
        assert_eq!(state.ag_indicators.callheld.value.unwrap(), 1);

        let response = at_resp!(Ciev { ind: SIGNAL_INDICATOR_INDEX, value: 1 });
        assert_matches!(procedure.transition(&mut state, response), Ok(_));
        assert_eq!(state.ag_indicators.signal.value.unwrap(), 1);

        let response = at_resp!(Ciev { ind: ROAM_INDICATOR_INDEX, value: 1 });
        assert_matches!(procedure.transition(&mut state, response), Ok(_));
        assert_eq!(state.ag_indicators.roam.value.unwrap(), true);

        let response = at_resp!(Ciev { ind: BATT_CHG_INDICATOR_INDEX, value: 1 });
        assert_matches!(procedure.transition(&mut state, response), Ok(_));
        assert_eq!(state.ag_indicators.battchg.value.unwrap(), 1);

        assert!(procedure.is_terminated());
    }

    #[fuchsia::test]
    fn update_maintains_value_when_updates_disabled() {
        let mut procedure = PhoneStatusProcedure::new();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);
        state.indicators_update_enabled = false;

        state.ag_indicators.set_default_values();

        assert!(!procedure.is_terminated());

        assert_eq!(state.ag_indicators.service.value.unwrap(), false);
        assert_eq!(state.ag_indicators.call.value.unwrap(), false);
        assert_eq!(state.ag_indicators.callsetup.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.callheld.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.signal.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.roam.value.unwrap(), false);
        assert_eq!(state.ag_indicators.battchg.value.unwrap(), 0);

        let responses = vec![
            at_resp!(Ciev { ind: SERVICE_INDICATOR_INDEX, value: 1 }),
            at_resp!(Ciev { ind: CALL_INDICATOR_INDEX, value: 1 }),
            at_resp!(Ciev { ind: CALL_SETUP_INDICATOR_INDEX, value: 1 }),
            at_resp!(Ciev { ind: CALL_HELD_INDICATOR_INDEX, value: 1 }),
            at_resp!(Ciev { ind: SIGNAL_INDICATOR_INDEX, value: 1 }),
            at_resp!(Ciev { ind: ROAM_INDICATOR_INDEX, value: 1 }),
            at_resp!(Ciev { ind: BATT_CHG_INDICATOR_INDEX, value: 1 }),
        ];

        for response in responses {
            assert_matches!(procedure.transition(&mut state, response), Ok(_));
        }

        assert_eq!(state.ag_indicators.service.value.unwrap(), false);
        assert_eq!(state.ag_indicators.call.value.unwrap(), false);
        assert_eq!(state.ag_indicators.callsetup.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.callheld.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.signal.value.unwrap(), 0);
        assert_eq!(state.ag_indicators.roam.value.unwrap(), false);
        assert_eq!(state.ag_indicators.battchg.value.unwrap(), 0);

        assert!(procedure.is_terminated());
    }
}
