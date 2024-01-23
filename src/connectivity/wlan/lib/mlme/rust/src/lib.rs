// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This crate implements IEEE Std 802.11-2016 MLME as a library for hardware that supports
//! SoftMAC. This is distinct from FullMAC, which is implemented by drivers and firmware. The
//! implementation is broadly divided between client and AP stations, with some shared components
//! and state machine infrastructure. See the [`client`] and [`ap`] modules.
//!
//! [`ap`]: crate::ap
//! [`client`]: crate::client

mod akm_algorithm;
pub mod ap;
pub mod auth;
mod block_ack;
pub mod buffer;
pub mod client;
mod ddk_converter;
pub mod device;
pub mod disconnect;
pub mod error;
mod logger;
mod minstrel;
#[allow(unused)] // TODO(https://fxbug.dev/79543): Remove annotation once used.
mod probe_sequence;

use {
    anyhow::{bail, format_err, Error},
    banjo_fuchsia_wlan_common as banjo_common, banjo_fuchsia_wlan_softmac as banjo_wlan_softmac,
    device::{completers::StopCompleter, DeviceOps},
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_softmac as fidl_softmac,
    fuchsia_sync::Mutex,
    fuchsia_trace as trace, fuchsia_zircon as zx,
    futures::{
        channel::{mpsc, oneshot},
        select, StreamExt,
    },
    std::{cmp, sync::Arc, time::Duration},
    tracing::info,
    wlan_sme,
};
pub use {ddk_converter::*, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, wlan_common as common};

// TODO(https://fxbug.dev/135356): This trait is migratory and reads both newer and deprecated fields that
//                         encode the same information (and prioritizes the newer fields). Remove
//                         this trait and directly access fields once the deprecated fields for
//                         basic rates and operating channels are removed from the SoftMAC FIDL
//                         APIs and the platform version for WLAN is bumped to or beyond the
//                         removal.
// These extension methods cannot enforce that client code reads fields in a manner that is
// backwards-compatible, but in exchange churn is greatly reduced (compared to the introduction of
// additional types, for example).
/// SDK backwards-compatiblity extensions for band capabilitites.
trait WlanSoftmacBandCapabilityExt {
    /// Gets supported basic rates with SDK backwards-compatibility.
    fn basic_rates(&self) -> Option<&[u8]>;

    /// Gets supported operating channels with SDK backwards-compatibility.
    fn operating_channels(&self) -> Option<&[u8]>;
}

impl WlanSoftmacBandCapabilityExt for fidl_softmac::WlanSoftmacBandCapability {
    fn basic_rates(&self) -> Option<&[u8]> {
        match (&self.basic_rates, (&self.basic_rate_count, &self.basic_rate_list)) {
            // Prefer the newer `basic_rates` field in the SoftMAC FIDL API.
            (Some(basic_rates), _) => Some(basic_rates),
            (None, (Some(n), Some(basic_rates))) => {
                Some(&basic_rates[..cmp::min(usize::from(*n), basic_rates.len())])
            }
            _ => None,
        }
    }

    fn operating_channels(&self) -> Option<&[u8]> {
        match (
            &self.operating_channels,
            (&self.operating_channel_count, &self.operating_channel_list),
        ) {
            // Prefer the newer `operating_channels` field in the SoftMAC FIDL API.
            (Some(operating_channels), _) => Some(operating_channels),
            (None, (Some(n), Some(operating_channels))) => {
                Some(&operating_channels[..cmp::min(usize::from(*n), operating_channels.len())])
            }
            _ => None,
        }
    }
}

trait WlanTxPacketExt {
    fn template(mac_frame: Vec<u8>) -> Self;
}

