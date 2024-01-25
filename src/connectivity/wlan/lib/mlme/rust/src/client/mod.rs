// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod channel_switch;
mod convert_beacon;
mod lost_bss;
mod scanner;
mod state;
#[cfg(test)]
mod test_utils;

use {
    crate::{
        akm_algorithm,
        block_ack::BlockAckTx,
        buffer::{BufferProvider, OutBuf},
        ddk_converter,
        device::{self, DeviceOps},
        disconnect::LocallyInitiated,
        error::Error,
        logger,
    },
    anyhow::{self, format_err},
    banjo_fuchsia_wlan_softmac as banjo_wlan_softmac,
    channel_switch::ChannelState,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_minstrel as fidl_minstrel, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fidl_fuchsia_wlan_softmac as fidl_softmac, fuchsia_trace as trace, fuchsia_zircon as zx,
    ieee80211::{Bssid, MacAddr, MacAddrBytes, Ssid},
    scanner::Scanner,
    state::States,
    std::convert::TryInto,
    tracing::{error, warn},
    wlan_common::{
        appendable::Appendable,
        bss::BssDescription,
        buffer_writer::BufferWriter,
        capabilities::{derive_join_capabilities, ClientCapabilities},
        channel::Channel,
        data_writer,
        ie::{self, rsn::rsne, Id, Reader},
        mac::{self, Aid, CapabilityInfo, PowerState},
        mgmt_writer,
        sequence::SequenceManager,
        time::TimeUnit,
        timer::{EventId, Timer},
        wmm,
    },
    wlan_frame_writer::{write_frame, write_frame_with_dynamic_buf, write_frame_with_fixed_buf},
    wlan_sme,
    zerocopy::ByteSlice,
};

pub use scanner::ScanError;

#[derive(Debug, Clone, PartialEq)]
pub enum TimedEvent {
    /// Connecting to AP timed out.
    Connecting,
    /// Timeout for reassociating after a disassociation.
    Reassociating,
    /// Association status update includes checking for auto deauthentication due to beacon loss
    /// and report signal strength
    AssociationStatusCheck,
    /// The delay for a scheduled channel switch has elapsed.
    ChannelSwitch,
}

#[cfg(test)]
impl TimedEvent {
    fn class(&self) -> TimedEventClass {
        match self {
            Self::Connecting => TimedEventClass::Connecting,
            Self::Reassociating => TimedEventClass::Reassociating,
            Self::AssociationStatusCheck => TimedEventClass::AssociationStatusCheck,
            Self::ChannelSwitch => TimedEventClass::ChannelSwitch,
        }
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum TimedEventClass {
    Connecting,
    Reassociating,
    AssociationStatusCheck,
    ChannelSwitch,
}

/// ClientConfig affects time duration used for different timeouts.
/// Originally added to more easily control behavior in tests.
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct ClientConfig {
    pub ensure_on_channel_time: zx::sys::zx_duration_t,
}

pub struct Context<D> {
    _config: ClientConfig,
    device: D,
    buf_provider: BufferProvider,
    timer: Timer<TimedEvent>,
    seq_mgr: SequenceManager,
}

pub struct ClientMlme<D> {
    sta: Option<Client>,
    ctx: Context<D>,
    scanner: Scanner,
    channel_state: ChannelState,
}

impl<D: DeviceOps> crate::MlmeImpl for ClientMlme<D> {
    type Config = ClientConfig;
    type Device = D;
    type TimerEvent = TimedEvent;
    fn new(
        config: Self::Config,
        device: Self::Device,
        buf_provider: BufferProvider,
        timer: Timer<TimedEvent>,
    ) -> Result<Self, anyhow::Error> {
        Self::new(config, device, buf_provider, timer).map_err(From::from)
    }
    fn handle_mlme_request(&mut self, req: wlan_sme::MlmeRequest) -> Result<(), anyhow::Error> {
        Self::handle_mlme_req(self, req).map_err(From::from)
    }
    fn handle_mac_frame_rx(
        &mut self,
        bytes: &[u8],
        rx_info: banjo_fuchsia_wlan_softmac::WlanRxInfo,
    ) {
        trace::duration!("wlan", "ClientMlme::handle_mac_frame_rx");
        Self::on_mac_frame_rx(self, bytes, rx_info)
    }
    fn handle_eth_frame_tx(&mut self, bytes: &[u8]) -> Result<(), anyhow::Error> {
        Self::on_eth_frame_tx(self, bytes).map_err(From::from)
    }
    fn handle_scan_complete(&mut self, status: zx::Status, scan_id: u64) {
        Self::handle_scan_complete(self, status, scan_id);
    }
    fn handle_timeout(&mut self, event_id: EventId, event: TimedEvent) {
        Self::handle_timed_event(self, event_id, event)
    }
    fn access_device(&mut self) -> &mut Self::Device {
        &mut self.ctx.device
    }
}

impl<D> ClientMlme<D> {
    pub fn seq_mgr(&mut self) -> &mut SequenceManager {
        &mut self.ctx.seq_mgr
    }

    fn on_sme_get_iface_counter_stats(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::GetIfaceCounterStatsResponse>,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/43456): Implement stats
        let resp =
            fidl_mlme::GetIfaceCounterStatsResponse::ErrorStatus(zx::sys::ZX_ERR_NOT_SUPPORTED);
        responder.respond(resp);
        Ok(())
    }

    fn on_sme_get_iface_histogram_stats(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::GetIfaceHistogramStatsResponse>,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/43456): Implement stats
        let resp =
            fidl_mlme::GetIfaceHistogramStatsResponse::ErrorStatus(zx::sys::ZX_ERR_NOT_SUPPORTED);
        responder.respond(resp);
        Ok(())
    }

    fn on_sme_list_minstrel_peers(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::MinstrelListResponse>,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/79543): Implement once Minstrel is in Rust.
        error!("ListMinstrelPeers is not supported.");
        let peers = fidl_minstrel::Peers { addrs: vec![] };
        let resp = fidl_mlme::MinstrelListResponse { peers };
        responder.respond(resp);
        Ok(())
    }

    fn on_sme_get_minstrel_stats(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::MinstrelStatsResponse>,
        _addr: &MacAddr,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/79543): Implement once Minstrel is in Rust.
        error!("GetMinstrelStats is not supported.");
        let resp = fidl_mlme::MinstrelStatsResponse { peer: None };
        responder.respond(resp);
        Ok(())
    }
}

impl<D: DeviceOps> ClientMlme<D> {
    pub fn new(
        config: ClientConfig,
        mut device: D,
        buf_provider: BufferProvider,
        timer: Timer<TimedEvent>,
    ) -> Result<Self, Error> {
        logger::init();

        let iface_mac = device::try_query_iface_mac(&mut device)?;
        Ok(Self {
            sta: None,
            ctx: Context {
                _config: config,
                device,
                buf_provider,
                timer,
                seq_mgr: SequenceManager::new(),
            },
            scanner: Scanner::new(iface_mac.into()),
            channel_state: Default::default(),
        })
    }

    pub fn set_main_channel(
        &mut self,
        channel: fidl_common::WlanChannel,
    ) -> Result<(), zx::Status> {
        self.channel_state.bind(&mut self.ctx, &mut self.scanner).set_main_channel(channel)
    }

    pub fn on_mac_frame_rx(&mut self, frame: &[u8], rx_info: banjo_wlan_softmac::WlanRxInfo) {
        trace::duration!("wlan", "ClientMlme::on_mac_frame_rx");
        // TODO(https://fxbug.dev/44487): Send the entire frame to scanner.
        match mac::MacFrame::parse(frame, false) {
            Some(mac::MacFrame::Mgmt { mgmt_hdr, body, .. }) => {
                let bssid = Bssid::from(mgmt_hdr.addr3);
                let frame_ctrl = mgmt_hdr.frame_ctrl;
                match mac::MgmtBody::parse(frame_ctrl.mgmt_subtype(), body) {
                    Some(mac::MgmtBody::Beacon { bcn_hdr, elements }) => {
                        trace::duration!("wlan", "MgmtBody::Beacon");
                        self.scanner.bind(&mut self.ctx).handle_ap_advertisement(
                            bssid,
                            bcn_hdr.beacon_interval,
                            bcn_hdr.capabilities,
                            elements,
                            rx_info,
                        );
                    }
                    Some(mac::MgmtBody::ProbeResp { probe_resp_hdr, elements }) => {
                        trace::duration!("wlan", "MgmtBody::ProbeResp");
                        self.scanner.bind(&mut self.ctx).handle_ap_advertisement(
                            bssid,
                            probe_resp_hdr.beacon_interval,
                            probe_resp_hdr.capabilities,
                            elements,
                            rx_info,
                        )
                    }
                    _ => (),
                }
            }
            _ => (),
        }

        if let Some(sta) = self.sta.as_mut() {
            // Only pass the frame to a BoundClient under the following conditions:
            //   - ChannelState currently has a main channel.
            //   - ClientMlme received the frame on the main channel.
            match self.channel_state.get_main_channel() {
                Some(main_channel) if main_channel.primary == rx_info.channel.primary => sta
                    .bind(&mut self.ctx, &mut self.scanner, &mut self.channel_state)
                    .on_mac_frame(frame, rx_info),
                Some(_) => (),
                // TODO(https://fxbug.dev/124243): This is only reachable because the Client state machine
                // returns to the Joined state and clears the main channel upon deauthentication.
                None => {
                    error!(
                        "Received MAC frame on channel {:?} while main channel is not set.",
                        rx_info.channel
                    );
                }
            }
        }
    }

    pub fn handle_mlme_req(&mut self, req: wlan_sme::MlmeRequest) -> Result<(), Error> {
        use wlan_sme::MlmeRequest as Req;

        match req {
            // Handle non station specific MLME messages first (Join, Scan, etc.)
            Req::Scan(req) => Ok(self.on_sme_scan(req)),
            Req::Connect(req) => self.on_sme_connect(req),
            Req::GetIfaceCounterStats(responder) => self.on_sme_get_iface_counter_stats(responder),
            Req::GetIfaceHistogramStats(responder) => {
                self.on_sme_get_iface_histogram_stats(responder)
            }
            Req::QueryDeviceInfo(responder) => self.on_sme_query_device_info(responder),
            Req::QueryDiscoverySupport(responder) => self.on_sme_query_discovery_support(responder),
            Req::QueryMacSublayerSupport(responder) => {
                self.on_sme_query_mac_sublayer_support(responder)
            }
            Req::QuerySecuritySupport(responder) => self.on_sme_query_security_support(responder),
            Req::QuerySpectrumManagementSupport(responder) => {
                self.on_sme_query_spectrum_management_support(responder)
            }
            Req::ListMinstrelPeers(responder) => self.on_sme_list_minstrel_peers(responder),
            Req::GetMinstrelStats(req, responder) => {
                self.on_sme_get_minstrel_stats(responder, &req.peer_addr.into())
            }
            other_message => match &mut self.sta {
                None => {
                    if let Req::Reconnect(req) = other_message {
                        self.ctx.device.send_mlme_event(fidl_mlme::MlmeEvent::ConnectConf {
                            resp: fidl_mlme::ConnectConfirm {
                                peer_sta_address: req.peer_sta_address,
                                result_code: fidl_ieee80211::StatusCode::DeniedNoAssociationExists,
                                association_id: 0,
                                association_ies: vec![],
                            },
                        })?;
                    }
                    Err(Error::Status(
                        format!(
                            "Failed to handle {} MLME request when this ClientMlme has no sta.",
                            other_message.name()
                        ),
                        zx::Status::BAD_STATE,
                    ))
                }
                Some(sta) => Ok(sta
                    .bind(&mut self.ctx, &mut self.scanner, &mut self.channel_state)
                    .handle_mlme_req(other_message)),
            },
        }
    }

    fn on_sme_scan(&mut self, req: fidl_mlme::ScanRequest) {
        let txn_id = req.txn_id;
        let _ = self.scanner.bind(&mut self.ctx).on_sme_scan(req).map_err(|e| {
            error!("Scan failed in MLME: {:?}", e);
            let code = match e {
                Error::ScanError(scan_error) => scan_error.into(),
                _ => fidl_mlme::ScanResultCode::InternalError,
            };
            self.ctx
                .device
                .send_mlme_event(fidl_mlme::MlmeEvent::OnScanEnd {
                    end: fidl_mlme::ScanEnd { txn_id, code },
                })
                .unwrap_or_else(|e| error!("error sending MLME ScanEnd: {}", e));
        });
    }

    pub fn handle_scan_complete(&mut self, status: zx::Status, scan_id: u64) {
        self.scanner.bind(&mut self.ctx).handle_scan_complete(status, scan_id);
    }

