// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        buffer::OutBuf,
        client::{
            channel_listener::{ChannelListener, ChannelListenerSource},
            channel_scheduler::ChannelScheduler,
            convert_beacon::construct_bss_description,
            Context, TimedEvent,
        },
        device::{Device, TxFlags},
        error::Error,
    },
    anyhow::format_err,
    banjo_ddk_hw_wlan_wlaninfo as banjo_hw_wlaninfo,
    banjo_fuchsia_hardware_wlan_mac as banjo_wlan_mac, banjo_fuchsia_wlan_common as banjo_common,
    banjo_fuchsia_wlan_ieee80211 as banjo_ieee80211, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_internal as fidl_internal, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fuchsia_zircon as zx,
    ieee80211::{Bssid, MacAddr},
    log::{error, warn},
    thiserror::Error,
    wlan_common::{
        mac::{self, CapabilityInfo},
        mgmt_writer,
        time::TimeUnit,
        timer::EventId,
    },
    wlan_frame_writer::write_frame,
};

#[derive(Error, Debug, PartialEq, Eq)]
pub enum ScanError {
    #[error("scanner is busy")]
    Busy,
    #[error("invalid arg: empty channel list")]
    EmptyChannelList,
    #[error("invalid arg: channel list too large")]
    ChannelListTooLarge,
    #[error("invalid arg: max_channel_time < min_channel_time")]
    MaxChannelTimeLtMin,
    #[error("invalid arg: SSID too long")]
    SsidTooLong,
    #[error("fail starting hw scan: {}", _0)]
    StartHwScanFails(zx::Status),
    #[error("hw scan aborted")]
    HwScanAborted,
    #[error("invalid arg: scanning only supports bss_type_selector ANY")]
    UnsupportedBssTypeSelector,
}

impl From<ScanError> for zx::Status {
    fn from(e: ScanError) -> Self {
        match e {
            ScanError::Busy => zx::Status::UNAVAILABLE,
            ScanError::EmptyChannelList
            | ScanError::ChannelListTooLarge
            | ScanError::MaxChannelTimeLtMin
            | ScanError::SsidTooLong
            | ScanError::UnsupportedBssTypeSelector => zx::Status::INVALID_ARGS,
            ScanError::StartHwScanFails(status) => status,
            ScanError::HwScanAborted => zx::Status::INTERNAL,
        }
    }
}

impl From<ScanError> for fidl_mlme::ScanResultCode {
    fn from(e: ScanError) -> Self {
        match e {
            ScanError::Busy => fidl_mlme::ScanResultCode::NotSupported,
            ScanError::EmptyChannelList
            | ScanError::ChannelListTooLarge
            | ScanError::MaxChannelTimeLtMin
            | ScanError::SsidTooLong
            | ScanError::UnsupportedBssTypeSelector => fidl_mlme::ScanResultCode::InvalidArgs,
            ScanError::StartHwScanFails(..) | ScanError::HwScanAborted => {
                fidl_mlme::ScanResultCode::InternalError
            }
        }
    }
}

pub struct Scanner {
    ongoing_scan: Option<OngoingScan>,
    /// MAC address of current client interface
    iface_mac: MacAddr,
}

impl Scanner {
    pub fn new(iface_mac: MacAddr) -> Self {
        Self { ongoing_scan: None, iface_mac }
    }

    pub fn bind<'a>(&'a mut self, ctx: &'a mut Context) -> BoundScanner<'a> {
        BoundScanner { scanner: self, ctx }
    }

    pub fn is_scanning(&self) -> bool {
        self.ongoing_scan.is_some()
    }

    #[cfg(test)]
    pub fn probe_delay_timeout_id(&self) -> Option<EventId> {
        match self.ongoing_scan {
            Some(OngoingScan { probe_delay_timeout_id: id, .. }) => id,
            None => None,
        }
    }
}

pub struct BoundScanner<'a> {
    scanner: &'a mut Scanner,
    ctx: &'a mut Context,
}

struct OngoingScan {
    /// Scan request that's currently being serviced.
    req: fidl_mlme::ScanRequest,
    /// ID of timeout event scheduled for active scan at beginning of each channel switch. At
    /// end of timeout, a probe request is sent.
    probe_delay_timeout_id: Option<EventId>,
}

