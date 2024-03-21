// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::input_device, crate::input_handler, async_trait::async_trait,
    futures::channel::mpsc::Sender, std::cell::RefCell, std::rc::Rc,
};

/// A fake [`InputHandler`] used for testing.
/// A [`ObserveFakeEventsInputHandler`] does not consume InputEvents.
pub struct ObserveFakeEventsInputHandler {
    /// Events received by [`handle_input_event()`] are sent to this channel.
    event_sender: RefCell<Sender<input_device::InputEvent>>,
}

#[cfg(test)]
impl ObserveFakeEventsInputHandler {
    pub fn new(event_sender: Sender<input_device::InputEvent>) -> Rc<Self> {
        Rc::new(ObserveFakeEventsInputHandler { event_sender: RefCell::new(event_sender) })
    }
}

#[async_trait(?Send)]
impl input_handler::InputHandler for ObserveFakeEventsInputHandler {
    async fn handle_input_event(
        self: Rc<Self>,
        input_event: input_device::InputEvent,
    ) -> Vec<input_device::InputEvent> {
        match self.event_sender.borrow_mut().try_send(input_event.clone()) {
            Err(_) => assert!(false),
            _ => {}
        };

        vec![input_event]
    }

    fn set_handler_healthy(self: std::rc::Rc<Self>) {
        // No inspect data on ObserveFakeEventsInputHandler. Do nothing.
    }

    fn set_handler_unhealthy(self: std::rc::Rc<Self>, _msg: &str) {
        // No inspect data on ObserveFakeEventsInputHandler. Do nothing.
    }
}