    fn on_sme_connect(&mut self, req: fidl_mlme::ConnectRequest) -> Result<(), Error> {
        // Cancel any ongoing scan so that it doesn't conflict with the connect request
        // TODO(b/254290448): Use enable/disable scanning for better guarantees.
        if let Err(e) = self.scanner.bind(&mut self.ctx).cancel_ongoing_scan() {
            warn!("Failed to cancel ongoing scan before connect: {}.", e);
        }

        let bssid = req.selected_bss.bssid;
        let result = match req.selected_bss.try_into() {
            Ok(bss) => {
                let req = ParsedConnectRequest {
                    selected_bss: bss,
                    connect_failure_timeout: req.connect_failure_timeout,
                    auth_type: req.auth_type,
                    sae_password: req.sae_password,
                    wep_key: req.wep_key.map(|k| *k),
                    security_ie: req.security_ie,
                };
                self.join_device(&req.selected_bss).map(|cap| (req, cap))
            }
            Err(e) => Err(Error::Status(
                format!("Error parsing BssDescription: {:?}", e),
                zx::Status::IO_INVALID,
            )),
        };

        match result {
            Ok((req, client_capabilities)) => {
                self.sta.replace(Client::new(
                    req,
                    device::try_query_iface_mac(&mut self.ctx.device)?,
                    client_capabilities,
                ));
                if let Some(sta) = &mut self.sta {
                    sta.bind(&mut self.ctx, &mut self.scanner, &mut self.channel_state)
                        .start_connecting();
                }
                Ok(())
            }
            Err(e) => {
                error!("Error setting up device for join: {}", e);
                // TODO(https://fxbug.dev/44317): Only one failure code defined in IEEE 802.11-2016 6.3.4.3
                // Can we do better?
                self.ctx.device.send_mlme_event(fidl_mlme::MlmeEvent::ConnectConf {
                    resp: fidl_mlme::ConnectConfirm {
                        peer_sta_address: bssid,
                        result_code: fidl_ieee80211::StatusCode::JoinFailure,
                        association_id: 0,
                        association_ies: vec![],
                    },
                })?;
                Err(e)
            }
        }
    }

    fn join_device(&mut self, bss: &BssDescription) -> Result<ClientCapabilities, Error> {
        let info =
            ddk_converter::mlme_device_info_from_softmac(device::try_query(&mut self.ctx.device)?)?;
        let join_caps = derive_join_capabilities(Channel::from(bss.channel), bss.rates(), &info)
            .map_err(|e| {
                Error::Status(
                    format!("Failed to derive join capabilities: {:?}", e),
                    zx::Status::NOT_SUPPORTED,
                )
            })?;

        self.set_main_channel(bss.channel.into())
            .map_err(|status| Error::Status(format!("Error setting device channel"), status))?;

        let join_bss_request = fidl_common::JoinBssRequest {
            bssid: Some(bss.bssid.to_array()),
            bss_type: Some(fidl_common::BssType::Infrastructure),
            remote: Some(true),
            beacon_period: Some(bss.beacon_period),
            ..Default::default()
        };

        // Configure driver to pass frames from this BSS to MLME. Otherwise they will be dropped.
        self.ctx
            .device
            .join_bss(&join_bss_request)
            .map(|()| join_caps)
            .map_err(|status| Error::Status(format!("Error setting BSS in driver"), status))
    }

    fn on_sme_query_device_info(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_mlme::DeviceInfo>,
    ) -> Result<(), Error> {
        let info =
            ddk_converter::mlme_device_info_from_softmac(device::try_query(&mut self.ctx.device)?)?;
        responder.respond(info);
        Ok(())
    }

    fn on_sme_query_discovery_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::DiscoverySupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_discovery_support(&mut self.ctx.device)?;
        responder.respond(support);
        Ok(())
    }

    fn on_sme_query_mac_sublayer_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::MacSublayerSupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_mac_sublayer_support(&mut self.ctx.device)?;
        responder.respond(support);
        Ok(())
    }

    fn on_sme_query_security_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::SecuritySupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_security_support(&mut self.ctx.device)?;
        responder.respond(support);
        Ok(())
    }

    fn on_sme_query_spectrum_management_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::SpectrumManagementSupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_spectrum_management_support(&mut self.ctx.device)?;
        responder.respond(support);
        Ok(())
    }

    pub fn on_eth_frame_tx<B: ByteSlice>(&mut self, bytes: B) -> Result<(), Error> {
        match self.sta.as_mut() {
            None => Err(Error::Status(
                format!("Ethernet frame dropped (Client does not exist)."),
                zx::Status::BAD_STATE,
            )),
            Some(sta) => sta
                .bind(&mut self.ctx, &mut self.scanner, &mut self.channel_state)
                .on_eth_frame_tx(bytes),
        }
    }

    /// Called when a previously scheduled `TimedEvent` fired.
    /// Return true if auto-deauth has triggered. Return false otherwise.
    pub fn handle_timed_event(&mut self, event_id: EventId, event: TimedEvent) {
        if let Some(sta) = self.sta.as_mut() {
            return sta
                .bind(&mut self.ctx, &mut self.scanner, &mut self.channel_state)
                .handle_timed_event(event, event_id);
        }
    }
}

/// A STA running in Client mode.
/// The Client STA is in its early development process and does not yet manage its internal state
/// machine or track negotiated capabilities.
pub struct Client {
    state: Option<States>,
    pub connect_req: ParsedConnectRequest,
    pub iface_mac: MacAddr,
    pub client_capabilities: ClientCapabilities,
    pub connect_timeout: Option<EventId>,
}

impl Client {
    pub fn new(
        connect_req: ParsedConnectRequest,
        iface_mac: MacAddr,
        client_capabilities: ClientCapabilities,
    ) -> Self {
        Self {
            state: Some(States::new_initial()),
            connect_req,
            iface_mac,
            client_capabilities,
            connect_timeout: None,
        }
    }

    pub fn ssid(&self) -> &Ssid {
        &self.connect_req.selected_bss.ssid
    }

    pub fn bssid(&self) -> Bssid {
        self.connect_req.selected_bss.bssid
    }

    pub fn beacon_period(&self) -> zx::Duration {
        zx::Duration::from(TimeUnit(self.connect_req.selected_bss.beacon_period))
    }

    pub fn eapol_required(&self) -> bool {
        self.connect_req.selected_bss.rsne().is_some()
        // TODO(https://fxbug.dev/61020): Add detection of WPA1 in softmac for testing
        // purposes only. In particular, connect-to-wpa1-network relies
        // on this half of the OR statement.
            || self.connect_req.selected_bss.find_wpa_ie().is_some()
    }

    pub fn bind<'a, D>(
        &'a mut self,
        ctx: &'a mut Context<D>,
        scanner: &'a mut Scanner,
        channel_state: &'a mut ChannelState,
    ) -> BoundClient<'a, D> {
        BoundClient { sta: self, ctx, scanner, channel_state }
    }

    pub fn pre_switch_off_channel<D: DeviceOps>(&mut self, ctx: &mut Context<D>) {
        // Safe to unwrap() because state is never None.
        let mut state = self.state.take().unwrap();
        state.pre_switch_off_channel(self, ctx);
        self.state.replace(state);
    }

    pub fn handle_back_on_channel<D: DeviceOps>(&mut self, ctx: &mut Context<D>) {
        // Safe to unwrap() because state is never None.
        let mut state = self.state.take().unwrap();
        state.handle_back_on_channel(self, ctx);
        self.state.replace(state);
    }

    /// Sends a power management data frame to the associated AP indicating that the client has
    /// entered the given power state. See `PowerState`.
    ///
    /// # Errors
    ///
    /// Returns an error if the data frame cannot be sent to the AP.
    fn send_power_state_frame<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        state: PowerState,
    ) -> Result<(), Error> {
        let (buf, bytes_written) = write_frame!(&mut ctx.buf_provider, {
            headers: {
                mac::FixedDataHdrFields: &mac::FixedDataHdrFields {
                    frame_ctrl: mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::DATA)
                        .with_data_subtype(mac::DataSubtype(0).with_null(true))
                        .with_power_mgmt(state)
                        .with_to_ds(true),
                    duration: 0,
                    addr1: self.bssid().into(),
                    addr2: self.iface_mac,
                    addr3: self.bssid().into(),
                    seq_ctrl: mac::SequenceControl(0)
                        .with_seq_num(ctx.seq_mgr.next_sns1(&self.bssid().into()) as u16)
                },
            },
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        ctx.device
            .send_wlan_frame(out_buf, 0)
            .map_err(|error| Error::Status(format!("error sending power management frame"), error))
    }

    /// Only management and data frames should be processed. Furthermore, the source address should
    /// be the BSSID the client associated to and the receiver address should either be non-unicast
    /// or the client's MAC address.
    fn should_handle_frame<B: ByteSlice>(&self, mac_frame: &mac::MacFrame<B>) -> bool {
        trace::duration!("wlan", "Client::should_handle_frame");

        // Technically, |transmitter_addr| and |receiver_addr| would be more accurate but using src
        // src and dst to be consistent with |data_dst_addr()|.
        let (src_addr, dst_addr) = match mac_frame {
            mac::MacFrame::Mgmt { mgmt_hdr, .. } => (Some(mgmt_hdr.addr3), mgmt_hdr.addr1),
            mac::MacFrame::Data { fixed_fields, .. } => {
                (mac::data_bssid(&fixed_fields), mac::data_dst_addr(&fixed_fields))
            }
            // Control frames are not supported. Drop them.
            _ => return false,
        };
        src_addr.is_some_and(|src_addr| src_addr == self.bssid().into())
            && (!dst_addr.is_unicast() || dst_addr == self.iface_mac)
    }
}

pub struct BoundClient<'a, D> {
    sta: &'a mut Client,
    // TODO(https://fxbug.dev/44079): pull everything out of Context and plop them here.
    ctx: &'a mut Context<D>,
    scanner: &'a mut Scanner,
    channel_state: &'a mut ChannelState,
}

impl<'a, D: DeviceOps> akm_algorithm::AkmAction for BoundClient<'a, D> {
    fn send_auth_frame(
        &mut self,
        auth_type: mac::AuthAlgorithmNumber,
        seq_num: u16,
        result_code: mac::StatusCode,
        auth_content: &[u8],
    ) -> Result<(), anyhow::Error> {
        self.send_auth_frame(auth_type, seq_num, result_code, auth_content).map_err(|e| e.into())
    }

    fn forward_sme_sae_rx(
        &mut self,
        seq_num: u16,
        status_code: fidl_ieee80211::StatusCode,
        sae_fields: Vec<u8>,
    ) {
        self.forward_sae_frame_rx(seq_num, status_code, sae_fields)
    }

    fn forward_sae_handshake_ind(&mut self) {
        self.forward_sae_handshake_ind()
    }
}

impl<'a, D: DeviceOps> BoundClient<'a, D> {
    /// Delivers a single MSDU to the STA's underlying device. The MSDU is delivered as an
    /// Ethernet II frame.
    /// Returns Err(_) if writing or delivering the Ethernet II frame failed.
    fn deliver_msdu<B: ByteSlice>(&mut self, msdu: mac::Msdu<B>) -> Result<(), Error> {
        let mac::Msdu { dst_addr, src_addr, llc_frame } = msdu;

        let (buf, bytes_written) = write_frame_with_fixed_buf!([0u8; mac::MAX_ETH_FRAME_LEN], {
            headers: {
                mac::EthernetIIHdr: &mac::EthernetIIHdr {
                    da: dst_addr,
                    sa: src_addr,
                    ether_type: llc_frame.hdr.protocol_id,
                },
            },
            payload: &llc_frame.body,
        })?;
        let (written, _remaining) = buf.split_at(bytes_written);
        self.ctx
            .device
            .deliver_eth_frame(written)
            .map_err(|s| Error::Status(format!("could not deliver Ethernet II frame"), s))
    }