impl<'a> BoundScanner<'a> {
    /// Handle scan request. Queue requested scan channels in channel scheduler.
    ///
    /// If a scan request is in progress, or the new request has invalid argument (empty channel
    /// list or larger min channel time than max), then the request is rejected.
    pub fn on_sme_scan<F, CL>(
        &'a mut self,
        req: fidl_mlme::ScanRequest,
        build_channel_listener: F,
        chan_sched: &mut ChannelScheduler,
    ) -> Result<(), ScanError>
    where
        F: FnOnce(&'a mut Context, &'a mut Scanner) -> CL,
        CL: ChannelListener,
    {
        macro_rules! send_scan_end_and_return {
            ($txn_id:expr, $scan_error:expr, $self:expr) => {{
                let error = $scan_error;
                send_scan_end($txn_id, error.into(), &mut $self.ctx.device);
                return Err($scan_error);
            }};
        }

        if !matches!(req.bss_type_selector, fidl_internal::BSS_TYPE_SELECTOR_ANY) {
            error!("scanning only supports bss_type_selector ANY");
            send_scan_end_and_return!(req.txn_id, ScanError::UnsupportedBssTypeSelector, self);
        }

        if self.scanner.ongoing_scan.is_some() {
            send_scan_end_and_return!(req.txn_id, ScanError::Busy, self);
        }

        let channel_list = req.channel_list.as_ref().map(|list| list.as_slice()).unwrap_or(&[][..]);
        if channel_list.is_empty() {
            send_scan_end_and_return!(req.txn_id, ScanError::EmptyChannelList, self);
        }
        if channel_list.len() > banjo_hw_wlaninfo::WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize {
            send_scan_end_and_return!(req.txn_id, ScanError::ChannelListTooLarge, self);
        }
        if req.max_channel_time < req.min_channel_time {
            send_scan_end_and_return!(req.txn_id, ScanError::MaxChannelTimeLtMin, self);
        }

        let wlanmac_info = self.ctx.device.wlanmac_info();
        let hw_scan = (wlanmac_info.driver_features
            & banjo_hw_wlaninfo::WlanInfoDriverFeature::SCAN_OFFLOAD)
            .0
            > 0;
        if hw_scan {
            let scan_type = if req.scan_type == fidl_mlme::ScanTypes::Active {
                banjo_wlan_mac::WlanHwScanType::ACTIVE
            } else {
                banjo_wlan_mac::WlanHwScanType::PASSIVE
            };
            let mut channels = [0; banjo_hw_wlaninfo::WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize];
            channels[..channel_list.len()].copy_from_slice(channel_list);

            // fuchsia.wlan.mlme/ScanRequest.ssid is always at most fidl_ieee80211::MAX_SSID_BYTE_LEN bytes
            let mut ssid = [0; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize];
            ssid[..req.ssid.len()].copy_from_slice(&req.ssid[..]);

            let config = banjo_wlan_mac::WlanHwScanConfig {
                scan_type,
                num_channels: channel_list.len() as u8,
                channels,
                ssid: banjo_ieee80211::CSsid { len: req.ssid.len() as u8, data: ssid },
            };
            if let Err(status) = self.ctx.device.start_hw_scan(&config) {
                self.scanner.ongoing_scan.take();
                send_scan_end_and_return!(req.txn_id, ScanError::StartHwScanFails(status), self);
            }
            self.scanner.ongoing_scan = Some(OngoingScan { req, probe_delay_timeout_id: None });
        } else {
            let channels = channel_list
                .iter()
                .map(|c| banjo_common::WlanChannel {
                    primary: *c,
                    cbw: banjo_common::ChannelBandwidth::CBW20,
                    secondary80: 0,
                })
                .collect();
            let max_channel_time = req.max_channel_time;
            // Note: for software scanning case, it's important to populate this beforehand because
            //       channel scheduler may `begin_requested_channel_time` immediately, and scanner
            //       needs these information to determine whether to send probe request.
            self.scanner.ongoing_scan = Some(OngoingScan { req, probe_delay_timeout_id: None });
            let mut listener = build_channel_listener(self.ctx, self.scanner);
            let dwell_time = TimeUnit(max_channel_time as u16).into();
            chan_sched
                .bind(&mut listener, ChannelListenerSource::Scanner)
                .queue_channels(channels, dwell_time);
        }
        Ok(())
    }