impl WlanTxPacketExt for fidl_softmac::WlanTxPacket {
    fn template(mac_frame: Vec<u8>) -> Self {
        fidl_softmac::WlanTxPacket {
            mac_frame,
            // TODO(https://fxbug.dev/105579): At time of writing, this field is ignored by the `iwlwifi`
            //                         vendor driver (the only one other than the tap driver used
            //                         for testing). The data used here is meaningless.
            info: fidl_softmac::WlanTxInfo {
                tx_flags: 0,
                valid_fields: 0,
                tx_vector_idx: 0,
                phy: fidl_common::WlanPhyType::Dsss,
                channel_bandwidth: fidl_common::ChannelBandwidth::Cbw20,
                mcs: 0,
            },
        }
    }
}

pub trait MlmeImpl {
    type Config: Send;
    type Device: DeviceOps;
    type TimerEvent;
    fn new(
        config: Self::Config,
        device: Self::Device,
        buf_provider: buffer::BufferProvider,
        scheduler: common::timer::Timer<Self::TimerEvent>,
    ) -> Result<Self, Error>
    where
        Self: Sized;
    fn handle_mlme_request(&mut self, msg: wlan_sme::MlmeRequest) -> Result<(), Error>;
    fn handle_mac_frame_rx(&mut self, bytes: &[u8], rx_info: banjo_wlan_softmac::WlanRxInfo);
    fn handle_eth_frame_tx(&mut self, bytes: &[u8]) -> Result<(), Error>;
    fn handle_scan_complete(&mut self, status: zx::Status, scan_id: u64);
    fn handle_timeout(&mut self, event_id: common::timer::EventId, event: Self::TimerEvent);
    fn access_device(&mut self) -> &mut Self::Device;
}

pub struct MinstrelTimer {
    timer: wlan_common::timer::Timer<()>,
    current_timer: Option<common::timer::EventId>,
}

impl minstrel::TimerManager for MinstrelTimer {
    fn schedule(&mut self, from_now: Duration) {
        self.current_timer.replace(self.timer.schedule_after(from_now.into(), ()));
    }
    fn cancel(&mut self) {
        self.current_timer.take();
    }
}

type MinstrelWrapper = Arc<Mutex<minstrel::MinstrelRateSelector<MinstrelTimer>>>;

// DriverEventSink is used by other devices to interact with our main loop thread. All
// events from our ethernet device or vendor device are converted to DriverEvents
// and sent through this sink, where they can then be handled serially. Multiple copies of
// DriverEventSink may be safely passed between threads, including one that is used by our
// vendor driver as the context for wlan_softmac_ifc_protocol_ops.
pub struct DriverEventSink(pub mpsc::UnboundedSender<DriverEvent>);

// TODO(https://fxbug.dev/29063): Remove copies from MacFrame and EthFrame.
pub enum DriverEvent {
    // Indicates that the device is being removed and our main loop should exit.
    Stop(StopCompleter),
    // TODO(https://fxbug.dev/43456): We need to keep stats for these events and respond to StatsQueryRequest.
    // Indicates receipt of a MAC frame from a peer.
    MacFrameRx { bytes: Vec<u8>, rx_info: banjo_wlan_softmac::WlanRxInfo },
    // Requests transmission of an ethernet frame over the air.
    EthFrameTx { bytes: Vec<u8> },
    // Reports a scan is complete.
    ScanComplete { status: zx::Status, scan_id: u64 },
    // Reports the result of an attempted frame transmission.
    TxResultReport { tx_result: banjo_common::WlanTxResult },
}

fn should_enable_minstrel(mac_sublayer: &fidl_common::MacSublayerSupport) -> bool {
    mac_sublayer.device.tx_status_report_supported && !mac_sublayer.rate_selection_offload.supported
}

const MINSTREL_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
// Remedy for https://fxbug.dev/8165 (https://fxbug.dev/33151)
// See |DATA_FRAME_INTERVAL_NANOS|
// in //src/connectivity/wlan/testing/hw-sim/test/rate_selection/src/lib.rs
// Ensure at least one probe frame (generated every 16 data frames)
// in every cycle:
// 16 <= (MINSTREL_UPDATE_INTERVAL_HW_SIM / MINSTREL_DATA_FRAME_INTERVAL_NANOS * 1e6) < 32.
const MINSTREL_UPDATE_INTERVAL_HW_SIM: std::time::Duration = std::time::Duration::from_millis(83);