    pub fn send_auth_frame(
        &mut self,
        auth_type: mac::AuthAlgorithmNumber,
        seq_num: u16,
        result_code: mac::StatusCode,
        auth_content: &[u8],
    ) -> Result<(), Error> {
        let (buf, bytes_written) = write_frame!(&mut self.ctx.buf_provider, {
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::AUTH),
                    self.sta.bssid(),
                    self.sta.iface_mac,
                    mac::SequenceControl(0)
                        .with_seq_num(self.ctx.seq_mgr.next_sns1(&self.sta.bssid().into()) as u16)
                ),
                mac::AuthHdr: &mac::AuthHdr {
                    auth_alg_num: auth_type,
                    auth_txn_seq_num: seq_num,
                    status_code: result_code,
                },
            },
            body: auth_content,
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        self.send_mgmt_or_ctrl_frame(out_buf)
            .map_err(|s| Error::Status(format!("error sending open auth frame"), s))
    }

    /// Sends an authentication frame using Open System authentication.
    pub fn send_open_auth_frame(&mut self) -> Result<(), Error> {
        self.send_auth_frame(
            mac::AuthAlgorithmNumber::OPEN,
            1,
            fidl_ieee80211::StatusCode::Success.into(),
            &[],
        )
    }

    /// Sends an association request frame based on device capability.
    // TODO(https://fxbug.dev/39148): Use an IE set instead of individual IEs.
    pub fn send_assoc_req_frame(&mut self) -> Result<(), Error> {
        let ssid = self.sta.ssid().clone();
        let cap = &self.sta.client_capabilities.0;
        let capability_info = cap.capability_info.0;
        let rates: Vec<u8> = cap.rates.iter().map(|r| r.rate()).collect();
        let ht_cap = cap.ht_cap;
        let vht_cap = cap.vht_cap;
        let security_ie = self.sta.connect_req.security_ie.clone();

        let (buf, bytes_written) = write_frame!(&mut self.ctx.buf_provider, {
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::ASSOC_REQ),
                    self.sta.bssid(),
                    self.sta.iface_mac,
                    mac::SequenceControl(0)
                        .with_seq_num(self.ctx.seq_mgr.next_sns1(&self.sta.bssid().into()) as u16)
                ),
                mac::AssocReqHdr: &mac::AssocReqHdr {
                    capabilities: mac::CapabilityInfo(capability_info),
                    listen_interval: 0,
                },
            },
            ies: {
                ssid: ssid,
                supported_rates: rates,
                extended_supported_rates: {/* continue rates */},
                rsne?: if !security_ie.is_empty() && security_ie[0] == ie::Id::RSNE.0 {
                    rsne::from_bytes(&security_ie[..])
                        .map_err(|e| format_err!("error parsing rsne {:?} : {:?}", security_ie, e))?
                        .1
                },
                ht_cap?: ht_cap,
                vht_cap?: vht_cap,
            },
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        self.send_mgmt_or_ctrl_frame(out_buf)
            .map_err(|s| Error::Status(format!("error sending assoc req frame"), s))
    }

    /// Sends a "keep alive" response to the BSS. A keep alive response is a NULL data frame sent as
    /// a response to the AP transmitting NULL data frames to the client.
    // Note: This function was introduced to meet C++ MLME feature parity. However, there needs to
    // be some investigation, whether these "keep alive" frames are the right way of keeping a
    // client associated to legacy APs.
    fn send_keep_alive_resp_frame(&mut self) -> Result<(), Error> {
        let (buf, bytes_written) = write_frame!(&mut self.ctx.buf_provider, {
            headers: {
                mac::FixedDataHdrFields: &data_writer::data_hdr_client_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::DATA)
                        .with_data_subtype(mac::DataSubtype(0).with_null(true)),
                    self.sta.bssid(),
                    self.sta.iface_mac,
                    mac::SequenceControl(0)
                        .with_seq_num(self.ctx.seq_mgr.next_sns1(&self.sta.bssid().into()) as u16)
                ),
            },
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        self.ctx
            .device
            .send_wlan_frame(out_buf, 0)
            .map_err(|s| Error::Status(format!("error sending keep alive frame"), s))
    }

    pub fn send_deauth_frame(&mut self, reason_code: mac::ReasonCode) -> Result<(), Error> {
        let (buf, bytes_written) = write_frame!(&mut self.ctx.buf_provider, {
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::DEAUTH),
                    self.sta.bssid(),
                    self.sta.iface_mac,
                    mac::SequenceControl(0)
                        .with_seq_num(self.ctx.seq_mgr.next_sns1(&self.sta.bssid().into()) as u16)
                ),
                mac::DeauthHdr: &mac::DeauthHdr {
                    reason_code,
                },
            },
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        let result = self
            .send_mgmt_or_ctrl_frame(out_buf)
            .map_err(|s| Error::Status(format!("error sending deauthenticate frame"), s));
        // Clear main_channel since there is no "main channel" after deauthenticating
        self.channel_state.bind(&mut self.ctx, &mut self.scanner).clear_main_channel();

        result
    }

    /// Sends the given payload as a data frame over the air.
    pub fn send_data_frame(
        &mut self,
        src: MacAddr,
        dst: MacAddr,
        is_protected: bool,
        qos_ctrl: bool,
        ether_type: u16,
        payload: &[u8],
    ) -> Result<(), Error> {
        let qos_ctrl = if qos_ctrl {
            Some(
                wmm::derive_tid(ether_type, payload)
                    .map_or(mac::QosControl(0), |tid| mac::QosControl(0).with_tid(tid as u16)),
            )
        } else {
            None
        };

        // IEEE Std 802.11-2016, Table 9-26 specifies address field contents and their relation
        // to the addr fields.
        // TODO(https://fxbug.dev/51295): Support A-MSDU address field contents.

        // We do not currently support RA other than the BSS.
        // TODO(https://fxbug.dev/45833): Support to_ds = false and alternative RA for TDLS.
        let to_ds = true;
        let from_ds = src != self.sta.iface_mac;
        // Detect when SA != TA, in which case we use addr4.
        let addr1 = self.sta.bssid().into();
        let addr2 = self.sta.iface_mac;
        let addr3 = match (to_ds, from_ds) {
            (false, false) => self.sta.bssid().into(),
            (false, true) => src,
            (true, _) => dst,
        };
        let addr4 = if from_ds && to_ds { Some(src) } else { None };

        let (buf, bytes_written) = write_frame!(&mut self.ctx.buf_provider, {
            headers: {
                mac::FixedDataHdrFields: &mac::FixedDataHdrFields {
                    frame_ctrl: mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::DATA)
                        .with_data_subtype(mac::DataSubtype(0).with_qos(qos_ctrl.is_some()))
                        .with_protected(is_protected)
                        .with_to_ds(to_ds)
                        .with_from_ds(from_ds),
                    duration: 0,
                    addr1,
                    addr2,
                    addr3,
                    seq_ctrl: mac::SequenceControl(0).with_seq_num(
                        match qos_ctrl.as_ref() {
                            None => self.ctx.seq_mgr.next_sns1(&dst),
                            Some(qos_ctrl) => self.ctx.seq_mgr.next_sns2(&dst, qos_ctrl.tid()),
                        } as u16
                    )
                },
                mac::Addr4?: addr4,
                mac::QosControl?: qos_ctrl,
                mac::LlcHdr: &data_writer::make_snap_llc_hdr(ether_type),
            },
            payload: payload,
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        let tx_flags = match ether_type {
            mac::ETHER_TYPE_EAPOL => banjo_wlan_softmac::WlanTxInfoFlags::FAVOR_RELIABILITY.0,
            _ => 0,
        };
        self.ctx
            .device
            .send_wlan_frame(out_buf, tx_flags)
            .map_err(|s| Error::Status(format!("error sending data frame"), s))
    }

    /// Sends an MLME-EAPOL.indication to MLME's SME peer.
    /// Note: MLME-EAPOL.indication is a custom Fuchsia primitive and not defined in IEEE 802.11.
    fn send_eapol_indication(
        &mut self,
        src_addr: MacAddr,
        dst_addr: MacAddr,
        eapol_frame: &[u8],
    ) -> Result<(), Error> {
        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::EapolInd {
                ind: fidl_mlme::EapolIndication {
                    src_addr: src_addr.to_array(),
                    dst_addr: dst_addr.to_array(),
                    data: eapol_frame.to_vec(),
                },
            })
            .map_err(|e| e.into())
    }

    /// Sends an EAPoL frame over the air and reports transmission status to SME via an
    /// MLME-EAPOL.confirm message.
    pub fn send_eapol_frame(
        &mut self,
        src: MacAddr,
        dst: MacAddr,
        is_protected: bool,
        eapol_frame: &[u8],
    ) {
        // TODO(https://fxbug.dev/34910): EAPoL frames can be send in QoS data frames. However, Fuchsia's old C++
        // MLME never sent EAPoL frames in QoS data frames. For feature parity do the same.
        let result = self.send_data_frame(
            src,
            dst,
            is_protected,
            false, /* don't use QoS */
            mac::ETHER_TYPE_EAPOL,
            eapol_frame,
        );
        let result_code = match result {
            Ok(()) => fidl_mlme::EapolResultCode::Success,
            Err(e) => {
                error!("error sending EAPoL frame: {}", e);
                fidl_mlme::EapolResultCode::TransmissionFailure
            }
        };

        // Report transmission result to SME.
        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::EapolConf {
                resp: fidl_mlme::EapolConfirm { result_code, dst_addr: dst.to_array() },
            })
            .unwrap_or_else(|e| error!("error sending MLME-EAPOL.confirm message: {}", e));
    }

    pub fn send_ps_poll_frame(&mut self, aid: Aid) -> Result<(), Error> {
        const PS_POLL_ID_MASK: u16 = 0b11000000_00000000;

        let (buf, bytes_written) = write_frame!(&mut self.ctx.buf_provider, {
            headers: {
                mac::FrameControl: &mac::FrameControl(0)
                    .with_frame_type(mac::FrameType::CTRL)
                    .with_ctrl_subtype(mac::CtrlSubtype::PS_POLL),
                mac::PsPoll: &mac::PsPoll {
                    // IEEE 802.11-2016 9.3.1.5 states the ID in the PS-Poll frame is the
                    // association ID with the 2 MSBs set to 1.
                    masked_aid: aid | PS_POLL_ID_MASK,
                    bssid: self.sta.bssid(),
                    ta: self.sta.iface_mac,
                },
            },
        })?;
        let out_buf = OutBuf::from(buf, bytes_written);
        self.send_mgmt_or_ctrl_frame(out_buf)
            .map_err(|s| Error::Status(format!("error sending PS-Poll frame"), s))
    }

    /// Called when a previously scheduled `TimedEvent` fired.
    pub fn handle_timed_event(&mut self, event: TimedEvent, event_id: EventId) {
        self.sta.state = Some(self.sta.state.take().unwrap().on_timed_event(self, event, event_id))
    }

    /// Called when an arbitrary frame was received over the air.
    pub fn on_mac_frame<B: ByteSlice>(
        &mut self,
        bytes: B,
        rx_info: banjo_wlan_softmac::WlanRxInfo,
    ) {
        trace::duration!("wlan", "BoundClient::on_mac_frame");
        // Safe: |state| is never None and always replaced with Some(..).
        self.sta.state = Some(self.sta.state.take().unwrap().on_mac_frame(self, bytes, rx_info));
    }

    pub fn on_eth_frame_tx<B: ByteSlice>(&mut self, frame: B) -> Result<(), Error> {
        // Safe: |state| is never None and always replaced with Some(..).
        let state = self.sta.state.take().unwrap();
        let result = state.on_eth_frame(self, frame);
        self.sta.state.replace(state);
        result
    }

    pub fn start_connecting(&mut self) {
        // Safe: |state| is never None and always replaced with Some(..).
        let next_state = self.sta.state.take().unwrap().start_connecting(self);
        self.sta.state.replace(next_state);
    }

    pub fn handle_mlme_req(&mut self, msg: wlan_sme::MlmeRequest) {
        // Safe: |state| is never None and always replaced with Some(..).
        let next_state = self.sta.state.take().unwrap().handle_mlme_req(self, msg);
        self.sta.state.replace(next_state);
    }

    fn send_connect_conf_failure(&mut self, result_code: fidl_ieee80211::StatusCode) {
        self.sta.connect_timeout.take();
        let bssid = self.sta.connect_req.selected_bss.bssid;
        self.send_connect_conf_failure_with_bssid(bssid, result_code);
    }

    /// Send ConnectConf failure with BSSID specified.
    /// The connect timeout is not cleared as this method may be called with a foreign BSSID.
    fn send_connect_conf_failure_with_bssid(
        &mut self,
        bssid: Bssid,
        result_code: fidl_ieee80211::StatusCode,
    ) {
        let connect_conf = fidl_mlme::ConnectConfirm {
            peer_sta_address: bssid.to_array(),
            result_code,
            association_id: 0,
            association_ies: vec![],
        };
        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::ConnectConf { resp: connect_conf })
            .unwrap_or_else(|e| error!("error sending MLME-CONNECT.confirm: {}", e));
    }

    fn send_connect_conf_success(&mut self, association_id: u16, association_ies: &[u8]) {
        self.sta.connect_timeout.take();
        let connect_conf = fidl_mlme::ConnectConfirm {
            peer_sta_address: self.sta.connect_req.selected_bss.bssid.to_array(),
            result_code: fidl_ieee80211::StatusCode::Success,
            association_id,
            association_ies: association_ies.to_vec(),
        };
        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::ConnectConf { resp: connect_conf })
            .unwrap_or_else(|e| error!("error sending MLME-CONNECT.confirm: {}", e));
    }

    /// Sends an MLME-DEAUTHENTICATE.indication message to the joined BSS.
    fn send_deauthenticate_ind(
        &mut self,
        reason_code: fidl_ieee80211::ReasonCode,
        locally_initiated: LocallyInitiated,
    ) {
        // Clear main_channel since there is no "main channel" after deauthenticating
        self.channel_state.bind(&mut self.ctx, &mut self.scanner).clear_main_channel();

        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::DeauthenticateInd {
                ind: fidl_mlme::DeauthenticateIndication {
                    peer_sta_address: self.sta.bssid().to_array(),
                    reason_code,
                    locally_initiated: locally_initiated.0,
                },
            })
            .unwrap_or_else(|e| error!("error sending MLME-DEAUTHENTICATE.indication: {}", e));
    }

    /// Sends an MLME-DISASSOCIATE.indication message to the joined BSS.
    fn send_disassoc_ind(
        &mut self,
        reason_code: fidl_ieee80211::ReasonCode,
        locally_initiated: LocallyInitiated,
    ) {
        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::DisassociateInd {
                ind: fidl_mlme::DisassociateIndication {
                    peer_sta_address: self.sta.bssid().to_array(),
                    reason_code,
                    locally_initiated: locally_initiated.0,
                },
            })
            .unwrap_or_else(|e| error!("error sending MLME-DISASSOCIATE.indication: {}", e));
    }

    fn clear_association(&mut self) -> Result<(), zx::Status> {
        self.ctx.device.clear_association(&fidl_softmac::WlanSoftmacBaseClearAssociationRequest {
            peer_addr: Some(self.sta.bssid().to_array()),
            ..Default::default()
        })
    }

    /// Sends an sae frame rx message to the SME.
    fn forward_sae_frame_rx(
        &mut self,
        seq_num: u16,
        status_code: fidl_ieee80211::StatusCode,
        sae_fields: Vec<u8>,
    ) {
        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::OnSaeFrameRx {
                frame: fidl_mlme::SaeFrame {
                    peer_sta_address: self.sta.bssid().to_array(),
                    seq_num,
                    status_code,
                    sae_fields,
                },
            })
            .unwrap_or_else(|e| error!("error sending OnSaeFrameRx: {}", e));
    }

    fn forward_sae_handshake_ind(&mut self) {
        self.ctx
            .device
            .send_mlme_event(fidl_mlme::MlmeEvent::OnSaeHandshakeInd {
                ind: fidl_mlme::SaeHandshakeIndication {
                    peer_sta_address: self.sta.bssid().to_array(),
                },
            })
            .unwrap_or_else(|e| error!("error sending OnSaeHandshakeInd: {}", e));
    }

    fn send_mgmt_or_ctrl_frame(&mut self, out_buf: OutBuf) -> Result<(), zx::Status> {
        self.ctx.device.send_wlan_frame(out_buf, 0)
    }
}