    /// Called when MLME receives a beacon or probe response so that scanner saves it in a BSS map.
    ///
    /// This method is a no-op if no scan request is in progress.
    pub fn handle_beacon_or_probe_response(
        &mut self,
        bssid: Bssid,
        beacon_interval: TimeUnit,
        capability_info: CapabilityInfo,
        ies: &[u8],
        rx_info: banjo_wlan_mac::WlanRxInfo,
    ) {
        let txn_id = match &self.scanner.ongoing_scan {
            Some(req) => req.req.txn_id,
            None => return,
        };
        let bss_description =
            construct_bss_description(bssid, beacon_interval, capability_info, ies, rx_info);
        let bss_description = match bss_description {
            Ok(bss) => bss,
            Err(e) => {
                warn!("Failed to process beacon or probe response: {}", e);
                return;
            }
        };
        send_scan_result(txn_id, bss_description, &mut self.ctx.device);
    }

    /// Notify scanner about end of probe-delay timeout so that it sends out probe request.
    pub fn handle_probe_delay_timeout(&mut self, channel: banjo_common::WlanChannel) {
        let ssid = match &self.scanner.ongoing_scan {
            Some(OngoingScan { req, .. }) => req.ssid.clone(),
            None => return,
        };
        if let Err(e) = self.send_probe_req(&ssid[..], channel) {
            error!("{}", e);
        }
    }

    pub fn handle_hw_scan_complete(&mut self, status: banjo_wlan_mac::WlanHwScan) {
        let req = match self.scanner.ongoing_scan.take() {
            Some(req) => req,
            None => {
                warn!("Received HwScanComplete with status {:?} while no req in progress", status);
                return;
            }
        };
        let result_code = if status == banjo_wlan_mac::WlanHwScan::SUCCESS {
            fidl_mlme::ScanResultCode::Success
        } else {
            fidl_mlme::ScanResultCode::InternalError
        };
        send_scan_end(req.req.txn_id, result_code, &mut self.ctx.device);
    }

    /// Called after switching to a requested channel from a scan request. It's primarily to
    /// send out, or schedule to send out, a probe request in an active scan.
    pub fn begin_requested_channel_time(&mut self, channel: banjo_common::WlanChannel) {
        let (req, probe_delay_timeout_id) = match &mut self.scanner.ongoing_scan {
            Some(req) => (&req.req, &mut req.probe_delay_timeout_id),
            None => return,
        };
        if req.scan_type == fidl_mlme::ScanTypes::Active {
            if req.probe_delay == 0 {
                let ssid = req.ssid.clone();
                if let Err(e) = self.send_probe_req(&ssid[..], channel) {
                    error!("{}", e);
                }
            } else {
                let timeout_id = self.ctx.timer.schedule_after(
                    TimeUnit(req.probe_delay as u16).into(),
                    TimedEvent::ScannerProbeDelay(channel),
                );
                probe_delay_timeout_id.replace(timeout_id);
            }
        }
    }

    fn send_probe_req(
        &mut self,
        ssid: &[u8],
        channel: banjo_common::WlanChannel,
    ) -> Result<(), Error> {
        let iface_info = self.ctx.device.wlanmac_info();
        let band_info = get_band_info(&iface_info, channel)
            .ok_or(format_err!("no band found for channel {:?}", channel.primary))?;
        let rates: Vec<u8> = band_info.rates.iter().cloned().filter(|r| *r > 0).collect();

        let (buf, bytes_written) = write_frame!(&mut self.ctx.buf_provider, {
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::PROBE_REQ),
                    Bssid(mac::BCAST_ADDR),
                    self.scanner.iface_mac,
                    mac::SequenceControl(0)
                        .with_seq_num(self.ctx.seq_mgr.next_sns1(&mac::BCAST_ADDR) as u16)),
            },
            ies: {
                ssid: ssid,
                supported_rates: rates,
                extended_supported_rates: {/* continue rates */},
            }
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        self.ctx
            .device
            .send_wlan_frame(out_buf, TxFlags::NONE)
            .map_err(|s| Error::Status(format!("error sending probe req frame"), s))
    }

    /// Called when channel scheduler has gone through all the requested channels from a scan
    /// request. The scanner submits scan results to SME.
    pub fn handle_channel_req_complete(&mut self) {
        if let Some(req) = self.scanner.ongoing_scan.take() {
            send_scan_end(req.req.txn_id, fidl_mlme::ScanResultCode::Success, &mut self.ctx.device);
        }
    }
}