pub async fn mlme_main_loop<T: MlmeImpl>(
    init_sender: oneshot::Sender<Result<(), zx::Status>>,
    config: T::Config,
    mut device: T::Device,
    buf_provider: buffer::BufferProvider,
    mlme_request_stream: mpsc::UnboundedReceiver<wlan_sme::MlmeRequest>,
    driver_event_stream: mpsc::UnboundedReceiver<DriverEvent>,
) -> Result<(), Error> {
    let (minstrel_timer, minstrel_time_stream) = common::timer::create_timer();
    let minstrel = device.mac_sublayer_support().ok().filter(should_enable_minstrel).map(
        |mac_sublayer_support| {
            let minstrel = Arc::new(Mutex::new(minstrel::MinstrelRateSelector::new(
                MinstrelTimer { timer: minstrel_timer, current_timer: None },
                if mac_sublayer_support.device.is_synthetic {
                    MINSTREL_UPDATE_INTERVAL_HW_SIM
                } else {
                    MINSTREL_UPDATE_INTERVAL
                },
                probe_sequence::ProbeSequence::random_new(),
            )));
            device.set_minstrel(minstrel.clone());
            minstrel
        },
    );
    let (timer, time_stream) = common::timer::create_timer();

    // Failure to create MLME likely indicates a problem querying the device. There is no recovery
    // path if this occurs.
    let mlme_impl = T::new(config, device, buf_provider, timer).expect("Failed to create MLME.");

    // Signal init is complete.
    init_sender.send(Ok(())).map_err(|_| format_err!("Failed to signal init complete."))?;

    main_loop_impl(
        mlme_impl,
        minstrel,
        mlme_request_stream,
        driver_event_stream,
        time_stream,
        minstrel_time_stream,
    )
    .await
}

/// Begin processing MLME events.
/// Does not return until iface destruction is requested via DriverEvent::Stop, unless
/// a critical error occurs. Note that MlmeHandle::stop will work in either case.
async fn main_loop_impl<T: MlmeImpl>(
    mut mlme_impl: T,
    minstrel: Option<MinstrelWrapper>,
    // A stream of requests coming from the parent SME of this MLME.
    mut mlme_request_stream: mpsc::UnboundedReceiver<wlan_sme::MlmeRequest>,
    // A stream of events initiated by C++ device drivers and then buffered here
    // by our MlmeHandle.
    mut driver_event_stream: mpsc::UnboundedReceiver<DriverEvent>,
    time_stream: common::timer::EventStream<T::TimerEvent>,
    minstrel_time_stream: common::timer::EventStream<()>,
) -> Result<(), Error> {
    let mut timer_stream = common::timer::make_async_timed_event_stream(time_stream).fuse();
    let mut minstrel_timer_stream =
        common::timer::make_async_timed_event_stream(minstrel_time_stream).fuse();
    loop {
        select! {
            // Process requests from SME.
            mlme_request = mlme_request_stream.next() => match mlme_request {
                Some(req) => {
                    let method_name = req.name();
                    if let Err(e) = mlme_impl.handle_mlme_request(req) {
                        info!("Failed to handle mlme {} request: {}", method_name, e);
                    }
                },
                None => bail!("MLME request stream terminated unexpectedly."),
            },
            // Process requests from our C++ drivers.
            driver_event = driver_event_stream.next() => match driver_event {
                Some(event) => match event {
                    // DriverEvent::Stop indicates a safe shutdown.
                    DriverEvent::Stop(stop_completer) => {
                        stop_completer.complete();
                        return Ok(())
                    },
                    DriverEvent::MacFrameRx { bytes, rx_info } => {
                        trace::duration!("wlan", "DriverEvent::MacFrameRx");
                        mlme_impl.handle_mac_frame_rx(&bytes[..], rx_info);
                    }
                    DriverEvent::EthFrameTx { bytes } => {
                        if let Err(e) = mlme_impl.handle_eth_frame_tx(&bytes[..]) {
                            // TODO(https://fxbug.dev/45464): Keep a counter of these failures.
                            info!("Failed to handle eth frame: {}", e);
                        }
                    }
                    DriverEvent::ScanComplete { status, scan_id } => {
                        mlme_impl.handle_scan_complete(status, scan_id)
                    },
                    DriverEvent::TxResultReport { tx_result } => {
                        if let Some(minstrel) = minstrel.as_ref() {
                            minstrel.lock().handle_tx_result_report(&tx_result)
                        }
                    }
                },
                None => bail!("Driver event stream terminated unexpectedly."),
            },
            timed_event = timer_stream.select_next_some() => {
                mlme_impl.handle_timeout(timed_event.id, timed_event.event);
            }
            _minstrel_timeout = minstrel_timer_stream.select_next_some() => {
                if let Some(minstrel) = minstrel.as_ref() {
                    minstrel.lock().handle_timeout()
                }
            }
        }
    }
}