pub struct ParsedConnectRequest {
    pub selected_bss: BssDescription,
    pub connect_failure_timeout: u32,
    pub auth_type: fidl_mlme::AuthenticationTypes,
    pub sae_password: Vec<u8>,
    pub wep_key: Option<fidl_mlme::SetKeyDescriptor>,
    pub security_ie: Vec<u8>,
}

pub struct ParsedAssociateResp {
    pub association_id: u16,
    pub capabilities: CapabilityInfo,
    pub rates: Vec<ie::SupportedRate>,
    pub ht_cap: Option<ie::HtCapabilities>,
    pub vht_cap: Option<ie::VhtCapabilities>,
}

impl ParsedAssociateResp {
    pub fn from<B: ByteSlice>(assoc_resp_hdr: &mac::AssocRespHdr, elements: B) -> Self {
        let mut parsed_assoc_resp = ParsedAssociateResp {
            association_id: assoc_resp_hdr.aid,
            capabilities: assoc_resp_hdr.capabilities,
            rates: vec![],
            ht_cap: None,
            vht_cap: None,
        };

        for (id, body) in Reader::new(elements) {
            match id {
                Id::SUPPORTED_RATES => match ie::parse_supported_rates(body) {
                    Err(e) => warn!("invalid Supported Rates: {}", e),
                    Ok(supported_rates) => {
                        // safe to unwrap because supported rate is 1-byte long thus always aligned
                        parsed_assoc_resp.rates.extend(supported_rates.iter());
                    }
                },
                Id::EXTENDED_SUPPORTED_RATES => match ie::parse_extended_supported_rates(body) {
                    Err(e) => warn!("invalid Extended Supported Rates: {}", e),
                    Ok(supported_rates) => {
                        // safe to unwrap because supported rate is 1-byte long thus always aligned
                        parsed_assoc_resp.rates.extend(supported_rates.iter());
                    }
                },
                Id::HT_CAPABILITIES => match ie::parse_ht_capabilities(body) {
                    Err(e) => warn!("invalid HT Capabilities: {}", e),
                    Ok(ht_cap) => {
                        parsed_assoc_resp.ht_cap = Some(*ht_cap);
                    }
                },
                Id::VHT_CAPABILITIES => match ie::parse_vht_capabilities(body) {
                    Err(e) => warn!("invalid VHT Capabilities: {}", e),
                    Ok(vht_cap) => {
                        parsed_assoc_resp.vht_cap = Some(*vht_cap);
                    }
                },
                // TODO(https://fxbug.dev/43938): parse vendor ID and include WMM param if exists
                _ => {}
            }
        }
        parsed_assoc_resp
    }
}

impl<'a, D: DeviceOps> BlockAckTx for BoundClient<'a, D> {
    /// Sends a BlockAck frame to the associated AP.
    ///
    /// BlockAck frames are described by 802.11-2016, section 9.6.5.2, 9.6.5.3, and 9.6.5.4.
    fn send_block_ack_frame(&mut self, n: usize, body: &[u8]) -> Result<(), Error> {
        let mut buffer = self.ctx.buf_provider.get_buffer(n)?;
        let mut writer = BufferWriter::new(&mut buffer[..]);
        write_block_ack_hdr(
            &mut writer,
            self.sta.bssid(),
            self.sta.iface_mac,
            &mut self.ctx.seq_mgr,
        )
        .and_then(|_| writer.append_bytes(body).map_err(Into::into))?;
        let n = writer.bytes_written();
        let buffer = OutBuf::from(buffer, n);
        self.send_mgmt_or_ctrl_frame(buffer)
            .map_err(|status| Error::Status(format!("error sending BlockAck frame"), status))
    }
}

/// Writes the header of the management frame for BlockAck frames to the given buffer.
///
/// The address may be that of the originator or recipient. The frame formats are described by IEEE
/// Std 802.11-2016, 9.6.5.
fn write_block_ack_hdr<B: Appendable>(
    buffer: &mut B,
    bssid: Bssid,
    addr: MacAddr,
    seq_mgr: &mut SequenceManager,
) -> Result<usize, Error> {
    // The management header differs for APs and clients. The frame control and management header
    // are constructed here, but AP and client STAs share the code that constructs the body. See
    // the `block_ack` module.
    write_frame_with_dynamic_buf!(
        buffer,
        {
            headers: {
                mac::MgmtHdr: &mgmt_writer::mgmt_hdr_to_ap(
                    mac::FrameControl(0)
                        .with_frame_type(mac::FrameType::MGMT)
                        .with_mgmt_subtype(mac::MgmtSubtype::ACTION),
                    bssid,
                    addr,
                    mac::SequenceControl(0)
                        .with_seq_num(seq_mgr.next_sns1(&bssid.into()) as u16),
                ),
            },
        }
    )
    .map(|(_, n)| n)
}

