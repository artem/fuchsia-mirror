// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Context, Result};
use fastboot::{
    command::{ClientVariable, Command},
    reply::Reply,
    send,
};
use fuchsia_async::{Task, Timer};
use std::collections::BTreeSet;
use std::time::Duration;
use thiserror::Error;
use usb_bulk::{AsyncInterface as Interface, InterfaceInfo, Open};

// USB fastboot interface IDs
const FASTBOOT_AND_CDC_ETH_USB_DEV_PRODUCT: u16 = 0xa027;
const FASTBOOT_USB_INTERFACE_CLASS: u8 = 0xff;
const FASTBOOT_USB_INTERFACE_SUBCLASS: u8 = 0x42;
const FASTBOOT_USB_INTERFACE_PROTOCOL: u8 = 0x03;

// Valid USB Product Identifiers
const USB_DEV_PRODUCTS: [u16; 3] = [0x4ee0, 0x0d02, FASTBOOT_AND_CDC_ETH_USB_DEV_PRODUCT];

// Vendor ID
const USB_DEV_VENDOR: u16 = 0x18d1;

//TODO(https://fxbug.dev/42130068) - serial info will probably get rolled into the target struct

#[derive(Error, Debug, Clone)]
enum InterfaceCheckError {
    #[error("Invalid IFC Class. Found {:?}, want {:?}", found_class, FASTBOOT_USB_INTERFACE_CLASS)]
    InvalidIfcClass { found_class: u8 },
    #[error(
        "Invalid IFC Subclass. Found {:?}, want {:?}",
        found_subclass,
        FASTBOOT_USB_INTERFACE_SUBCLASS
    )]
    InvalidIfcSubclass { found_subclass: u8 },
    #[error(
        "Invalid IFC Protocol. Found {:?}, want {:?}",
        found_protocol,
        FASTBOOT_USB_INTERFACE_PROTOCOL
    )]
    InvalidIfcProtocol { found_protocol: u8 },
    #[error("Invalid Device Vendor. Found {:?}, want {:?}", found_vendor, USB_DEV_VENDOR)]
    InvalidDevVendor { found_vendor: u16 },
    #[error("Invalid Device Product. Found {:?}, want {:?}", found_product, valid_products)]
    InvalidDevProduct { found_product: u16, valid_products: Vec<u16> },
}

fn valid_ifc_class(info: &InterfaceInfo) -> Result<(), InterfaceCheckError> {
    if info.ifc_class == FASTBOOT_USB_INTERFACE_CLASS {
        Ok(())
    } else {
        Err(InterfaceCheckError::InvalidIfcClass { found_class: info.ifc_class })
    }
}

fn valid_ifc_subclass(info: &InterfaceInfo) -> Result<(), InterfaceCheckError> {
    if info.ifc_subclass == FASTBOOT_USB_INTERFACE_SUBCLASS {
        Ok(())
    } else {
        Err(InterfaceCheckError::InvalidIfcSubclass { found_subclass: info.ifc_subclass })
    }
}

fn valid_ifc_protocol(info: &InterfaceInfo) -> Result<(), InterfaceCheckError> {
    if info.ifc_protocol == FASTBOOT_USB_INTERFACE_PROTOCOL {
        Ok(())
    } else {
        Err(InterfaceCheckError::InvalidIfcProtocol { found_protocol: info.ifc_protocol })
    }
}

fn valid_dev_vendor(info: &InterfaceInfo) -> Result<(), InterfaceCheckError> {
    if info.dev_vendor == USB_DEV_VENDOR {
        Ok(())
    } else {
        Err(InterfaceCheckError::InvalidDevVendor { found_vendor: info.dev_vendor })
    }
}

fn valid_dev_product(info: &InterfaceInfo) -> Result<(), InterfaceCheckError> {
    if USB_DEV_PRODUCTS.contains(&info.dev_product) {
        Ok(())
    } else {
        Err(InterfaceCheckError::InvalidDevProduct {
            found_product: info.dev_product,
            valid_products: USB_DEV_PRODUCTS.to_vec(),
        })
    }
}