#[cfg(test)]
pub mod test_utils {
    use {
        super::*,
        crate::device::FakeDevice,
        banjo_fuchsia_wlan_common as banjo_common, fidl_fuchsia_wlan_common as fidl_common,
        fidl_fuchsia_wlan_mlme as fidl_mlme,
        ieee80211::{MacAddr, MacAddrBytes},
        wlan_common::channel,
    };

    pub struct FakeMlme {
        device: FakeDevice,
    }

    impl MlmeImpl for FakeMlme {
        type Config = ();
        type Device = FakeDevice;
        type TimerEvent = ();

        fn new(
            _config: Self::Config,
            device: Self::Device,
            _buf_provider: buffer::BufferProvider,
            _scheduler: wlan_common::timer::Timer<Self::TimerEvent>,
        ) -> Result<Self, Error> {
            Ok(Self { device })
        }

        fn handle_mlme_request(
            &mut self,
            _msg: wlan_sme::MlmeRequest,
        ) -> Result<(), anyhow::Error> {
            unimplemented!()
        }

        fn handle_mac_frame_rx(&mut self, _bytes: &[u8], _rx_info: banjo_wlan_softmac::WlanRxInfo) {
            unimplemented!()
        }

        fn handle_eth_frame_tx(&mut self, _bytes: &[u8]) -> Result<(), anyhow::Error> {
            unimplemented!()
        }

        fn handle_scan_complete(&mut self, _status: zx::Status, _scan_id: u64) {
            unimplemented!()
        }

        fn handle_timeout(
            &mut self,
            _event_id: wlan_common::timer::EventId,
            _event: Self::TimerEvent,
        ) {
            unimplemented!()
        }

        fn access_device(&mut self) -> &mut Self::Device {
            &mut self.device
        }
    }

    pub(crate) fn fake_wlan_channel() -> channel::Channel {
        channel::Channel { primary: 1, cbw: channel::Cbw::Cbw20 }
    }

    #[derive(Copy, Clone, Debug)]
    pub struct MockWlanRxInfo {
        pub rx_flags: banjo_wlan_softmac::WlanRxInfoFlags,
        pub valid_fields: banjo_wlan_softmac::WlanRxInfoValid,
        pub phy: banjo_common::WlanPhyType,
        pub data_rate: u32,
        pub channel: fidl_common::WlanChannel,
        pub mcs: u8,
        pub rssi_dbm: i8,
        pub snr_dbh: i16,
    }