#[cfg(test)]
mod tests {
    use {
        super::{state::DEFAULT_AUTO_DEAUTH_TIMEOUT_BEACON_COUNT, *},
        crate::{
            block_ack::{self, BlockAckState, Closed, ADDBA_REQ_FRAME_LEN, ADDBA_RESP_FRAME_LEN},
            buffer::FakeBufferProvider,
            client::{lost_bss::LostBssCounter, test_utils::drain_timeouts},
            device::{test_utils, FakeDevice, FakeDeviceConfig, FakeDeviceState, LinkStatus},
            test_utils::{fake_wlan_channel, MockWlanRxInfo},
        },
        banjo_fuchsia_wlan_common as banjo_common, fidl_fuchsia_wlan_common as fidl_common,
        fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync,
        fuchsia_sync::Mutex,
        futures::task::Poll,
        ieee80211::MacAddrBytes,
        lazy_static::lazy_static,
        std::{convert::TryFrom, sync::Arc},
        wlan_common::{
            assert_variant,
            capabilities::StaCapabilities,
            channel::Cbw,
            fake_bss_description, fake_fidl_bss_description, ie,
            stats::SignalStrengthAverage,
            test_utils::{fake_capabilities::fake_client_capabilities, fake_frames::*},
            timer::{self, create_timer},
        },
        wlan_sme::responder::Responder,
        wlan_statemachine::*,
    };
    lazy_static! {
        static ref BSSID: Bssid = [6u8; 6].into();
        static ref IFACE_MAC: MacAddr = [7u8; 6].into();
    }
    const RSNE: &[u8] = &[
        0x30, 0x14, //  ID and len
        1, 0, //  version
        0x00, 0x0f, 0xac, 0x04, //  group data cipher suite
        0x01, 0x00, //  pairwise cipher suite count
        0x00, 0x0f, 0xac, 0x04, //  pairwise cipher suite list
        0x01, 0x00, //  akm suite count
        0x00, 0x0f, 0xac, 0x02, //  akm suite list
        0xa8, 0x04, //  rsn capabilities
    ];
    const SCAN_CHANNEL_PRIMARY: u8 = 6;
    // Note: not necessarily valid beacon frame.
    #[rustfmt::skip]
    const BEACON_FRAME: &'static [u8] = &[
        // Mgmt header
        0b10000000, 0, // Frame Control
        0, 0, // Duration
        255, 255, 255, 255, 255, 255, // addr1
        6, 6, 6, 6, 6, 6, // addr2
        6, 6, 6, 6, 6, 6, // addr3
        0, 0, // Sequence Control
        // Beacon header:
        0, 0, 0, 0, 0, 0, 0, 0, // Timestamp
        10, 0, // Beacon interval
        33, 0, // Capabilities
        // IEs:
        0, 4, 0x73, 0x73, 0x69, 0x64, // SSID - "ssid"
        1, 8, 1, 2, 3, 4, 5, 6, 7, 8, // Supported rates
        3, 1, 11, // DSSS parameter set - channel 11
        5, 4, 0, 0, 0, 0, // TIM
    ];

    struct MockObjects {
        fake_device: FakeDevice,
        fake_device_state: Arc<Mutex<FakeDeviceState>>,
        timer: Option<Timer<super::TimedEvent>>,
        time_stream: timer::EventStream<super::TimedEvent>,
    }

    impl MockObjects {
        fn new(executor: &fasync::TestExecutor) -> Self {
            let (timer, time_stream) = create_timer();
            let (fake_device, fake_device_state) = FakeDevice::new_with_config(
                executor,
                FakeDeviceConfig::default()
                    .with_mock_mac_role(fidl_common::WlanMacRole::Client)
                    .with_mock_sta_addr((*IFACE_MAC).to_array()),
            );
            Self { fake_device, fake_device_state, timer: Some(timer), time_stream }
        }

        fn make_mlme(&mut self) -> ClientMlme<FakeDevice> {
            let mut mlme = ClientMlme::new(
                Default::default(),
                self.fake_device.clone(),
                FakeBufferProvider::new(),
                self.timer.take().unwrap(),
            )
            .expect("Failed to create client MLME.");
            mlme.set_main_channel(fake_wlan_channel().into()).expect("unable to set main channel");
            mlme
        }
    }

    fn scan_req() -> fidl_mlme::ScanRequest {
        fidl_mlme::ScanRequest {
            txn_id: 1337,
            scan_type: fidl_mlme::ScanTypes::Passive,
            channel_list: vec![SCAN_CHANNEL_PRIMARY],
            ssid_list: vec![Ssid::try_from("ssid").unwrap().into()],
            probe_delay: 0,
            min_channel_time: 100,
            max_channel_time: 300,
        }
    }

    fn make_client_station() -> Client {
        let connect_req = ParsedConnectRequest {
            selected_bss: fake_bss_description!(Open, bssid: BSSID.to_array()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        };
        Client::new(connect_req, *IFACE_MAC, fake_client_capabilities())
    }

    fn make_client_station_protected() -> Client {
        let connect_req = ParsedConnectRequest {
            selected_bss: fake_bss_description!(Wpa2, bssid: BSSID.to_array()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: RSNE.to_vec(),
        };
        Client::new(connect_req, *IFACE_MAC, fake_client_capabilities())
    }

    impl ClientMlme<FakeDevice> {
        fn make_client_station(&mut self) {
            self.sta.replace(make_client_station());
        }

        fn make_client_station_protected(&mut self) {
            self.sta.replace(make_client_station_protected());
        }

        fn get_bound_client(&mut self) -> Option<BoundClient<'_, FakeDevice>> {
            match self.sta.as_mut() {
                None => None,
                Some(sta) => {
                    Some(sta.bind(&mut self.ctx, &mut self.scanner, &mut self.channel_state))
                }
            }
        }
    }

    impl BoundClient<'_, FakeDevice> {
        fn move_to_associated_state(&mut self) {
            use super::state::*;
            let status_check_timeout =
                schedule_association_status_timeout(self.sta.beacon_period(), &mut self.ctx.timer);
            let state =
                States::from(wlan_statemachine::testing::new_state(Associated(Association {
                    aid: 42,
                    assoc_resp_ies: vec![],
                    controlled_port_open: true,
                    ap_ht_op: None,
                    ap_vht_op: None,
                    qos: Qos::Disabled,
                    lost_bss_counter: LostBssCounter::start(
                        self.sta.beacon_period(),
                        DEFAULT_AUTO_DEAUTH_TIMEOUT_BEACON_COUNT,
                    ),
                    status_check_timeout,
                    signal_strength_average: SignalStrengthAverage::new(),
                    block_ack_state: StateMachine::new(BlockAckState::from(State::new(Closed))),
                })));
            self.sta.state.replace(state);
        }

        fn close_controlled_port(&mut self) {
            self.handle_mlme_req(wlan_sme::MlmeRequest::SetCtrlPort(
                fidl_mlme::SetControlledPortRequest {
                    peer_sta_address: BSSID.to_array(),
                    state: fidl_mlme::ControlledPortState::Closed,
                },
            ));
        }
    }

    #[test]
    fn spawns_new_sta_on_connect_request_from_sme() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        assert!(me.get_bound_client().is_none(), "MLME should not contain client, yet");
        me.on_sme_connect(fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Open, ssid: Ssid::try_from("foo").unwrap()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        })
        .expect("valid ConnectRequest should be handled successfully");
        me.get_bound_client().expect("client sta should have been created by now.");
    }

    #[test]
    fn fails_to_connect_if_channel_unknown() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        assert!(me.get_bound_client().is_none(), "MLME should not contain client, yet");
        let mut req = fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Open, ssid: Ssid::try_from("foo").unwrap()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        };

        req.selected_bss.channel.cbw = fidl_fuchsia_wlan_common::ChannelBandwidth::unknown();
        me.on_sme_connect(req).expect_err("ConnectRequest with unknown channel should be rejected");
        assert!(me.get_bound_client().is_none());
    }

    #[test]
    fn rsn_ie_implies_sta_eapol_required() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        assert!(me.get_bound_client().is_none(), "MLME should not contain client, yet");
        me.on_sme_connect(fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Wpa2, ssid: Ssid::try_from("foo").unwrap()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        })
        .expect("valid ConnectRequest should be handled successfully");
        let client = me.get_bound_client().expect("client sta should have been created by now.");
        assert!(client.sta.eapol_required());
    }

    #[test]
    fn wpa1_implies_sta_eapol_required() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        assert!(me.get_bound_client().is_none(), "MLME should not contain client, yet");
        me.on_sme_connect(fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Wpa1, ssid: Ssid::try_from("foo").unwrap()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        })
        .expect("valid ConnectRequest should be handled successfully");
        let client = me.get_bound_client().expect("client sta should have been created by now.");
        assert!(client.sta.eapol_required());
    }

    #[test]
    fn no_wpa_or_rsn_ie_implies_sta_eapol_not_required() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        assert!(me.get_bound_client().is_none(), "MLME should not contain client, yet");
        me.on_sme_connect(fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Open, ssid: Ssid::try_from("foo").unwrap()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        })
        .expect("valid ConnectRequest should be handled successfully");
        let client = me.get_bound_client().expect("client sta should have been created by now.");
        assert!(!client.sta.eapol_required());
    }

    /// Consumes `TimedEvent` values from the `timer::EventStream` held by `mock_objects` and
    /// handles each `TimedEvent` value with `mlme`. This function makes the following assertions:
    ///
    ///   - The `timer::EventStream` held by `mock_objects` starts with one `StatusCheckTimeout`
    ///     pending.
    ///   - For the `beacon_count` specified, `mlme` will consume the current `StatusCheckTimeout`
    ///     and schedule the next.
    ///   - `mlme` produces a `fidl_mlme::SignalReportIndication` for each StatusCheckTimeout
    ///     consumed.
    fn handle_association_status_checks_and_signal_reports(
        mock_objects: &mut MockObjects,
        mlme: &mut ClientMlme<FakeDevice>,
        beacon_count: u32,
    ) {
        for _ in 0..beacon_count / super::state::ASSOCIATION_STATUS_TIMEOUT_BEACON_COUNT {
            let (_, timed_event) = mock_objects
                .time_stream
                .try_next()
                .unwrap()
                .expect("Should have scheduled a timed event");
            mlme.handle_timed_event(timed_event.id, timed_event.event);
            assert_eq!(mock_objects.fake_device_state.lock().wlan_queue.len(), 0);
            mock_objects
                .fake_device_state
                .lock()
                .next_mlme_msg::<fidl_internal::SignalReportIndication>()
                .expect("error reading SignalReport.indication");
        }
    }

    #[test]
    fn test_auto_deauth_uninterrupted_interval() {
        let exec = fasync::TestExecutor::new();
        let mut mock_objects = MockObjects::new(&exec);
        let mut mlme = mock_objects.make_mlme();
        mlme.make_client_station();
        let mut client = mlme.get_bound_client().expect("client should be present");

        client.move_to_associated_state();

        // Verify timer is scheduled and move the time to immediately before auto deauth is triggered.
        handle_association_status_checks_and_signal_reports(
            &mut mock_objects,
            &mut mlme,
            DEFAULT_AUTO_DEAUTH_TIMEOUT_BEACON_COUNT,
        );

        // One more timeout to trigger the auto deauth
        let (_, timed_event) = mock_objects
            .time_stream
            .try_next()
            .unwrap()
            .expect("Should have scheduled a timed event");

        // Verify that triggering event at deadline causes deauth
        mlme.handle_timed_event(timed_event.id, timed_event.event);
        mock_objects
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_internal::SignalReportIndication>()
            .expect("error reading SignalReport.indication");
        assert_eq!(mock_objects.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&mock_objects.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header:
            0b1100_00_00, 0b00000000, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // Sequence Control
            3, 0, // reason code
        ][..]);
        let deauth_ind = mock_objects
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DeauthenticateIndication>()
            .expect("error reading DEAUTHENTICATE.indication");
        assert_eq!(
            deauth_ind,
            fidl_mlme::DeauthenticateIndication {
                peer_sta_address: BSSID.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                locally_initiated: true,
            }
        );
    }

    #[test]
    fn test_auto_deauth_received_beacon() {
        let exec = fasync::TestExecutor::new();
        let mut mock_objects = MockObjects::new(&exec);
        let mut mlme = mock_objects.make_mlme();
        mlme.make_client_station();
        let mut client = mlme.get_bound_client().expect("client should be present");

        client.move_to_associated_state();

        // Move the countdown to just about to cause auto deauth.
        handle_association_status_checks_and_signal_reports(
            &mut mock_objects,
            &mut mlme,
            DEFAULT_AUTO_DEAUTH_TIMEOUT_BEACON_COUNT,
        );

        // Receive beacon midway, so lost bss countdown is reset.
        // If this beacon is not received, the next timeout will trigger auto deauth.
        mlme.on_mac_frame_rx(
            BEACON_FRAME,
            banjo_wlan_softmac::WlanRxInfo {
                rx_flags: banjo_fuchsia_wlan_softmac::WlanRxInfoFlags(0),
                valid_fields: banjo_fuchsia_wlan_softmac::WlanRxInfoValid(0),
                phy: banjo_common::WlanPhyType::DSSS,
                data_rate: 0,
                channel: ddk_converter::ddk_channel_from_fidl(
                    mlme.channel_state.get_main_channel().unwrap(),
                )
                .unwrap(),
                mcs: 0,
                rssi_dbm: 0,
                snr_dbh: 0,
            },
        );

        // Verify auto deauth is not triggered for the entire duration.
        handle_association_status_checks_and_signal_reports(
            &mut mock_objects,
            &mut mlme,
            DEFAULT_AUTO_DEAUTH_TIMEOUT_BEACON_COUNT,
        );

        // Verify more timer is scheduled
        let (_, timed_event2) = mock_objects
            .time_stream
            .try_next()
            .unwrap()
            .expect("Should have scheduled a timed event");

        // Verify that triggering event at new deadline causes deauth
        mlme.handle_timed_event(timed_event2.id, timed_event2.event);
        mock_objects
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_internal::SignalReportIndication>()
            .expect("error reading SignalReport.indication");
        assert_eq!(mock_objects.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&mock_objects.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header:
            0b1100_00_00, 0b00000000, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // Sequence Control
            3, 0, // reason code
        ][..]);
        let deauth_ind = mock_objects
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DeauthenticateIndication>()
            .expect("error reading DEAUTHENTICATE.indication");
        assert_eq!(
            deauth_ind,
            fidl_mlme::DeauthenticateIndication {
                peer_sta_address: BSSID.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                locally_initiated: true,
            }
        );
    }

    #[test]
    fn client_send_open_auth_frame() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.send_open_auth_frame().expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header:
            0b1011_00_00, 0b00000000, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // Sequence Control
            // Auth header:
            0, 0, // auth algorithm
            1, 0, // auth txn seq num
            0, 0, // status code
        ][..]);
    }

    #[test]
    fn client_send_assoc_req_frame() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let connect_req = ParsedConnectRequest {
            selected_bss: fake_bss_description!(Wpa2,
                ssid: Ssid::try_from([11, 22, 33, 44]).unwrap(),
                bssid: BSSID.to_array(),
            ),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: RSNE.to_vec(),
        };
        let client_capabilities = ClientCapabilities(StaCapabilities {
            capability_info: CapabilityInfo(0x1234),
            rates: vec![8u8, 7, 6, 5, 4, 3, 2, 1, 0].into_iter().map(ie::SupportedRate).collect(),
            ht_cap: ie::parse_ht_capabilities(&(0..26).collect::<Vec<u8>>()[..]).map(|h| *h).ok(),
            vht_cap: ie::parse_vht_capabilities(&(100..112).collect::<Vec<u8>>()[..])
                .map(|v| *v)
                .ok(),
        });
        me.sta.replace(Client::new(connect_req, *IFACE_MAC, client_capabilities));
        let mut client = me.get_bound_client().expect("client should be present");
        client.send_assoc_req_frame().expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &m.fake_device_state.lock().wlan_queue[0].0[..],
            &[
                // Mgmt header:
                0, 0, // FC
                0, 0, // Duration
                6, 6, 6, 6, 6, 6, // addr1
                7, 7, 7, 7, 7, 7, // addr2
                6, 6, 6, 6, 6, 6, // addr3
                0x10, 0, // Sequence Control
                // Association Request header:
                0x34, 0x12, // capability info
                0, 0, // listen interval
                // IEs
                0, 4, // SSID id and length
                11, 22, 33, 44, // SSID
                1, 8, // supp rates id and length
                8, 7, 6, 5, 4, 3, 2, 1, // supp rates
                50, 1, // ext supp rates and length
                0, // ext supp rates
                0x30, 0x14, // RSNE ID and len
                1, 0, // RSNE version
                0x00, 0x0f, 0xac, 0x04, // RSNE group data cipher suite
                0x01, 0x00, // RSNE pairwise cipher suite count
                0x00, 0x0f, 0xac, 0x04, // RSNE pairwise cipher suite list
                0x01, 0x00, // RSNE akm suite count
                0x00, 0x0f, 0xac, 0x02, // RSNE akm suite list
                0xa8, 0x04, // RSNE rsn capabilities
                45, 26, // HT Cap id and length
                0, 1, 2, 3, 4, 5, 6, 7, // HT Cap \
                8, 9, 10, 11, 12, 13, 14, 15, // HT Cap \
                16, 17, 18, 19, 20, 21, 22, 23, // HT Cap \
                24, 25, // HT Cap (26 bytes)
                191, 12, // VHT Cap id and length
                100, 101, 102, 103, 104, 105, 106, 107, // VHT Cap \
                108, 109, 110, 111, // VHT Cap (12 bytes)
            ][..]
        );
    }

    #[test]
    fn client_send_keep_alive_resp_frame() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.send_keep_alive_resp_frame().expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Data header:
            0b0100_10_00, 0b0000000_1, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // Sequence Control
        ][..]);
    }

    #[test]
    fn client_send_data_frame() {
        let payload = vec![5; 8];
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client
            .send_data_frame(*IFACE_MAC, [4; 6].into(), false, false, 0x1234, &payload[..])
            .expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Data header:
            0b0000_10_00, 0b0000000_1, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            4, 4, 4, 4, 4, 4, // addr3
            0x10, 0, // Sequence Control
            // LLC header:
            0xAA, 0xAA, 0x03, // DSAP, SSAP, Control
            0, 0, 0, // OUI
            0x12, 0x34, // Protocol ID
            // Payload
            5, 5, 5, 5, 5, 5, 5, 5,
        ][..]);
    }

    #[test]
    fn client_send_data_frame_ipv4_qos() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let mut client = make_client_station();
        client
            .bind(&mut me.ctx, &mut me.scanner, &mut me.channel_state)
            .send_data_frame(
                *IFACE_MAC,
                [4; 6].into(),
                false,
                true,
                0x0800,              // IPv4
                &[1, 0xB0, 3, 4, 5], // DSCP = 0b101100 (i.e. VOICE-ADMIT)
            )
            .expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Data header:
            0b1000_10_00, 0b0000000_1, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            4, 4, 4, 4, 4, 4, // addr3
            0x10, 0, // Sequence Control
            0x06, 0, // QoS Control - TID = 6
            // LLC header:
            0xAA, 0xAA, 0x03, // DSAP, SSAP, Control
            0, 0, 0, // OUI
            0x08, 0x00, // Protocol ID
            // Payload
            1, 0xB0, 3, 4, 5,
        ][..]);
    }

    #[test]
    fn client_send_data_frame_ipv6_qos() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let mut client = make_client_station();
        client
            .bind(&mut me.ctx, &mut me.scanner, &mut me.channel_state)
            .send_data_frame(
                *IFACE_MAC,
                [4; 6].into(),
                false,
                true,
                0x86DD,                         // IPv6
                &[0b0101, 0b10000000, 3, 4, 5], // DSCP = 0b010110 (i.e. AF23)
            )
            .expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Data header:
            0b1000_10_00, 0b0000000_1, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            4, 4, 4, 4, 4, 4, // addr3
            0x10, 0, // Sequence Control
            0x03, 0, // QoS Control - TID = 3
            // LLC header:
            0xAA, 0xAA, 0x03, // DSAP, SSAP, Control
            0, 0, 0, // OUI
            0x86, 0xDD, // Protocol ID
            // Payload
            0b0101, 0b10000000, 3, 4, 5,
        ][..]);
    }

    #[test]
    fn client_send_data_frame_from_ds() {
        let payload = vec![5; 8];
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client
            .send_data_frame([3; 6].into(), [4; 6].into(), false, false, 0x1234, &payload[..])
            .expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Data header:
            0b0000_10_00, 0b000000_11, // FC (ToDS=1, FromDS=1)
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2 = IFACE_MAC
            4, 4, 4, 4, 4, 4, // addr3
            0x10, 0, // Sequence Control
            3, 3, 3, 3, 3, 3, // addr4
            // LLC header:
            0xAA, 0xAA, 0x03, // DSAP, SSAP, Control
            0, 0, 0, // OUI
            0x12, 0x34, // Protocol ID
            // Payload
            5, 5, 5, 5, 5, 5, 5, 5,
        ][..]);
    }

    #[test]
    fn client_send_deauthentication_notification() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");

        client
            .send_deauth_frame(fidl_ieee80211::ReasonCode::ApInitiated.into())
            .expect("error delivering WLAN frame");
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header:
            0b1100_00_00, 0b00000000, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // Sequence Control
            47, 0, // reason code
        ][..]);
    }

    fn mock_rx_info<'a>(client: &BoundClient<'a, FakeDevice>) -> banjo_wlan_softmac::WlanRxInfo {
        let channel = client.channel_state.get_main_channel().unwrap();
        MockWlanRxInfo::with_channel(channel).into()
    }

    #[test]
    fn respond_to_keep_alive_request() {
        #[rustfmt::skip]
        let data_frame = vec![
            // Data header:
            0b0100_10_00, 0b000000_1_0, // FC
            0, 0, // Duration
            7, 7, 7, 7, 7, 7, // addr1
            6, 6, 6, 6, 6, 6, // addr2
            42, 42, 42, 42, 42, 42, // addr3
            0x10, 0, // Sequence Control
        ];
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();

        client.on_mac_frame(&data_frame[..], mock_rx_info(&client));

        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Data header:
            0b0100_10_00, 0b0000000_1, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // Sequence Control
        ][..]);
    }

    #[test]
    fn data_frame_to_ethernet_single_llc() {
        let mut data_frame = make_data_frame_single_llc(None, None);
        data_frame[1] = 0b00000010; // from_ds = 1, to_ds = 0 when AP sends to client (us)
        data_frame[4..10].copy_from_slice(IFACE_MAC.as_array()); // addr1 - receiver - client (us)
        data_frame[10..16].copy_from_slice(BSSID.as_array()); // addr2 - bssid

        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();

        client.on_mac_frame(&data_frame[..], mock_rx_info(&client));

        assert_eq!(m.fake_device_state.lock().eth_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(m.fake_device_state.lock().eth_queue[0], [
            7, 7, 7, 7, 7, 7, // dst_addr
            5, 5, 5, 5, 5, 5, // src_addr
            9, 10, // ether_type
            11, 11, 11, // payload
        ]);
    }

    #[test]
    fn data_frame_to_ethernet_amsdu() {
        let mut data_frame = make_data_frame_amsdu();
        data_frame[1] = 0b00000010; // from_ds = 1, to_ds = 0 when AP sends to client (us)
        data_frame[4..10].copy_from_slice(IFACE_MAC.as_array()); // addr1 - receiver - client (us)
        data_frame[10..16].copy_from_slice(BSSID.as_array()); // addr2 - bssid

        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();

        client.on_mac_frame(&data_frame[..], mock_rx_info(&client));

        let queue = &m.fake_device_state.lock().eth_queue;
        assert_eq!(queue.len(), 2);
        #[rustfmt::skip]
        let mut expected_first_eth_frame = vec![
            0x78, 0x8a, 0x20, 0x0d, 0x67, 0x03, // dst_addr
            0xb4, 0xf7, 0xa1, 0xbe, 0xb9, 0xab, // src_addr
            0x08, 0x00, // ether_type
        ];
        expected_first_eth_frame.extend_from_slice(MSDU_1_PAYLOAD);
        assert_eq!(queue[0], &expected_first_eth_frame[..]);
        #[rustfmt::skip]
        let mut expected_second_eth_frame = vec![
            0x78, 0x8a, 0x20, 0x0d, 0x67, 0x04, // dst_addr
            0xb4, 0xf7, 0xa1, 0xbe, 0xb9, 0xac, // src_addr
            0x08, 0x01, // ether_type
        ];
        expected_second_eth_frame.extend_from_slice(MSDU_2_PAYLOAD);
        assert_eq!(queue[1], &expected_second_eth_frame[..]);
    }

    #[test]
    fn data_frame_to_ethernet_amsdu_padding_too_short() {
        let mut data_frame = make_data_frame_amsdu_padding_too_short();
        data_frame[1] = 0b00000010; // from_ds = 1, to_ds = 0 when AP sends to client (us)
        data_frame[4..10].copy_from_slice(IFACE_MAC.as_array()); // addr1 - receiver - client (us)
        data_frame[10..16].copy_from_slice(BSSID.as_array()); // addr2 - bssid

        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();

        client.on_mac_frame(&data_frame[..], mock_rx_info(&client));

        let queue = &m.fake_device_state.lock().eth_queue;
        assert_eq!(queue.len(), 1);
        #[rustfmt::skip]
            let mut expected_first_eth_frame = vec![
            0x78, 0x8a, 0x20, 0x0d, 0x67, 0x03, // dst_addr
            0xb4, 0xf7, 0xa1, 0xbe, 0xb9, 0xab, // src_addr
            0x08, 0x00, // ether_type
        ];
        expected_first_eth_frame.extend_from_slice(MSDU_1_PAYLOAD);
        assert_eq!(queue[0], &expected_first_eth_frame[..]);
    }

    #[test]
    fn data_frame_controlled_port_closed() {
        let mut data_frame = make_data_frame_single_llc(None, None);
        data_frame[1] = 0b00000010; // from_ds = 1, to_ds = 0 when AP sends to client (us)
        data_frame[4..10].copy_from_slice(IFACE_MAC.as_array()); // addr1 - receiver - client (us)
        data_frame[10..16].copy_from_slice(BSSID.as_array()); // addr2 - bssid

        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station_protected();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();
        client.close_controlled_port();

        client.on_mac_frame(&data_frame[..], mock_rx_info(&client));

        // Verify frame was not sent to netstack.
        assert_eq!(m.fake_device_state.lock().eth_queue.len(), 0);
    }

    #[test]
    fn eapol_frame_controlled_port_closed() {
        let (src_addr, dst_addr, mut eapol_frame) = make_eapol_frame(*IFACE_MAC);
        eapol_frame[1] = 0b00000010; // from_ds = 1, to_ds = 0 when AP sends to client (us)
        eapol_frame[4..10].copy_from_slice(IFACE_MAC.as_array()); // addr1 - receiver - client (us)
        eapol_frame[10..16].copy_from_slice(BSSID.as_array()); // addr2 - bssid

        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station_protected();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();
        client.close_controlled_port();

        client.on_mac_frame(&eapol_frame[..], mock_rx_info(&client));

        // Verify EAPoL frame was not sent to netstack.
        assert_eq!(m.fake_device_state.lock().eth_queue.len(), 0);

        // Verify EAPoL frame was sent to SME.
        let eapol_ind = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::EapolIndication>()
            .expect("error reading EAPOL.indication");
        assert_eq!(
            eapol_ind,
            fidl_mlme::EapolIndication {
                src_addr: src_addr.to_array(),
                dst_addr: dst_addr.to_array(),
                data: EAPOL_PDU.to_vec()
            }
        );
    }

    #[test]
    fn eapol_frame_is_controlled_port_open() {
        let (src_addr, dst_addr, mut eapol_frame) = make_eapol_frame(*IFACE_MAC);
        eapol_frame[1] = 0b00000010; // from_ds = 1, to_ds = 0 when AP sends to client (us)
        eapol_frame[4..10].copy_from_slice(IFACE_MAC.as_array()); // addr1 - receiver - client (us)
        eapol_frame[10..16].copy_from_slice(BSSID.as_array()); // addr2 - bssid

        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();

        client.on_mac_frame(&eapol_frame[..], mock_rx_info(&client));

        // Verify EAPoL frame was not sent to netstack.
        assert_eq!(m.fake_device_state.lock().eth_queue.len(), 0);

        // Verify EAPoL frame was sent to SME.
        let eapol_ind = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::EapolIndication>()
            .expect("error reading EAPOL.indication");
        assert_eq!(
            eapol_ind,
            fidl_mlme::EapolIndication {
                src_addr: src_addr.to_array(),
                dst_addr: dst_addr.to_array(),
                data: EAPOL_PDU.to_vec()
            }
        );
    }

    #[test]
    fn send_eapol_ind_success() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client
            .send_eapol_indication([1; 6].into(), [2; 6].into(), &[5; 200])
            .expect("expected EAPOL.indication to be sent");
        let eapol_ind = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::EapolIndication>()
            .expect("error reading EAPOL.indication");
        assert_eq!(
            eapol_ind,
            fidl_mlme::EapolIndication {
                src_addr: [1; 6].into(),
                dst_addr: [2; 6].into(),
                data: vec![5; 200]
            }
        );
    }

    #[test]
    fn send_eapol_frame_success() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.send_eapol_frame(*IFACE_MAC, (*BSSID).into(), false, &[5; 8]);

        // Verify EAPOL.confirm message was sent to SME.
        let eapol_confirm = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::EapolConfirm>()
            .expect("error reading EAPOL.confirm");
        assert_eq!(
            eapol_confirm,
            fidl_mlme::EapolConfirm {
                result_code: fidl_mlme::EapolResultCode::Success,
                dst_addr: BSSID.to_array(),
            }
        );

        // Verify EAPoL frame was sent over the air.
        #[rustfmt::skip]
        assert_eq!(&m.fake_device_state.lock().wlan_queue[0].0[..], &[
            // Data header:
            0b0000_10_00, 0b0000000_1, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // Sequence Control
            // LLC header:
            0xaa, 0xaa, 0x03, // dsap ssap ctrl
            0x00, 0x00, 0x00, // oui
            0x88, 0x8E, // protocol id (EAPOL)
            // EAPoL PDU:
            5, 5, 5, 5, 5, 5, 5, 5,
        ][..]);
    }

    #[test]
    fn send_eapol_frame_failure() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        m.fake_device_state.lock().config.send_wlan_frame_fails = true;
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.send_eapol_frame([1; 6].into(), [2; 6].into(), false, &[5; 200]);

        // Verify EAPOL.confirm message was sent to SME.
        let eapol_confirm = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::EapolConfirm>()
            .expect("error reading EAPOL.confirm");
        assert_eq!(
            eapol_confirm,
            fidl_mlme::EapolConfirm {
                result_code: fidl_mlme::EapolResultCode::TransmissionFailure,
                dst_addr: [2; 6].into(),
            }
        );

        // Verify EAPoL frame was not sent over the air.
        assert!(m.fake_device_state.lock().wlan_queue.is_empty());
    }

    #[test]
    fn send_keys() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station_protected();
        let mut client = me.get_bound_client().expect("client should be present");
        client.move_to_associated_state();

        assert!(m.fake_device_state.lock().keys.is_empty());
        client.handle_mlme_req(crate::test_utils::fake_set_keys_req((*BSSID).into()));
        assert_eq!(m.fake_device_state.lock().keys.len(), 1);

        let sent_key = crate::test_utils::fake_key((*BSSID).into());
        let received_key = &m.fake_device_state.lock().keys[0];
        assert_eq!(received_key.key, Some(sent_key.key));
        assert_eq!(received_key.key_idx, Some(sent_key.key_id as u8));
        assert_eq!(received_key.key_type, Some(fidl_common::WlanKeyType::Pairwise));
    }

    #[test]
    fn send_ps_poll_frame() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.send_ps_poll_frame(0xABCD).expect("failed sending PS POLL frame");
    }

    #[test]
    fn send_power_state_doze_frame_success() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let mut client = make_client_station();
        client
            .send_power_state_frame(&mut me.ctx, PowerState::DOZE)
            .expect("failed sending doze frame");
        client
            .send_power_state_frame(&mut me.ctx, PowerState::AWAKE)
            .expect("failed sending awake frame");
    }

    #[test]
    fn send_addba_req_frame() {
        let exec = fasync::TestExecutor::new();
        let mut mock = MockObjects::new(&exec);
        let mut mlme = mock.make_mlme();
        mlme.make_client_station();
        let mut client = mlme.get_bound_client().expect("client should be present");

        let mut body = [0u8; 16];
        let mut writer = BufferWriter::new(&mut body[..]);
        block_ack::write_addba_req_body(&mut writer, 1)
            .and_then(|_| client.send_block_ack_frame(ADDBA_REQ_FRAME_LEN, writer.into_written()))
            .expect("failed sending addba frame");
        assert_eq!(
            &mock.fake_device_state.lock().wlan_queue[0].0[..],
            &[
                // Mgmt header 1101 for action frame
                0b11010000, 0b00000000, // frame control
                0, 0, // duration
                6, 6, 6, 6, 6, 6, // addr1
                7, 7, 7, 7, 7, 7, // addr2
                6, 6, 6, 6, 6, 6, // addr3
                0x10, 0, // sequence control
                // Action frame header (Also part of ADDBA request frame)
                0x03, // Action Category: block ack (0x03)
                0x00, // block ack action: ADDBA request (0x00)
                1,    // block ack dialog token
                0b00000011, 0b00010000, // block ack parameters (u16)
                0, 0, // block ack timeout (u16) (0: disabled)
                0b00010000, 0, // block ack starting sequence number: fragment 0, sequence 1
            ][..]
        );
    }

    #[test]
    fn send_addba_resp_frame() {
        let exec = fasync::TestExecutor::new();
        let mut mock = MockObjects::new(&exec);
        let mut mlme = mock.make_mlme();
        mlme.make_client_station();
        let mut client = mlme.get_bound_client().expect("client should be present");

        let mut body = [0u8; 16];
        let mut writer = BufferWriter::new(&mut body[..]);
        block_ack::write_addba_resp_body(&mut writer, 1)
            .and_then(|_| client.send_block_ack_frame(ADDBA_RESP_FRAME_LEN, writer.into_written()))
            .expect("failed sending addba frame");
        assert_eq!(
            &mock.fake_device_state.lock().wlan_queue[0].0[..],
            &[
                // Mgmt header 1101 for action frame
                0b11010000, 0b00000000, // frame control
                0, 0, // duration
                6, 6, 6, 6, 6, 6, // addr1
                7, 7, 7, 7, 7, 7, // addr2
                6, 6, 6, 6, 6, 6, // addr3
                0x10, 0, // sequence control
                // Action frame header (Also part of ADDBA response frame)
                0x03, // Action Category: block ack (0x03)
                0x01, // block ack action: ADDBA response (0x01)
                1,    // block ack dialog token
                0, 0, // status
                0b00000011, 0b00010000, // block ack parameters (u16)
                0, 0, // block ack timeout (u16) (0: disabled)
            ][..]
        );
    }

    #[test]
    fn client_send_successful_connect_conf() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");

        client.send_connect_conf_success(42, &[0, 5, 3, 4, 5, 6, 7]);
        let connect_conf = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ConnectConfirm>()
            .expect("error reading Connect.confirm");
        assert_eq!(
            connect_conf,
            fidl_mlme::ConnectConfirm {
                peer_sta_address: BSSID.to_array(),
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 42,
                association_ies: vec![0, 5, 3, 4, 5, 6, 7],
            }
        );
    }

    #[test]
    fn client_send_failed_connect_conf() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();
        let mut client = me.get_bound_client().expect("client should be present");
        client.send_connect_conf_failure(fidl_ieee80211::StatusCode::DeniedNoMoreStas);
        let connect_conf = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ConnectConfirm>()
            .expect("error reading Connect.confirm");
        assert_eq!(
            connect_conf,
            fidl_mlme::ConnectConfirm {
                peer_sta_address: BSSID.to_array(),
                result_code: fidl_ieee80211::StatusCode::DeniedNoMoreStas,
                association_id: 0,
                association_ies: vec![],
            }
        );
    }

    #[test]
    fn client_send_scan_end_on_mlme_scan_busy() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();

        // Issue a second scan before the first finishes
        me.on_sme_scan(scan_req());
        me.on_sme_scan(fidl_mlme::ScanRequest { txn_id: 1338, ..scan_req() });

        let scan_end = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1338, code: fidl_mlme::ScanResultCode::NotSupported }
        );
    }

    #[test]
    fn client_send_scan_end_on_scan_busy() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        me.make_client_station();

        // Issue a second scan before the first finishes
        me.on_sme_scan(scan_req());
        me.on_sme_scan(fidl_mlme::ScanRequest { txn_id: 1338, ..scan_req() });

        let scan_end = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1338, code: fidl_mlme::ScanResultCode::NotSupported }
        );
    }

    #[test]
    fn client_send_scan_end_on_mlme_scan_invalid_args() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        me.make_client_station();
        me.on_sme_scan(fidl_mlme::ScanRequest {
            txn_id: 1337,
            scan_type: fidl_mlme::ScanTypes::Passive,
            channel_list: vec![], // empty channel list
            ssid_list: vec![Ssid::try_from("ssid").unwrap().into()],
            probe_delay: 0,
            min_channel_time: 100,
            max_channel_time: 300,
        });
        let scan_end = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn client_send_scan_end_on_scan_invalid_args() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        me.make_client_station();
        me.on_sme_scan(fidl_mlme::ScanRequest {
            txn_id: 1337,
            scan_type: fidl_mlme::ScanTypes::Passive,
            channel_list: vec![6],
            ssid_list: vec![Ssid::try_from("ssid").unwrap().into()],
            probe_delay: 0,
            min_channel_time: 300, // min > max
            max_channel_time: 100,
        });
        let scan_end = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn client_send_scan_end_on_passive_scan_fails() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        m.fake_device_state.lock().config.start_passive_scan_fails = true;
        let mut me = m.make_mlme();

        me.make_client_station();
        me.on_sme_scan(scan_req());
        let scan_end = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ScanEnd>()
            .expect("error reading MLME ScanEnd");
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1337, code: fidl_mlme::ScanResultCode::NotSupported }
        );
    }

    #[test]
    fn mlme_respond_to_query_device_info() {
        let mut exec = fasync::TestExecutor::new();
        let mut mock_objects = MockObjects::new(&exec);
        let mut mlme = mock_objects.make_mlme();

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            mlme.handle_mlme_req(wlan_sme::MlmeRequest::QueryDeviceInfo(responder)),
            Ok(())
        );
        let info = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(
            info,
            fidl_mlme::DeviceInfo {
                sta_addr: IFACE_MAC.to_array(),
                role: fidl_common::WlanMacRole::Client,
                bands: test_utils::fake_mlme_band_caps(),
                softmac_hardware_capability: 0,
                qos_capable: false,
            }
        );
    }

    #[test]
    fn mlme_respond_to_query_discovery_support() {
        let mut exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            me.handle_mlme_req(wlan_sme::MlmeRequest::QueryDiscoverySupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.scan_offload.supported, true);
        assert_eq!(resp.probe_response_offload.supported, false);
    }

    #[test]
    fn mlme_respond_to_query_mac_sublayer_support() {
        let mut exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            me.handle_mlme_req(wlan_sme::MlmeRequest::QueryMacSublayerSupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.rate_selection_offload.supported, false);
        assert_eq!(resp.data_plane.data_plane_type, fidl_common::DataPlaneType::EthernetDevice);
        assert_eq!(resp.device.is_synthetic, true);
        assert_eq!(
            resp.device.mac_implementation_type,
            fidl_common::MacImplementationType::Softmac
        );
        assert_eq!(resp.device.tx_status_report_supported, true);
    }

    #[test]
    fn mlme_respond_to_query_security_support() {
        let mut exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            me.handle_mlme_req(wlan_sme::MlmeRequest::QuerySecuritySupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.mfp.supported, false);
        assert_eq!(resp.sae.driver_handler_supported, false);
        assert_eq!(resp.sae.sme_handler_supported, false);
    }

    #[test]
    fn mlme_respond_to_query_spectrum_management_support() {
        let mut exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            me.handle_mlme_req(wlan_sme::MlmeRequest::QuerySpectrumManagementSupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.dfs.supported, true);
    }

    #[test]
    fn mlme_connect_unprotected_happy_path() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let channel = Channel::new(6, Cbw::Cbw40);
        let connect_req = fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("ssid").unwrap().into(),
                bssid: BSSID.to_array(),
                channel: channel.clone(),
            ),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        };
        let result = me.handle_mlme_req(wlan_sme::MlmeRequest::Connect(connect_req));
        assert_variant!(result, Ok(()));

        // Verify an event was queued up in the timer.
        assert_variant!(drain_timeouts(&mut m.time_stream).get(&TimedEventClass::Connecting), Some(ids) => {
            assert_eq!(ids.len(), 1);
        });

        // Verify authentication frame was sent to AP.
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        let (frame, _txflags) = m.fake_device_state.lock().wlan_queue.remove(0);
        #[rustfmt::skip]
        let expected = vec![
            // Mgmt Header:
            0b1011_00_00, 0b00000000, // Frame Control
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // Addr1
            7, 7, 7, 7, 7, 7, // Addr2
            6, 6, 6, 6, 6, 6, // Addr3
            0x10, 0, // Sequence Control
            // Auth Header:
            0, 0, // Algorithm Number (Open)
            1, 0, // Txn Sequence Number
            0, 0, // Status Code
        ];
        assert_eq!(&frame[..], &expected[..]);

        // Mock auth frame response from the AP
        #[rustfmt::skip]
        let auth_resp_success = vec![
            // Mgmt Header:
            0b1011_00_00, 0b00000000, // Frame Control
            0, 0, // Duration
            7, 7, 7, 7, 7, 7, // Addr1
            7, 7, 7, 7, 7, 7, // Addr2
            6, 6, 6, 6, 6, 6, // Addr3
            0x10, 0, // Sequence Control
            // Auth Header:
            0, 0, // Algorithm Number (Open)
            2, 0, // Txn Sequence Number
            0, 0, // Status Code
        ];
        me.on_mac_frame_rx(
            &auth_resp_success[..],
            MockWlanRxInfo::with_channel(channel.into()).into(),
        );

        // Verify association request frame was went to AP
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        let (frame, _txflags) = m.fake_device_state.lock().wlan_queue.remove(0);
        #[rustfmt::skip]
        let expected = vec![
            // Mgmt header:
            0, 0, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x20, 0, // Sequence Control
            // Association Request header:
            0x01, 0x00, // capability info
            0, 0, // listen interval
            // IEs
            0, 4, // SSID id and length
            0x73, 0x73, 0x69, 0x64, // SSID
            1, 8, // supp rates id and length
            2, 4, 11, 22, 12, 18, 24, 36, // supp rates
            50, 4, // ext supp rates and length
            48, 72, 96, 108, // ext supp rates
            45, 26, // HT Cap id and length
            0x63, 0, 0x17, 0xff, 0, 0, 0, // HT Cap \
            0, 0, 0, 0, 0, 0, 0, 0, 1, // HT Cap \
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // HT Cap
        ];
        assert_eq!(&frame[..], &expected[..]);

        // Mock assoc resp frame from the AP
        #[rustfmt::skip]
        let assoc_resp_success = vec![
            // Mgmt Header:
            0b0001_00_00, 0b00000000, // Frame Control
            0, 0, // Duration
            7, 7, 7, 7, 7, 7, // Addr1 == IFACE_MAC
            7, 7, 7, 7, 7, 7, // Addr2
            6, 6, 6, 6, 6, 6, // Addr3
            0x20, 0, // Sequence Control
            // Assoc Resp Header:
            0, 0, // Capabilities
            0, 0, // Status Code
            42, 0, // AID
            // IEs
            // Basic Rates
            0x01, 0x08, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24,
            // HT Capabilities
            0x2d, 0x1a, 0xef, 0x09, // HT capabilities info
            0x17, // A-MPDU parameters
            0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            // VHT Capabilities
            0xbf, 0x0c, 0x91, 0x59, 0x82, 0x0f, // VHT capabilities info
            0xea, 0xff, 0x00, 0x00, 0xea, 0xff, 0x00, 0x00, // VHT supported MCS set
        ];
        me.on_mac_frame_rx(
            &assoc_resp_success[..],
            MockWlanRxInfo::with_channel(channel.into()).into(),
        );

        // Verify a successful connect conf is sent
        let msg = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ConnectConfirm>()
            .expect("expect ConnectConf");
        assert_eq!(
            msg,
            fidl_mlme::ConnectConfirm {
                peer_sta_address: BSSID.to_array(),
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 42,
                association_ies: vec![
                    // IEs
                    // Basic Rates
                    0x01, 0x08, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24,
                    // HT Capabilities
                    0x2d, 0x1a, 0xef, 0x09, // HT capabilities info
                    0x17, // A-MPDU parameters
                    0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, // VHT Capabilities
                    0xbf, 0x0c, 0x91, 0x59, 0x82, 0x0f, // VHT capabilities info
                    0xea, 0xff, 0x00, 0x00, 0xea, 0xff, 0x00, 0x00, // VHT supported MCS set
                ],
            }
        );

        // Verify eth link is up
        assert_eq!(m.fake_device_state.lock().link_status, LinkStatus::UP);
    }

    #[test]
    fn mlme_connect_protected_happy_path() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let channel = Channel::new(6, Cbw::Cbw40);
        let connect_req = fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Wpa2,
                ssid: Ssid::try_from("ssid").unwrap().into(),
                bssid: BSSID.to_array(),
                channel: channel.clone(),
            ),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![
                48, 18, // RSNE header
                1, 0, // Version
                0x00, 0x0F, 0xAC, 4, // Group Cipher: CCMP-128
                1, 0, 0x00, 0x0F, 0xAC, 4, // 1 Pairwise Cipher: CCMP-128
                1, 0, 0x00, 0x0F, 0xAC, 2, // 1 AKM: PSK
            ],
        };
        let result = me.handle_mlme_req(wlan_sme::MlmeRequest::Connect(connect_req));
        assert_variant!(result, Ok(()));

        // Verify an event was queued up in the timer.
        assert_variant!(drain_timeouts(&mut m.time_stream).get(&TimedEventClass::Connecting), Some(ids) => {
            assert_eq!(ids.len(), 1);
        });

        // Verify authentication frame was sent to AP.
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        let (frame, _txflags) = m.fake_device_state.lock().wlan_queue.remove(0);
        #[rustfmt::skip]
        let expected = vec![
            // Mgmt Header:
            0b1011_00_00, 0b00000000, // Frame Control
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // Addr1
            7, 7, 7, 7, 7, 7, // Addr2
            6, 6, 6, 6, 6, 6, // Addr3
            0x10, 0, // Sequence Control
            // Auth Header:
            0, 0, // Algorithm Number (Open)
            1, 0, // Txn Sequence Number
            0, 0, // Status Code
        ];
        assert_eq!(&frame[..], &expected[..]);

        // Mock auth frame response from the AP
        #[rustfmt::skip]
        let auth_resp_success = vec![
            // Mgmt Header:
            0b1011_00_00, 0b00000000, // Frame Control
            0, 0, // Duration
            7, 7, 7, 7, 7, 7, // Addr1
            7, 7, 7, 7, 7, 7, // Addr2
            6, 6, 6, 6, 6, 6, // Addr3
            0x10, 0, // Sequence Control
            // Auth Header:
            0, 0, // Algorithm Number (Open)
            2, 0, // Txn Sequence Number
            0, 0, // Status Code
        ];
        me.on_mac_frame_rx(
            &auth_resp_success[..],
            MockWlanRxInfo::with_channel(channel.into()).into(),
        );

        // Verify association request frame was went to AP
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        let (frame, _txflags) = m.fake_device_state.lock().wlan_queue.remove(0);
        #[rustfmt::skip]
        let expected = vec![
            // Mgmt header:
            0, 0, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x20, 0, // Sequence Control
            // Association Request header:
            0x01, 0x00, // capability info
            0, 0, // listen interval
            // IEs
            0, 4, // SSID id and length
            0x73, 0x73, 0x69, 0x64, // SSID
            1, 8, // supp rates id and length
            2, 4, 11, 22, 12, 18, 24, 36, // supp rates
            50, 4, // ext supp rates and length
            48, 72, 96, 108, // ext supp rates
            48, 18, // RSNE id and length
            1, 0, // RSN \
            0x00, 0x0F, 0xAC, 4, // RSN \
            1, 0, 0x00, 0x0F, 0xAC, 4, // RSN \
            1, 0, 0x00, 0x0F, 0xAC, 2, // RSN
            45, 26, // HT Cap id and length
            0x63, 0, 0x17, 0xff, 0, 0, 0, // HT Cap \
            0, 0, 0, 0, 0, 0, 0, 0, 1, // HT Cap \
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // HT Cap
        ];
        assert_eq!(&frame[..], &expected[..]);

        // Mock assoc resp frame from the AP
        #[rustfmt::skip]
        let assoc_resp_success = vec![
            // Mgmt Header:
            0b0001_00_00, 0b00000000, // Frame Control
            0, 0, // Duration
            7, 7, 7, 7, 7, 7, // Addr1 == IFACE_MAC
            7, 7, 7, 7, 7, 7, // Addr2
            6, 6, 6, 6, 6, 6, // Addr3
            0x20, 0, // Sequence Control
            // Assoc Resp Header:
            0, 0, // Capabilities
            0, 0, // Status Code
            42, 0, // AID
            // IEs
            // Basic Rates
            0x01, 0x08, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24,
            // RSN
            0x30, 18, 1, 0, // RSN header and version
            0x00, 0x0F, 0xAC, 4, // Group Cipher: CCMP-128
            1, 0, 0x00, 0x0F, 0xAC, 4, // 1 Pairwise Cipher: CCMP-128
            1, 0, 0x00, 0x0F, 0xAC, 2, // 1 AKM: PSK
            // HT Capabilities
            0x2d, 0x1a, 0xef, 0x09, // HT capabilities info
            0x17, // A-MPDU parameters
            0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Other HT Cap fields
            // VHT Capabilities
            0xbf, 0x0c, 0x91, 0x59, 0x82, 0x0f, // VHT capabilities info
            0xea, 0xff, 0x00, 0x00, 0xea, 0xff, 0x00, 0x00, // VHT supported MCS set
        ];
        me.on_mac_frame_rx(
            &assoc_resp_success[..],
            MockWlanRxInfo::with_channel(channel.into()).into(),
        );

        // Verify a successful connect conf is sent
        let msg = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ConnectConfirm>()
            .expect("expect ConnectConf");
        assert_eq!(
            msg,
            fidl_mlme::ConnectConfirm {
                peer_sta_address: BSSID.to_array(),
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 42,
                association_ies: vec![
                    // IEs
                    // Basic Rates
                    0x01, 0x08, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24, // RSN
                    0x30, 18, 1, 0, // RSN header and version
                    0x00, 0x0F, 0xAC, 4, // Group Cipher: CCMP-128
                    1, 0, 0x00, 0x0F, 0xAC, 4, // 1 Pairwise Cipher: CCMP-128
                    1, 0, 0x00, 0x0F, 0xAC, 2, // 1 AKM: PSK
                    // HT Capabilities
                    0x2d, 0x1a, 0xef, 0x09, // HT capabilities info
                    0x17, // A-MPDU parameters
                    0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, // Other HT Cap fields
                    // VHT Capabilities
                    0xbf, 0x0c, 0x91, 0x59, 0x82, 0x0f, // VHT capabilities info
                    0xea, 0xff, 0x00, 0x00, 0xea, 0xff, 0x00, 0x00, // VHT supported MCS set
                ],
            }
        );

        // Verify that link is still down
        assert_eq!(m.fake_device_state.lock().link_status, LinkStatus::DOWN);

        // Send a request to open controlled port
        me.handle_mlme_req(wlan_sme::MlmeRequest::SetCtrlPort(
            fidl_mlme::SetControlledPortRequest {
                peer_sta_address: BSSID.to_array(),
                state: fidl_mlme::ControlledPortState::Open,
            },
        ))
        .expect("expect sending msg to succeed");

        // Verify that link is now up
        assert_eq!(m.fake_device_state.lock().link_status, LinkStatus::UP);
    }

    #[test]
    fn mlme_connect_vht() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let channel = Channel::new(36, Cbw::Cbw40);
        let connect_req = fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Open,
                ssid: Ssid::try_from("ssid").unwrap().into(),
                bssid: BSSID.to_array(),
                channel: channel.clone(),
            ),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        };
        let result = me.handle_mlme_req(wlan_sme::MlmeRequest::Connect(connect_req));
        assert_variant!(result, Ok(()));

        // Verify an event was queued up in the timer.
        assert_variant!(drain_timeouts(&mut m.time_stream).get(&TimedEventClass::Connecting), Some(ids) => {
            assert_eq!(ids.len(), 1);
        });

        // Auth frame
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        let (_frame, _txflags) = m.fake_device_state.lock().wlan_queue.remove(0);

        // Mock auth frame response from the AP
        #[rustfmt::skip]
        let auth_resp_success = vec![
            // Mgmt Header:
            0b1011_00_00, 0b00000000, // Frame Control
            0, 0, // Duration
            7, 7, 7, 7, 7, 7, // Addr1
            7, 7, 7, 7, 7, 7, // Addr2
            6, 6, 6, 6, 6, 6, // Addr3
            0x10, 0, // Sequence Control
            // Auth Header:
            0, 0, // Algorithm Number (Open)
            2, 0, // Txn Sequence Number
            0, 0, // Status Code
        ];
        me.on_mac_frame_rx(
            &auth_resp_success[..],
            MockWlanRxInfo::with_channel(channel.into()).into(),
        );

        // Verify association request frame was went to AP
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        let (frame, _txflags) = m.fake_device_state.lock().wlan_queue.remove(0);
        #[rustfmt::skip]
        let expected = vec![
            // Mgmt header:
            0, 0, // FC
            0, 0, // Duration
            6, 6, 6, 6, 6, 6, // addr1
            7, 7, 7, 7, 7, 7, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x20, 0, // Sequence Control
            // Association Request header:
            0x01, 0x00, // capability info
            0, 0, // listen interval
            // IEs
            0, 4, // SSID id and length
            0x73, 0x73, 0x69, 0x64, // SSID
            1, 6, // supp rates id and length
            2, 4, 11, 22, 48, 96, // supp rates
            45, 26, // HT Cap id and length
            0x63, 0, 0x17, 0xff, 0, 0, 0, // HT Cap \
            0, 0, 0, 0, 0, 0, 0, 0, 1, // HT Cap \
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // HT Cap
            191, 12, // VHT Cap id and length
            50, 80, 128, 15, 254, 255, 0, 0, 254, 255, 0, 0, // VHT Cap
        ];
        assert_eq!(&frame[..], &expected[..]);
    }

    #[test]
    fn mlme_connect_timeout() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();
        let connect_req = fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Open, bssid: BSSID.to_array()),
            connect_failure_timeout: 100,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![],
            wep_key: None,
            security_ie: vec![],
        };
        let result = me.handle_mlme_req(wlan_sme::MlmeRequest::Connect(connect_req));
        assert_variant!(result, Ok(()));

        // Verify an event was queued up in the timer.
        let (event, id) = assert_variant!(drain_timeouts(&mut m.time_stream).get(&TimedEventClass::Connecting), Some(events) => {
            assert_eq!(events.len(), 1);
            events[0].clone()
        });

        // Quick check that a frame was sent (this is authentication frame).
        assert_eq!(m.fake_device_state.lock().wlan_queue.len(), 1);
        let (_frame, _txflags) = m.fake_device_state.lock().wlan_queue.remove(0);

        // Send connect timeout
        me.handle_timed_event(id, event);

        // Verify a connect confirm message was sent
        let msg = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ConnectConfirm>()
            .expect("expect msg");
        assert_eq!(
            msg,
            fidl_mlme::ConnectConfirm {
                peer_sta_address: BSSID.to_array(),
                result_code: fidl_ieee80211::StatusCode::RejectedSequenceTimeout,
                association_id: 0,
                association_ies: vec![],
            },
        );
    }

    #[test]
    fn mlme_reconnect_no_sta() {
        let exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        let reconnect_req = fidl_mlme::ReconnectRequest { peer_sta_address: [1, 2, 3, 4, 5, 6] };
        let result = me.handle_mlme_req(wlan_sme::MlmeRequest::Reconnect(reconnect_req));
        assert_variant!(result, Err(Error::Status(_, zx::Status::BAD_STATE)));

        // Verify a connect confirm message was sent
        let msg = m
            .fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::ConnectConfirm>()
            .expect("expect msg");
        assert_eq!(
            msg,
            fidl_mlme::ConnectConfirm {
                peer_sta_address: [1, 2, 3, 4, 5, 6],
                result_code: fidl_ieee80211::StatusCode::DeniedNoAssociationExists,
                association_id: 0,
                association_ies: vec![],
            },
        );
    }

    #[test]
    fn mlme_respond_to_get_iface_counter_stats_with_error_status() {
        let mut exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            me.handle_mlme_req(wlan_sme::MlmeRequest::GetIfaceCounterStats(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(
            resp,
            fidl_mlme::GetIfaceCounterStatsResponse::ErrorStatus(zx::sys::ZX_ERR_NOT_SUPPORTED)
        );
    }

    #[test]
    fn mlme_respond_to_get_iface_histogram_stats_with_error_status() {
        let mut exec = fasync::TestExecutor::new();
        let mut m = MockObjects::new(&exec);
        let mut me = m.make_mlme();

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            me.handle_mlme_req(wlan_sme::MlmeRequest::GetIfaceHistogramStats(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(
            resp,
            fidl_mlme::GetIfaceHistogramStatsResponse::ErrorStatus(zx::sys::ZX_ERR_NOT_SUPPORTED)
        );
    }

    #[test]
    fn drop_mgmt_frame_wrong_bssid() {
        let frame = [
            // Mgmt header 1101 for action frame
            0b11010000, 0b00000000, // frame control
            0, 0, // duration
            7, 7, 7, 7, 7, 7, // addr1
            6, 6, 6, 6, 6, 6, // addr2
            0, 0, 0, 0, 0, 0, // addr3 (bssid should have been [6; 6])
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(false, make_client_station().should_handle_frame(&frame));
    }

    #[test]
    fn drop_mgmt_frame_wrong_dst_addr() {
        let frame = [
            // Mgmt header 1101 for action frame
            0b11010000, 0b00000000, // frame control
            0, 0, // duration
            0, 0, 0, 0, 0, 0, // addr1 (dst_addr should have been [7; 6])
            6, 6, 6, 6, 6, 6, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(false, make_client_station().should_handle_frame(&frame));
    }

    #[test]
    fn mgmt_frame_ok_broadcast() {
        let frame = [
            // Mgmt header 1101 for action frame
            0b11010000, 0b00000000, // frame control
            0, 0, // duration
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // addr1 (dst_addr is broadcast)
            6, 6, 6, 6, 6, 6, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(true, make_client_station().should_handle_frame(&frame));
    }

    #[test]
    fn mgmt_frame_ok_client_addr() {
        let frame = [
            // Mgmt header 1101 for action frame
            0b11010000, 0b00000000, // frame control
            0, 0, // duration
            7, 7, 7, 7, 7, 7, // addr1 (dst_addr should have been [7; 6])
            6, 6, 6, 6, 6, 6, // addr2
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(true, make_client_station().should_handle_frame(&frame));
    }

    #[test]
    fn drop_data_frame_wrong_bssid() {
        let frame = [
            // Data header 0100
            0b01001000,
            0b00000010, // frame control. right 2 bits of octet 2: from_ds(1), to_ds(0)
            0, 0, // duration
            7, 7, 7, 7, 7, 7, // addr1 (dst_addr)
            0, 0, 0, 0, 0, 0, // addr2 (bssid should have been [6; 6])
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(false, make_client_station().should_handle_frame(&frame));
    }

    #[test]
    fn drop_data_frame_wrong_dst_addr() {
        let frame = [
            // Data header 0100
            0b01001000,
            0b00000010, // frame control. right 2 bits of octet 2: from_ds(1), to_ds(0)
            0, 0, // duration
            0, 0, 0, 0, 0, 0, // addr1 (dst_addr should have been [7; 6])
            6, 6, 6, 6, 6, 6, // addr2 (bssid)
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(false, make_client_station().should_handle_frame(&frame));
    }

    #[test]
    fn data_frame_ok_broadcast() {
        let frame = [
            // Data header 0100
            0b01001000,
            0b00000010, // frame control. right 2 bits of octet 2: from_ds(1), to_ds(0)
            0, 0, // duration
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // addr1 (dst_addr is broadcast)
            6, 6, 6, 6, 6, 6, // addr2 (bssid)
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(true, make_client_station().should_handle_frame(&frame));
    }

    #[test]
    fn data_frame_ok_client_addr() {
        let frame = [
            // Data header 0100
            0b01001000,
            0b00000010, // frame control. right 2 bits of octet 2: from_ds(1), to_ds(0)
            0, 0, // duration
            7, 7, 7, 7, 7, 7, // addr1 (dst_addr)
            6, 6, 6, 6, 6, 6, // addr2 (bssid)
            6, 6, 6, 6, 6, 6, // addr3
            0x10, 0, // sequence control
        ];
        let frame = mac::MacFrame::parse(&frame[..], false).unwrap();
        assert_eq!(true, make_client_station().should_handle_frame(&frame));
    }
}
