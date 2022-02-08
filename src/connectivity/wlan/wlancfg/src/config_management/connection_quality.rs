// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{client::types as client_types, util::pseudo_energy::*},
    fuchsia_zircon as zx,
    log::error,
    std::collections::VecDeque,
};

// Number of previous RSSI measurements to exponentially weigh into average.
// TODO(fxbug.dev/84870): Tune smoothing factor.
pub const EWMA_SMOOTHING_FACTOR: usize = 10;

/// Connection quality data related to signal
#[derive(Clone, Debug, PartialEq)]
pub struct SignalData {
    pub ewma_rssi: EwmaPseudoDecibel,
    pub ewma_snr: EwmaPseudoDecibel,
    pub rssi_velocity: PseudoDecibel,
}

impl SignalData {
    pub fn new(
        initial_rssi: PseudoDecibel,
        initial_snr: PseudoDecibel,
        ewma_weight: usize,
    ) -> Self {
        Self {
            ewma_rssi: EwmaPseudoDecibel::new(ewma_weight, initial_rssi),
            ewma_snr: EwmaPseudoDecibel::new(ewma_weight, initial_snr),
            rssi_velocity: 0,
        }
    }
    pub fn update_with_new_measurement(&mut self, rssi: PseudoDecibel, snr: PseudoDecibel) {
        let prev_rssi = self.ewma_rssi.get();
        self.ewma_rssi.update_average(rssi);
        self.ewma_snr.update_average(snr);
        self.rssi_velocity =
            match calculate_pseudodecibel_velocity(vec![prev_rssi, self.ewma_rssi.get()]) {
                Ok(velocity) => velocity,
                Err(e) => {
                    error!("Failed to update SignalData velocity: {:?}", e);
                    self.rssi_velocity
                }
            };
    }
}

/// Data points related to historical connection
#[derive(Clone, Debug)]
pub struct PastConnectionData {
    /// Time at which connect was first attempted
    pub connection_attempt_time: zx::Time,
    /// Duration from connection attempt to success
    pub time_to_connect: zx::Duration,
    /// Time at which the connection was ended
    pub disconnect_time: zx::Time,
    /// Cause of disconnect or failure to connect
    pub disconnect_reason: client_types::DisconnectReason,
    /// Final signal strength measure before disconnect
    pub signal_data_at_disconnect: SignalData,
    /// Average phy rate over connection duration
    pub average_tx_rate: u32,
}

/// Aggregated information about the current BSS's connection quality, used for evaluation.
pub struct BssQualityData<'a> {
    pub signal_data: SignalData,
    pub channel: client_types::WlanChan,
    // TX and RX rate, respectively.
    pub phy_rates: (u32, u32),
    // Connection data of past successful connections to this BSS.
    pub past_connections_list: &'a VecDeque<PastConnectionData>,
}

impl<'a> BssQualityData<'a> {
    pub fn new(
        signal_data: SignalData,
        channel: client_types::WlanChan,
        past_connections_list: &'a VecDeque<PastConnectionData>,
    ) -> Self {
        BssQualityData {
            signal_data: signal_data,
            channel: channel,
            phy_rates: (0, 0),
            past_connections_list: past_connections_list,
        }
    }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        test_util::{assert_gt, assert_lt},
    };

    #[fuchsia::test]
    fn test_update_with_new_measurements() {
        let mut signal_data = SignalData::new(-40, 30, EWMA_SMOOTHING_FACTOR);
        signal_data.update_with_new_measurement(-60, 15);
        assert_lt!(signal_data.ewma_rssi.get(), -40);
        assert_gt!(signal_data.ewma_rssi.get(), -60);
        assert_lt!(signal_data.ewma_snr.get(), 30);
        assert_gt!(signal_data.ewma_snr.get(), 15);
        assert_lt!(signal_data.rssi_velocity, 0);
    }
}