    impl MockWlanRxInfo {
        pub(crate) fn with_channel(channel: fidl_common::WlanChannel) -> Self {
            Self {
                valid_fields: banjo_wlan_softmac::WlanRxInfoValid(
                    banjo_wlan_softmac::WlanRxInfoValid::CHAN_WIDTH.0
                        | banjo_wlan_softmac::WlanRxInfoValid::RSSI.0
                        | banjo_wlan_softmac::WlanRxInfoValid::SNR.0,
                ),
                channel,
                rssi_dbm: -40,
                snr_dbh: 35,

                // Default to 0 for these fields since there are no
                // other reasonable values to mock.
                rx_flags: banjo_wlan_softmac::WlanRxInfoFlags(0),
                phy: banjo_common::WlanPhyType::DSSS,
                data_rate: 0,
                mcs: 0,
            }
        }
    }

    impl From<MockWlanRxInfo> for banjo_wlan_softmac::WlanRxInfo {
        fn from(mock_rx_info: MockWlanRxInfo) -> banjo_wlan_softmac::WlanRxInfo {
            banjo_wlan_softmac::WlanRxInfo {
                rx_flags: mock_rx_info.rx_flags,
                valid_fields: mock_rx_info.valid_fields,
                phy: mock_rx_info.phy,
                data_rate: mock_rx_info.data_rate,
                channel: ddk_converter::ddk_channel_from_fidl(mock_rx_info.channel).unwrap(),
                mcs: mock_rx_info.mcs,
                rssi_dbm: mock_rx_info.rssi_dbm,
                snr_dbh: mock_rx_info.snr_dbh,
            }
        }
    }

    pub(crate) fn fake_key(address: MacAddr) -> fidl_mlme::SetKeyDescriptor {
        fidl_mlme::SetKeyDescriptor {
            cipher_suite_oui: [1, 2, 3],
            cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(4),
            key_type: fidl_mlme::KeyType::Pairwise,
            address: address.to_array(),
            key_id: 6,
            key: vec![1, 2, 3, 4, 5, 6, 7],
            rsc: 8,
        }
    }

    pub(crate) fn fake_set_keys_req(address: MacAddr) -> wlan_sme::MlmeRequest {
        wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest {
            keylist: vec![fake_key(address)],
        })
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{buffer::FakeBufferProvider, device::FakeDevice, test_utils::FakeMlme, *},
        fuchsia_async::TestExecutor,
        futures::channel::{mpsc, oneshot},
        std::task::Poll,
        wlan_common::assert_variant,
    };

    #[test]
    fn start_and_stop_main_loop() {
        let mut exec = TestExecutor::new();
        let (fake_device, _fake_device_state) = FakeDevice::new(&exec);
        let buf_provider = FakeBufferProvider::new();
        let (device_sink, device_stream) = mpsc::unbounded();
        let (_mlme_request_sink, mlme_request_stream) = mpsc::unbounded();
        let (init_sender, mut init_receiver) = oneshot::channel::<Result<(), zx::Status>>();
        let (shutdown_sender, mut shutdown_receiver) = oneshot::channel::<()>();
        let mut main_loop = Box::pin(mlme_main_loop::<FakeMlme>(
            init_sender,
            (),
            fake_device,
            buf_provider,
            mlme_request_stream,
            device_stream,
        ));
        assert_variant!(exec.run_until_stalled(&mut main_loop), Poll::Pending);
        assert_eq!(exec.run_until_stalled(&mut init_receiver), Poll::Ready(Ok(Ok(()))));

        device_sink
            .unbounded_send(DriverEvent::Stop(StopCompleter::new(Box::new(move || {
                shutdown_sender.send(()).expect("Failed to signal shutdown completion.")
            }))))
            .expect("Failed to send stop event");
        assert_variant!(exec.run_until_stalled(&mut main_loop), Poll::Ready(Ok(())));
        assert_eq!(exec.run_until_stalled(&mut shutdown_receiver), Poll::Ready(Ok(())));
        assert!(device_sink.is_closed());
    }
}