fn get_band_info(
    iface_info: &banjo_wlan_mac::WlanmacInfo,
    channel: banjo_common::WlanChannel,
) -> Option<&banjo_hw_wlaninfo::WlanInfoBandInfo> {
    const _2GHZ_BAND_HIGHEST_CHANNEL: u8 = 14;
    iface_info.bands[..iface_info.bands_count as usize]
        .iter()
        .filter(|b| match channel.primary {
            x if x > _2GHZ_BAND_HIGHEST_CHANNEL => {
                b.band == banjo_hw_wlaninfo::WlanInfoBand::FIVE_GHZ
            }
            _ => b.band == banjo_hw_wlaninfo::WlanInfoBand::TWO_GHZ,
        })
        .next()
}

fn send_scan_result(txn_id: u64, bss: fidl_internal::BssDescription, device: &mut Device) {
    let result = device.mlme_control_handle().send_on_scan_result(&mut fidl_mlme::ScanResult {
        txn_id,
        timestamp_nanos: zx::Time::get_monotonic().into_nanos(),
        bss,
    });
    if let Err(e) = result {
        error!("error sending MLME ScanResult: {}", e);
    }
}

fn send_scan_end(txn_id: u64, code: fidl_mlme::ScanResultCode, device: &mut Device) {
    let result =
        device.mlme_control_handle().send_on_scan_end(&mut fidl_mlme::ScanEnd { txn_id, code });
    if let Err(e) = result {
        error!("error sending MLME ScanEnd: {}", e);
    }
}
#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            buffer::FakeBufferProvider,
            client::{
                channel_listener::{LEvent, MockListenerState},
                channel_scheduler, ClientConfig,
            },
            device::FakeDevice,
            test_utils::MockWlanRxInfo,
        },
        fidl_fuchsia_wlan_common as fidl_common, fuchsia_async as fasync,
        lazy_static::lazy_static,
        std::{cell::RefCell, rc::Rc},
        wlan_common::{
            assert_variant,
            sequence::SequenceManager,
            timer::{create_timer, TimeStream, Timer},
        },
    };

    const BSSID: Bssid = Bssid([6u8; 6]);
    const IFACE_MAC: MacAddr = [7u8; 6];
    // Original channel set by FakeDevice
    const ORIGINAL_CHAN: banjo_common::WlanChannel = channel(0);

    // Capability information: ESS
    const CAPABILITY_INFO: CapabilityInfo = CapabilityInfo(1);
    const BEACON_INTERVAL: u16 = 100;
    lazy_static! {
        pub static ref RX_INFO: banjo_wlan_mac::WlanRxInfo = MockWlanRxInfo::default().into();
    }

    fn scan_req() -> fidl_mlme::ScanRequest {
        fidl_mlme::ScanRequest {
            txn_id: 1337,
            bss_type_selector: fidl_internal::BSS_TYPE_SELECTOR_ANY,
            bssid: BSSID.0,
            ssid: b"ssid".to_vec(),
            scan_type: fidl_mlme::ScanTypes::Passive,
            probe_delay: 0,
            channel_list: Some(vec![6]),
            min_channel_time: 100,
            max_channel_time: 300,
            ssid_list: None,
        }
    }

    #[test]
    fn test_handle_scan_req_queues_channels() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");
        let req_id = channel_scheduler::RequestId(1, ChannelListenerSource::Scanner);
        assert_eq!(
            m.listener_state.drain_events(),
            vec![
                LEvent::PreSwitch { from: ORIGINAL_CHAN, to: channel(6), req_id },
                LEvent::PostSwitch { from: ORIGINAL_CHAN, to: channel(6), req_id },
            ]
        );
    }

    #[test]
    fn test_active_scan_probe_req_sent_with_no_delay() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        let scan_req = fidl_mlme::ScanRequest {
            scan_type: fidl_mlme::ScanTypes::Active,
            probe_delay: 0,
            ..scan_req()
        };
        scanner
            .bind(&mut ctx)
            .on_sme_scan(scan_req, m.listener_state.create_channel_listener_fn(), &mut m.chan_sched)
            .expect("expect scan req accepted");
        let req_id = channel_scheduler::RequestId(1, ChannelListenerSource::Scanner);
        assert_eq!(
            m.listener_state.drain_events(),
            vec![
                LEvent::PreSwitch { from: ORIGINAL_CHAN, to: channel(6), req_id },
                LEvent::PostSwitch { from: ORIGINAL_CHAN, to: channel(6), req_id },
            ]
        );

        // On post-switch announcement, the listener would call `begin_requested_channel_time`
        scanner.bind(&mut ctx).begin_requested_channel_time(channel(6));
        assert_eq!(m.fake_device.wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device.wlan_queue[0].0[..], &[
            // Mgmt header:
            0b0100_00_00, 0b00000000, // FC
            0, 0, // Duration
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // addr3
            0x10, 0, // Sequence Control
            // IEs
            0, 4, // SSID id and length
            115, 115, 105, 100, // SSID
            1, 6, // supp_rates id and length
            12, 24, 48, 54, 96, 108, // supp_rates
        ][..]);
    }

    fn get_timed_event(id: EventId, time_stream: &mut TimeStream<TimedEvent>) -> TimedEvent {
        loop {
            let (_, event) = time_stream.try_next().unwrap().unwrap();
            if event.id == id {
                return event.event;
            }
        }
    }

    #[test]
    fn test_active_scan_probe_req_sent_with_delay() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        let scan_req = fidl_mlme::ScanRequest {
            scan_type: fidl_mlme::ScanTypes::Active,
            probe_delay: 5,
            ..scan_req()
        };
        scanner
            .bind(&mut ctx)
            .on_sme_scan(scan_req, m.listener_state.create_channel_listener_fn(), &mut m.chan_sched)
            .expect("expect scan req accepted");
        scanner.bind(&mut ctx).begin_requested_channel_time(channel(6));

        // Verify nothing is sent yet, but timeout is scheduled
        assert!(m.fake_device.wlan_queue.is_empty());
        assert!(scanner.probe_delay_timeout_id().is_some());
        let timeout_id = scanner.probe_delay_timeout_id().unwrap();

        assert_eq!(
            get_timed_event(timeout_id, &mut m.time_stream),
            TimedEvent::ScannerProbeDelay(channel(6))
        );

        // Check that telling scanner to handle timeout would send probe request frame
        scanner.bind(&mut ctx).handle_probe_delay_timeout(channel(6));
        assert_eq!(m.fake_device.wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device.wlan_queue[0].0[..], &[
            // Mgmt header:
            0b0100_00_00, 0b00000000, // FC
            0, 0, // Duration
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // addr3
            0x10, 0, // Sequence Control
            // IEs
            0, 4, // SSID id and length
            115, 115, 105, 100, // SSID
            1, 6, // supp_rates id and length
            12, 24, 48, 54, 96, 108, // supp_rates
        ][..]);
    }

    #[test]
    fn test_handle_scan_req_reject_if_busy() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");
        let scan_req = fidl_mlme::ScanRequest { txn_id: 1338, ..scan_req() };
        let result = scanner.bind(&mut ctx).on_sme_scan(
            scan_req,
            m.listener_state.create_channel_listener_fn(),
            &mut m.chan_sched,
        );
        assert_variant!(result, Err(ScanError::Busy));
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1338, code: fidl_mlme::ScanResultCode::NotSupported }
        );
    }

    #[test]
    fn test_handle_scan_req_reject_if_bad_bss_type_selector() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");
        let scan_req = fidl_mlme::ScanRequest {
            bss_type_selector: fidl_internal::BSS_TYPE_SELECTOR_INFRASTRUCTURE,
            ..scan_req()
        };
        let result = scanner.bind(&mut ctx).on_sme_scan(
            scan_req,
            m.listener_state.create_channel_listener_fn(),
            &mut m.chan_sched,
        );
        assert_variant!(result, Err(ScanError::UnsupportedBssTypeSelector));
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn test_handle_scan_req_empty_channel_list() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        let scan_req = fidl_mlme::ScanRequest { channel_list: Some(vec![]), ..scan_req() };
        let result = scanner.bind(&mut ctx).on_sme_scan(
            scan_req,
            m.listener_state.create_channel_listener_fn(),
            &mut m.chan_sched,
        );
        assert_variant!(result, Err(ScanError::EmptyChannelList));
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn test_handle_scan_req_long_channel_list() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        let mut channel_list = vec![];
        for i in 1..=65 {
            channel_list.push(i);
        }
        let scan_req = fidl_mlme::ScanRequest { channel_list: Some(channel_list), ..scan_req() };
        let result = scanner.bind(&mut ctx).on_sme_scan(
            scan_req,
            m.listener_state.create_channel_listener_fn(),
            &mut m.chan_sched,
        );
        assert_variant!(result, Err(ScanError::ChannelListTooLarge));
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn test_handle_scan_req_invalid_channel_time() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        let scan_req =
            fidl_mlme::ScanRequest { min_channel_time: 101, max_channel_time: 100, ..scan_req() };
        let result = scanner.bind(&mut ctx).on_sme_scan(
            scan_req,
            m.listener_state.create_channel_listener_fn(),
            &mut m.chan_sched,
        );
        assert_variant!(result, Err(ScanError::MaxChannelTimeLtMin));
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn test_start_hw_scan_success() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        m.fake_device.info.driver_features |=
            banjo_hw_wlaninfo::WlanInfoDriverFeature::SCAN_OFFLOAD;
        let mut scanner = Scanner::new(IFACE_MAC);

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");

        // Verify that hw-scan is requested
        assert_variant!(m.fake_device.hw_scan_req, Some(config) => {
            assert_eq!(config.scan_type, banjo_wlan_mac::WlanHwScanType::PASSIVE);
            assert_eq!(config.num_channels, 1);

            let mut channels = [0u8; banjo_hw_wlaninfo::WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize];
            channels[..1].copy_from_slice(&[6]);
            assert_eq!(&config.channels[..], &channels[..]);

            let mut ssid = [0; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize];
            ssid[..4].copy_from_slice(b"ssid");
            assert_eq!(config.ssid, banjo_ieee80211::CSsid { len: 4, data: ssid });
        }, "HW scan not initiated");

        // Mock receiving a beacon
        handle_beacon(&mut scanner, &mut ctx, &beacon_ies()[..]);
        m.fake_device.next_mlme_msg::<fidl_mlme::ScanResult>().expect("error reading ScanResult");

        // Verify scan results are sent on hw scan complete
        scanner.bind(&mut ctx).handle_hw_scan_complete(banjo_wlan_mac::WlanHwScan::SUCCESS);
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::Success }
        );
    }

    #[test]
    fn test_start_hw_scan_fails() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let device = m.fake_device.as_device_fail_start_hw_scan();
        let mut ctx = m.make_ctx_with_device(device);
        m.fake_device.info.driver_features |=
            banjo_hw_wlaninfo::WlanInfoDriverFeature::SCAN_OFFLOAD;
        let mut scanner = Scanner::new(IFACE_MAC);

        let result = scanner.bind(&mut ctx).on_sme_scan(
            scan_req(),
            m.listener_state.create_channel_listener_fn(),
            &mut m.chan_sched,
        );
        assert_variant!(result, Err(ScanError::StartHwScanFails(zx::Status::NOT_SUPPORTED)));
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InternalError }
        );
    }

    #[test]
    fn test_start_hw_scan_aborted() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        m.fake_device.info.driver_features |=
            banjo_hw_wlaninfo::WlanInfoDriverFeature::SCAN_OFFLOAD;
        let mut scanner = Scanner::new(IFACE_MAC);

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");

        // Verify that hw-scan is requested
        assert_variant!(m.fake_device.hw_scan_req, Some(_), "HW scan not initiated");

        // Mock receiving a beacon
        handle_beacon(&mut scanner, &mut ctx, &beacon_ies()[..]);
        m.fake_device.next_mlme_msg::<fidl_mlme::ScanResult>().expect("error reading ScanResult");

        // Verify scan results are sent on hw scan complete
        scanner.bind(&mut ctx).handle_hw_scan_complete(banjo_wlan_mac::WlanHwScan::ABORTED);
        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InternalError }
        );
    }

    #[test]
    fn test_handle_beacon_or_probe_response() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);
        let test_start_timestamp_nanos = zx::Time::get_monotonic().into_nanos();

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");
        handle_beacon(&mut scanner, &mut ctx, &beacon_ies()[..]);
        scanner.bind(&mut ctx).handle_channel_req_complete();

        let scan_result = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanResult>()
            .expect("error reading MLME ScanResult");
        assert_eq!(scan_result.txn_id, 1337);
        assert!(scan_result.timestamp_nanos > test_start_timestamp_nanos);
        assert_eq!(
            scan_result.bss,
            fidl_internal::BssDescription {
                bssid: BSSID.0,
                bss_type: fidl_internal::BssType::Infrastructure,
                beacon_period: BEACON_INTERVAL,
                capability_info: CAPABILITY_INFO.0,
                ies: beacon_ies(),
                rssi_dbm: RX_INFO.rssi_dbm,
                channel: fidl_common::WlanChannel {
                    primary: RX_INFO.channel.primary,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                },
                snr_db: 0,
            }
        );

        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::Success }
        );
    }

    #[test]
    fn test_handle_beacon_or_probe_response_multiple() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");
        handle_beacon(&mut scanner, &mut ctx, &beacon_ies()[..]);
        // Replace with beacon that has different SSID
        handle_beacon(&mut scanner, &mut ctx, &beacon_ies_2()[..]);
        scanner.bind(&mut ctx).handle_channel_req_complete();

        // Verify that one scan result is sent for each beacon
        let scan_result = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanResult>()
            .expect("error reading MLME ScanResult");
        assert_eq!(scan_result.bss.ies, beacon_ies());

        let scan_result = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanResult>()
            .expect("error reading MLME ScanResult");
        assert_eq!(scan_result.bss.ies, beacon_ies_2());

        let scan_end = m
            .fake_device
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::Success }
        );
    }

    #[test]
    fn not_scanning_vs_scanning() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut m = MockObjects::new(&exec);
        let mut ctx = m.make_ctx();
        let mut scanner = Scanner::new(IFACE_MAC);
        assert_eq!(false, scanner.is_scanning());

        scanner
            .bind(&mut ctx)
            .on_sme_scan(
                scan_req(),
                m.listener_state.create_channel_listener_fn(),
                &mut m.chan_sched,
            )
            .expect("expect scan req accepted");
        assert_eq!(true, scanner.is_scanning());
    }

    fn handle_beacon(scanner: &mut Scanner, ctx: &mut Context, ies: &[u8]) {
        scanner.bind(ctx).handle_beacon_or_probe_response(
            BSSID,
            TimeUnit(BEACON_INTERVAL),
            CAPABILITY_INFO,
            ies,
            RX_INFO.clone(),
        );
    }

    const fn channel(primary: u8) -> banjo_common::WlanChannel {
        banjo_common::WlanChannel {
            primary,
            cbw: banjo_common::ChannelBandwidth::CBW20,
            secondary80: 0,
        }
    }

    fn beacon_ies() -> Vec<u8> {
        #[rustfmt::skip]
        let ies = vec![
            // SSID: "ssid"
            0x00, 0x04, 115, 115, 105, 100,
            // Supported rates: 24(B), 36, 48, 54
            0x01, 0x04, 0xb0, 0x48, 0x60, 0x6c,
            // TIM - DTIM count: 0, DTIM period: 1, PVB: 2
            0x05, 0x04, 0x00, 0x01, 0x00, 0x02,
        ];
        ies
    }

    fn beacon_ies_2() -> Vec<u8> {
        #[rustfmt::skip]
        let ies = vec![
            // SSID: "ss"
            0x00, 0x02, 115, 115,
            // Supported rates: 24(B), 36, 48, 54
            0x01, 0x04, 0xb0, 0x48, 0x60, 0x6c,
            // TIM - DTIM count: 0, DTIM period: 1, PVB: 2
            0x05, 0x04, 0x00, 0x01, 0x00, 0x02,
        ];
        ies
    }

    struct MockObjects {
        fake_device: FakeDevice,
        time_stream: TimeStream<TimedEvent>,
        timer: Option<Timer<TimedEvent>>,
        listener_state: MockListenerState,
        chan_sched: ChannelScheduler,
    }

    impl MockObjects {
        fn new(exec: &fasync::TestExecutor) -> Self {
            let (timer, time_stream) = create_timer();
            Self {
                fake_device: FakeDevice::new(exec),
                time_stream,
                timer: Some(timer),
                listener_state: MockListenerState { events: Rc::new(RefCell::new(vec![])) },
                chan_sched: ChannelScheduler::new(),
            }
        }

        fn make_ctx(&mut self) -> Context {
            let device = self.fake_device.as_device();
            self.make_ctx_with_device(device)
        }

        fn make_ctx_with_device(&mut self, device: Device) -> Context {
            Context {
                config: ClientConfig { ensure_on_channel_time: 0 },
                device,
                buf_provider: FakeBufferProvider::new(),
                timer: self.timer.take().unwrap(),
                seq_mgr: SequenceManager::new(),
            }
        }
    }
}