fn is_fastboot_match(info: &InterfaceInfo) -> bool {
    let mut results = vec![];

    results.push(valid_dev_vendor(info));
    results.push(valid_ifc_class(info));
    results.push(valid_ifc_subclass(info));
    results.push(valid_ifc_protocol(info));

    let errs: Vec<_> = results.into_iter().filter_map(|r| r.err()).collect();

    if !errs.is_empty() {
        tracing::debug!(
            "Interface is not valid fastboot match. Encountered errors: \n\t{}",
            errs.clone().into_iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n\t")
        );
        false
    } else {
        if let Err(e) = valid_dev_product(info) {
            tracing::debug!(
                "Interface is a valid Fastboot match, but may not be a Fuchsia Product: {}",
                e
            );
        }
        true
    }
}

fn enumerate_interfaces<F>(mut cb: F)
where
    F: FnMut(&InterfaceInfo),
{
    tracing::debug!("Enumerating USB fastboot interfaces");
    let mut cb = |info: &InterfaceInfo| -> bool {
        if is_fastboot_match(info) {
            cb(info)
        }
        // Do not open anything.
        false
    };
    // Do not open anything. Check does not drain the input queue
    let _result = Interface::check(&mut cb);
}

pub fn find_serial_numbers() -> Vec<String> {
    let mut serials = Vec::new();
    let cb = |info: &InterfaceInfo| serials.push(extract_serial_number(info));
    enumerate_interfaces(cb);
    serials
}

fn open_interface<F>(mut cb: F) -> Result<Interface>
where
    F: FnMut(&InterfaceInfo) -> bool,
{
    tracing::debug!("Selecting USB fastboot interface to open");

    let mut open_cb = |info: &InterfaceInfo| -> bool {
        if is_fastboot_match(info) {
            cb(info)
        } else {
            // Do not open.
            false
        }
    };
    Interface::open(&mut open_cb).map_err(Into::into)
}

fn extract_serial_number(info: &InterfaceInfo) -> String {
    let null_pos = match info.serial_number.iter().position(|&c| c == 0) {
        Some(p) => p,
        None => {
            return "".to_string();
        }
    };
    (*String::from_utf8_lossy(&info.serial_number[..null_pos])).to_string()
}

#[tracing::instrument]
pub async fn open_interface_with_serial(serial: &str) -> Result<Interface> {
    tracing::debug!("Opening USB fastboot interface with serial number: {}", serial);
    let mut interface =
        open_interface(|info: &InterfaceInfo| -> bool { extract_serial_number(info) == *serial })
            .with_context(|| format!("opening interface with serial number: {}", serial))?;
    match send(Command::GetVar(ClientVariable::Version), &mut interface).await {
        Ok(Reply::Okay(version)) =>
        // Only support 0.4 right now.
        {
            if version == "0.4".to_string() {
                Ok(interface)
            } else {
                bail!(format!("USB serial {serial}: wrong version ({version})"))
            }
        }
        e => bail!(format!("USB serial {serial}: could not get version. Error: {e:#?}")),
    }
}

#[allow(async_fn_in_trait)]
pub trait FastbootUsbTester: Send + 'static {
    /// Checks if the interface with the given serial number is in Fastboot
    async fn is_fastboot_usb(&mut self, serial: &str) -> bool;
}

struct OpenInterfaceFastbootUsbTester;

impl FastbootUsbTester for OpenInterfaceFastbootUsbTester {
    async fn is_fastboot_usb(&mut self, serial: &str) -> bool {
        open_interface_with_serial(serial).await.is_ok()
    }
}

