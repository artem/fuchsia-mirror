// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuchsia_sync::Mutex,
    futures::channel::mpsc,
    std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    tracing::{info, warn},
};

/// Capacity of "first come, first serve" slots available to clients of
/// the mpsc::Sender<TelemetryEvent>.
pub const TELEMETRY_EVENT_BUFFER_SIZE: usize = 100;

#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub struct TelemetrySender {
    sender: Arc<Mutex<mpsc::Sender<crate::TelemetryEvent>>>,
    sender_is_blocked: Arc<AtomicBool>,
}

impl TelemetrySender {
    pub fn new(sender: mpsc::Sender<crate::TelemetryEvent>) -> Self {
        Self {
            sender: Arc::new(Mutex::new(sender)),
            sender_is_blocked: Arc::new(AtomicBool::new(false)),
        }
    }

    // Send telemetry event. Log an error if it fails
    pub fn send(&self, event: crate::TelemetryEvent) {
        match self.sender.lock().try_send(event) {
            Ok(_) => {
                // If sender has been blocked before, set bool to false and log message
                if self
                    .sender_is_blocked
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    info!("TelemetrySender recovered and resumed sending");
                }
            }
            Err(_) => {
                // If sender has not been blocked before, set bool to true and log error message
                if self
                    .sender_is_blocked
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    warn!("TelemetrySender dropped a msg: either buffer is full or no receiver is waiting");
                }
            }
        }
    }
}