/// Checks if the USB Interface for the given serial is live and a fastboot match
/// by inspecting the Interface Information.
///
/// This does not mean that the device is ready to respond to any fastboot commands,
/// only that the USB Device with the given serial number exists and declares itself
/// to be a Fastboot device.
///
/// This calls AsyncInterface::check which does _not_ drain the USB's device's
/// buffer upon connecting
pub struct UnversionedFastbootUsbTester;

impl FastbootUsbTester for UnversionedFastbootUsbTester {
    async fn is_fastboot_usb(&mut self, serial: &str) -> bool {
        let mut open_cb = |info: &InterfaceInfo| -> bool {
            if is_fastboot_match(info) {
                extract_serial_number(info) == *serial
            } else {
                // Do not open.
                false
            }
        };
        Interface::check(&mut open_cb)
    }
}

pub struct FastbootUsbWatcher {
    // Task for the discovery loop
    discovery_task: Option<Task<()>>,
    // Task for the drain loop
    drain_task: Option<Task<()>>,
}

#[derive(Debug, PartialEq)]
pub enum FastbootEvent {
    Discovered(String),
    Lost(String),
}

pub trait FastbootEventHandler: Send + 'static {
    /// Handles an event.
    fn handle_event(&mut self, event: Result<FastbootEvent>);
}

impl<F> FastbootEventHandler for F
where
    F: FnMut(Result<FastbootEvent>) -> () + Send + 'static,
{
    fn handle_event(&mut self, x: Result<FastbootEvent>) -> () {
        self(x)
    }
}

pub trait SerialNumberFinder: Send + 'static {
    fn find_serial_numbers(&mut self) -> Vec<String>;
}

impl<F> SerialNumberFinder for F
where
    F: FnMut() -> Vec<String> + Send + 'static,
{
    fn find_serial_numbers(&mut self) -> Vec<String> {
        self()
    }
}

pub fn recommended_watcher<F>(event_handler: F) -> Result<FastbootUsbWatcher>
where
    F: FastbootEventHandler,
{
    Ok(FastbootUsbWatcher::new(
        event_handler,
        find_serial_numbers,
        OpenInterfaceFastbootUsbTester {},
        Duration::from_secs(1),
    ))
}

impl FastbootUsbWatcher {
    pub fn new<F, W, O>(event_handler: F, finder: W, opener: O, interval: Duration) -> Self
    where
        F: FastbootEventHandler,
        W: SerialNumberFinder,
        O: FastbootUsbTester,
    {
        let mut res = Self { discovery_task: None, drain_task: None };

        let (sender, receiver) = async_channel::bounded::<FastbootEvent>(1);

        res.discovery_task.replace(Task::local(discovery_loop(sender, finder, opener, interval)));
        res.drain_task.replace(Task::local(handle_events_loop(receiver, event_handler)));

        res
    }
}

async fn discovery_loop<F, O>(
    events_out: async_channel::Sender<FastbootEvent>,
    mut finder: F,
    mut opener: O,
    discovery_interval: Duration,
) -> ()
where
    F: SerialNumberFinder,
    O: FastbootUsbTester,
{
    let mut serials = BTreeSet::<String>::new();
    loop {
        // Enumerate interfaces
        let new_serials = finder.find_serial_numbers();
        let new_serials = BTreeSet::from_iter(new_serials);
        tracing::trace!("found serials: {:#?}", new_serials);
        // Update Cache
        for serial in &new_serials {
            // Just because the serial is found doesnt mean that the target is ready
            if !opener.is_fastboot_usb(serial.as_str()).await {
                tracing::debug!("Skipping adding serial number: {serial} as although it appears to be a fastboot device it is not readily accepting connections");
                continue;
            }

            tracing::trace!("Inserting new serial: {}", serial);
            if serials.insert(serial.clone()) {
                tracing::trace!("Sending discovered event for serial: {}", serial);
                let _ = events_out.send(FastbootEvent::Discovered(serial.clone())).await;
                tracing::trace!("Sent discovered event for serial: {}", serial);
            }
        }

        // Check for any missing Serials
        let missing_serials: Vec<_> = serials.difference(&new_serials).cloned().collect();
        tracing::trace!("missing serials: {:#?}", missing_serials);
        for serial in missing_serials {
            serials.remove(&serial);
            tracing::trace!("Sening lost event for serial: {}", serial);
            let _ = events_out.send(FastbootEvent::Lost(serial.clone())).await;
            tracing::trace!("Sent lost event for serial: {}", serial);
        }

        tracing::trace!("discovery loop... waiting for {:#?}", discovery_interval);
        Timer::new(discovery_interval).await;
    }
}

async fn handle_events_loop<F>(receiver: async_channel::Receiver<FastbootEvent>, mut handler: F)
where
    F: FastbootEventHandler,
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
    use futures::channel::mpsc::unbounded;
    use pretty_assertions::assert_eq;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    struct TestFastbootUsbTester {
        serial_to_is_fastboot: HashMap<String, bool>,
    }

    impl FastbootUsbTester for TestFastbootUsbTester {
        async fn is_fastboot_usb(&mut self, _serial: &str) -> bool {
            *self.serial_to_is_fastboot.get(_serial).unwrap()
        }
    }

    struct TestSerialNumberFinder {
        responses: Vec<Vec<String>>,
        is_empty: Arc<Mutex<bool>>,
    }

    impl SerialNumberFinder for TestSerialNumberFinder {
        fn find_serial_numbers(&mut self) -> Vec<String> {
            if let Some(res) = self.responses.pop() {
                res
            } else {
                let mut lock = self.is_empty.lock().unwrap();
                *lock = true;
                vec![]
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_usb_watcher() -> Result<()> {
        let empty_signal = Arc::new(Mutex::new(false));
        let serial_finder = TestSerialNumberFinder {
            responses: vec![
                vec!["1234".to_string(), "2345".to_string(), "ABCD".to_string()],
                vec!["1234".to_string(), "5678".to_string()],
            ],
            is_empty: empty_signal.clone(),
        };

        let mut serial_to_is_fastboot = HashMap::new();
        serial_to_is_fastboot.insert("1234".to_string(), true);
        serial_to_is_fastboot.insert("2345".to_string(), true);
        serial_to_is_fastboot.insert("5678".to_string(), true);
        // Since this is not in fastboot then it should not appear in our results
        serial_to_is_fastboot.insert("ABCD".to_string(), false);
        let fastboot_tester = TestFastbootUsbTester { serial_to_is_fastboot };

        let (sender, mut queue) = unbounded();
        let watcher = FastbootUsbWatcher::new(
            move |res: Result<FastbootEvent>| {
                let _ = sender.unbounded_send(res);
            },
            serial_finder,
            fastboot_tester,
            Duration::from_millis(1),
        );

        while !*empty_signal.lock().unwrap() {
            // Wait a tiny bit so the watcher can drain the finder queue
            Timer::new(Duration::from_millis(1)).await;
        }

        drop(watcher);
        let mut events = Vec::<FastbootEvent>::new();
        while let Ok(Some(event)) = queue.try_next() {
            events.push(event.unwrap());
        }

        // Assert state of events
        assert_eq!(events.len(), 6);
        assert_eq!(
            &events,
            &vec![
                // First set of discovery events
                FastbootEvent::Discovered("1234".to_string()),
                FastbootEvent::Discovered("5678".to_string()),
                // Second set of discovery events
                FastbootEvent::Discovered("2345".to_string()),
                FastbootEvent::Lost("5678".to_string()),
                // Last set... there are no more items left in the queue
                // so we lose all serials.
                FastbootEvent::Lost("1234".to_string()),
                FastbootEvent::Lost("2345".to_string()),
            ]
        );
        // Reiterating... serial ABCD was not in fastboot so it should not appear in our results
        Ok(())
    }
}
