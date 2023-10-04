// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{bail, Context, Error},
    fidl_fuchsia_wlan_sme as fidl_sme, fidl_fuchsia_wlan_wlanix as fidl_wlanix,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_zircon as zx,
    futures::StreamExt,
    netlink_packet_core::{NetlinkDeserializable, NetlinkHeader, NetlinkSerializable},
    netlink_packet_generic::GenlMessage,
    parking_lot::Mutex,
    std::{
        convert::{TryFrom, TryInto},
        sync::Arc,
    },
    tracing::{error, info, warn},
    wlan_common::channel::Channel,
};

mod ifaces;
#[allow(unused)]
mod nl80211;

use {
    ifaces::{ClientIface, IfaceManager},
    nl80211::{Nl80211, Nl80211Attr, Nl80211BandAttr, Nl80211Cmd, Nl80211FrequencyAttr},
};

const FAKE_CHIP_ID: u32 = 1;
const IFACE_NAME: &str = "sta-iface-name";

async fn handle_wifi_sta_iface_request(req: fidl_wlanix::WifiStaIfaceRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiStaIfaceRequest::GetName { responder } => {
            info!("fidl_wlanix::WifiStaIfaceRequest::GetName");
            let response = fidl_wlanix::WifiStaIfaceGetNameResponse {
                iface_name: Some(IFACE_NAME.to_string()),
                ..Default::default()
            };
            responder.send(&response).context("send GetName response")?;
        }
        fidl_wlanix::WifiStaIfaceRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WifiStaIfaceRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wifi_sta_iface(reqs: fidl_wlanix::WifiStaIfaceRequestStream) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_sta_iface_request(req).await {
                    warn!("Failed to handle WifiStaIfaceRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wifi sta iface request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_wifi_chip_request(req: fidl_wlanix::WifiChipRequest) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiChipRequest::CreateStaIface { payload, responder, .. } => {
            info!("fidl_wlanix::WifiChipRequest::CreateStaIface");
            match payload.iface {
                Some(iface) => {
                    let reqs = iface.into_stream().context("create WifiStaIface stream")?;
                    responder.send(Ok(())).context("send CreateStaIface response")?;
                    serve_wifi_sta_iface(reqs).await;
                }
                None => {
                    responder
                        .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                        .context("send CreateStaIface response")?;
                }
            }
        }
        fidl_wlanix::WifiChipRequest::GetAvailableModes { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetAvailableModes");
            let response = fidl_wlanix::WifiChipGetAvailableModesResponse {
                chip_modes: Some(vec![fidl_wlanix::ChipMode {
                    id: Some(1),
                    available_combinations: Some(vec![fidl_wlanix::ChipConcurrencyCombination {
                        limits: Some(vec![fidl_wlanix::ChipConcurrencyCombinationLimit {
                            types: Some(vec![fidl_wlanix::IfaceConcurrencyType::Sta]),
                            max_ifaces: Some(1),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }]),
                ..Default::default()
            };
            responder.send(&response).context("send GetAvailableModes response")?;
        }
        fidl_wlanix::WifiChipRequest::GetMode { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetMode");
            let response =
                fidl_wlanix::WifiChipGetModeResponse { mode: Some(0), ..Default::default() };
            responder.send(&response).context("send GetMode response")?;
        }
        fidl_wlanix::WifiChipRequest::GetCapabilities { responder } => {
            info!("fidl_wlanix::WifiChipRequest::GetCapabilities");
            let response = fidl_wlanix::WifiChipGetCapabilitiesResponse {
                capabilities_mask: Some(0),
                ..Default::default()
            };
            responder.send(&response).context("send GetCapabilities response")?;
        }
        fidl_wlanix::WifiChipRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WifiChipRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wifi_chip(_chip_id: u32, reqs: fidl_wlanix::WifiChipRequestStream) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_chip_request(req).await {
                    warn!("Failed to handle WifiChipRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wifi chip request stream failed: {}", e);
            }
        }
    })
    .await;
}

fn run_callbacks<T>(
    callback_fn: impl Fn(&T) -> Result<(), fidl::Error>,
    callbacks: &[T],
    ctx: &'static str,
) {
    let mut failed_callbacks = 0u32;
    for callback in callbacks {
        if let Err(_e) = callback_fn(callback) {
            failed_callbacks += 1;
        }
    }
    if failed_callbacks > 0 {
        warn!("Failed sending {} event to {} subscribers", ctx, failed_callbacks);
    }
}

#[derive(Default)]
struct WifiState {
    started: bool,
    callbacks: Vec<fidl_wlanix::WifiEventCallbackProxy>,
    scan_multicast_proxy: Option<fidl_wlanix::Nl80211MulticastProxy>,
}

async fn handle_wifi_request(
    req: fidl_wlanix::WifiRequest,
    state: Arc<Mutex<WifiState>>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::WifiRequest::RegisterEventCallback { payload, .. } => {
            info!("fidl_wlanix::WifiRequest::RegisterEventCallback");
            if let Some(callback) = payload.callback {
                state.lock().callbacks.push(callback.into_proxy()?);
            }
        }
        fidl_wlanix::WifiRequest::Start { responder } => {
            info!("fidl_wlanix::WifiRequest::Start");
            let mut state = state.lock();
            state.started = true;
            responder.send(Ok(())).context("send Start response")?;
            run_callbacks(
                fidl_wlanix::WifiEventCallbackProxy::on_start,
                &state.callbacks[..],
                "OnStart",
            );
        }
        fidl_wlanix::WifiRequest::Stop { responder } => {
            info!("fidl_wlanix::WifiRequest::Stop");
            let mut state = state.lock();
            state.started = false;
            responder.send(Ok(())).context("send Stop response")?;
            run_callbacks(
                fidl_wlanix::WifiEventCallbackProxy::on_stop,
                &state.callbacks[..],
                "OnStop",
            );
        }
        fidl_wlanix::WifiRequest::GetState { responder } => {
            info!("fidl_wlanix::WifiRequest::GetState");
            let response = fidl_wlanix::WifiGetStateResponse {
                is_started: Some(state.lock().started),
                ..Default::default()
            };
            responder.send(&response).context("send GetState response")?;
        }
        fidl_wlanix::WifiRequest::GetChipIds { responder } => {
            info!("fidl_wlanix::WifiRequest::GetChipIds");
            let response = fidl_wlanix::WifiGetChipIdsResponse {
                chip_ids: Some(vec![FAKE_CHIP_ID]),
                ..Default::default()
            };
            responder.send(&response).context("send GetChipIds response")?;
        }
        fidl_wlanix::WifiRequest::GetChip { payload, responder } => {
            info!("fidl_wlanix::WifiRequest::GetChip - chip_id {:?}", payload.chip_id);
            match (payload.chip_id, payload.chip) {
                (Some(chip_id), Some(chip)) => {
                    let chip_stream = chip.into_stream().context("create WifiChip stream")?;
                    responder.send(Ok(())).context("send GetChip response")?;
                    serve_wifi_chip(chip_id, chip_stream).await;
                }
                _ => {
                    warn!("No chip_id or chip in fidl_wlanix::WifiRequest::GetChip");
                    responder
                        .send(Err(zx::sys::ZX_ERR_INVALID_ARGS))
                        .context("send GetChip response")?;
                }
            }
        }
        fidl_wlanix::WifiRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WifiRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wifi(reqs: fidl_wlanix::WifiRequestStream, state: Arc<Mutex<WifiState>>) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_wifi_request(req, Arc::clone(&state)).await {
                    warn!("Failed to handle WifiRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wifi request stream failed: {}", e);
            }
        }
    })
    .await;
}

#[derive(Default)]
struct SupplicantStaNetworkState {
    ssid: Option<Vec<u8>>,
}

#[derive(Default)]
struct SupplicantStaIfaceState {
    callbacks: Vec<fidl_wlanix::SupplicantStaIfaceCallbackProxy>,
}

async fn handle_supplicant_sta_network_request<C: ClientIface>(
    req: fidl_wlanix::SupplicantStaNetworkRequest,
    sta_network_state: Arc<Mutex<SupplicantStaNetworkState>>,
    sta_iface_state: Arc<Mutex<SupplicantStaIfaceState>>,
    iface: Arc<C>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::SupplicantStaNetworkRequest::SetSsid { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaNetworkRequest::SetSsid");
            if let Some(ssid) = payload.ssid {
                sta_network_state.lock().ssid.replace(ssid);
            }
        }
        fidl_wlanix::SupplicantStaNetworkRequest::Select { responder } => {
            info!("fidl_wlanix::SupplicantStaNetworkRequest::Select");
            let ssid = sta_network_state.lock().ssid.clone();
            let result = match ssid {
                Some(ssid) => match iface.connect_to_network(&ssid[..]).await {
                    Ok(connected_result) => {
                        info!("Connected to requested network");
                        let event = fidl_wlanix::SupplicantStaIfaceCallbackOnStateChangedRequest {
                            new_state: Some(fidl_wlanix::StaIfaceCallbackState::Completed),
                            bssid: Some(connected_result.bssid.0),
                            // TODO(fxbug.dev/128604): do we need to keep track of actual id?
                            id: Some(1),
                            ssid: Some(connected_result.ssid),
                            ..Default::default()
                        };
                        run_callbacks(
                            |callback_proxy| callback_proxy.on_state_changed(&event),
                            &sta_iface_state.lock().callbacks[..],
                            "on_state_changed",
                        );
                        Ok(())
                    }
                    Err(e) => {
                        warn!("Connecting to network failed: {}", e);
                        Err(zx::sys::ZX_ERR_INTERNAL)
                    }
                },
                None => {
                    warn!("No SSID set. fidl_wlanix::SupplicantStaNetworkRequest::Select ignored");
                    Err(zx::sys::ZX_ERR_BAD_STATE)
                }
            };
            responder.send(result).context("send Select response")?;
        }
        fidl_wlanix::SupplicantStaNetworkRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown SupplicantStaNetworkRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_supplicant_sta_network<C: ClientIface>(
    reqs: fidl_wlanix::SupplicantStaNetworkRequestStream,
    sta_iface_state: Arc<Mutex<SupplicantStaIfaceState>>,
    iface: Arc<C>,
) {
    let sta_network_state = Arc::new(Mutex::new(SupplicantStaNetworkState::default()));
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_supplicant_sta_network_request(
                    req,
                    Arc::clone(&sta_network_state),
                    Arc::clone(&sta_iface_state),
                    Arc::clone(&iface),
                )
                .await
                {
                    warn!("Failed to handle SupplicantStaNetwork: {}", e);
                }
            }
            Err(e) => {
                error!("SupplicantStaNetwork request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_supplicant_sta_iface_request<C: ClientIface>(
    req: fidl_wlanix::SupplicantStaIfaceRequest,
    sta_iface_state: Arc<Mutex<SupplicantStaIfaceState>>,
    iface: Arc<C>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::SupplicantStaIfaceRequest::RegisterCallback { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::RegisterCallback");
            if let Some(callback) = payload.callback {
                sta_iface_state.lock().callbacks.push(callback.into_proxy()?);
            }
        }
        fidl_wlanix::SupplicantStaIfaceRequest::AddNetwork { payload, .. } => {
            info!("fidl_wlanix::SupplicantStaIfaceRequest::AddNetwork");
            if let Some(supplicant_sta_network) = payload.network {
                let supplicant_sta_network_stream = supplicant_sta_network
                    .into_stream()
                    .context("create SupplicantStaNetwork stream")?;
                // TODO(fxbug.dev/128604): Should we return NetworkAdded event?
                serve_supplicant_sta_network(supplicant_sta_network_stream, sta_iface_state, iface)
                    .await;
            }
        }
        fidl_wlanix::SupplicantStaIfaceRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown SupplicantStaIfaceRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_supplicant_sta_iface<C: ClientIface>(
    reqs: fidl_wlanix::SupplicantStaIfaceRequestStream,
    iface: Arc<C>,
) {
    let sta_iface_state = Arc::new(Mutex::new(SupplicantStaIfaceState::default()));
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_supplicant_sta_iface_request(
                    req,
                    Arc::clone(&sta_iface_state),
                    Arc::clone(&iface),
                )
                .await
                {
                    warn!("Failed to handle SupplicantRequest: {}", e);
                }
            }
            Err(e) => {
                error!("SupplicantStaIface request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn handle_supplicant_request<I: IfaceManager>(
    req: fidl_wlanix::SupplicantRequest,
    iface_manager: Arc<I>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::SupplicantRequest::AddStaInterface { payload, .. } => {
            info!("fidl_wlanix::SupplicantRequest::AddStaInterface");
            if let Some(supplicant_sta_iface) = payload.iface {
                // TODO(fxbug.dev/128604): Actually create the iface.
                let existing_ifaces = iface_manager.list_interfaces().await?;
                for iface in existing_ifaces {
                    if iface.role == fidl_fuchsia_wlan_common::WlanMacRole::Client {
                        let client_iface = iface_manager.get_client_iface(iface.id).await?;
                        let supplicant_sta_iface_stream = supplicant_sta_iface
                            .into_stream()
                            .context("create SupplicantStaIface stream")?;
                        serve_supplicant_sta_iface(supplicant_sta_iface_stream, client_iface).await;
                        break;
                    }
                }
            }
        }
        fidl_wlanix::SupplicantRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown SupplicantRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_supplicant<I: IfaceManager>(
    reqs: fidl_wlanix::SupplicantRequestStream,
    iface_manager: Arc<I>,
) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) = handle_supplicant_request(req, Arc::clone(&iface_manager)).await {
                    warn!("Failed to handle SupplicantRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Supplicant request stream failed: {}", e);
            }
        }
    })
    .await;
}

fn nl80211_message_resp(
    responses: Vec<fidl_wlanix::Nl80211Message>,
) -> fidl_wlanix::Nl80211MessageResponse {
    fidl_wlanix::Nl80211MessageResponse { responses: Some(responses), ..Default::default() }
}

fn build_nl80211_message(cmd: Nl80211Cmd, attrs: Vec<Nl80211Attr>) -> fidl_wlanix::Nl80211Message {
    let resp = GenlMessage::from_payload(Nl80211 { cmd, attrs });
    let mut buffer = vec![0u8; resp.buffer_len()];
    resp.serialize(&mut buffer);
    fidl_wlanix::Nl80211Message {
        message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
        payload: Some(buffer),
        ..Default::default()
    }
}

fn build_nl80211_ack() -> fidl_wlanix::Nl80211Message {
    fidl_wlanix::Nl80211Message {
        message_type: Some(fidl_wlanix::Nl80211MessageType::Ack),
        payload: None,
        ..Default::default()
    }
}

fn build_nl80211_done() -> fidl_wlanix::Nl80211Message {
    fidl_wlanix::Nl80211Message {
        message_type: Some(fidl_wlanix::Nl80211MessageType::Done),
        payload: None,
        ..Default::default()
    }
}

async fn handle_nl80211_message<I: IfaceManager>(
    netlink_message: fidl_wlanix::Nl80211Message,
    responder: fidl_wlanix::Nl80211MessageResponder,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
) -> Result<(), Error> {
    let payload = match netlink_message {
        fidl_wlanix::Nl80211Message {
            message_type: Some(fidl_wlanix::Nl80211MessageType::Message),
            payload: Some(p),
            ..
        } => p,
        _ => return Ok(()),
    };
    let deserialized = GenlMessage::<Nl80211>::deserialize(&NetlinkHeader::default(), &payload[..]);
    let Ok(message) = deserialized else {
        bail!("Failed to parse nl80211 message: {}", deserialized.unwrap_err())
    };
    match message.payload.cmd {
        Nl80211Cmd::GetWiphy => {
            info!("Nl80211Cmd::GetWiphy");
            responder
                .send(Ok(nl80211_message_resp(vec![build_nl80211_message(
                    Nl80211Cmd::NewWiphy,
                    vec![
                        // Phy ID
                        Nl80211Attr::Wiphy(0),
                        // Supported bands
                        Nl80211Attr::WiphyBands(vec![vec![Nl80211BandAttr::Frequencies({
                            // Hardcode US non-DFS frequencies for now.
                            vec![
                                2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462,
                                2467, 2472, 2484, 5180, 5190, 5200, 5210, 5220, 5230, 5240, 5745,
                                5755, 5765, 5775, 5785, 5795, 5805, 5825,
                            ]
                            .into_iter()
                            .map(|freq| vec![Nl80211FrequencyAttr::Frequency(freq)])
                            .collect()
                        })]]),
                        // Scan capabilities
                        Nl80211Attr::MaxScanSsids(32),
                        Nl80211Attr::MaxScheduledScanSsids(32),
                        Nl80211Attr::MaxMatchSets(32),
                        // Feature flags
                        Nl80211Attr::FeatureFlags(0),
                        Nl80211Attr::ExtendedFeatures(vec![]),
                    ],
                )])))
                .context("Failed to send NewWiphy")?;
        }
        Nl80211Cmd::GetInterface => {
            info!("Nl80211Cmd::GetInterface");
            let ifaces = iface_manager.list_interfaces().await?;
            let mut resp = vec![];
            for iface in ifaces {
                resp.push(build_nl80211_message(
                    Nl80211Cmd::NewInterface,
                    vec![
                        Nl80211Attr::IfaceIndex(iface.id.into()),
                        // TODO(fxbug.dev/128604): Populate this with the real iface name assigned by netcfg.
                        Nl80211Attr::IfaceName(IFACE_NAME.to_string()),
                        Nl80211Attr::Mac(iface.sta_addr),
                    ],
                ));
            }
            resp.push(build_nl80211_done());
            responder
                .send(Ok(nl80211_message_resp(resp)))
                .context("Failed to send scan results")?;
        }
        Nl80211Cmd::GetProtocolFeatures => {
            info!("Nl80211Cmd::GetProtocolFeatures");
            responder
                .send(Ok(nl80211_message_resp(vec![build_nl80211_message(
                    Nl80211Cmd::GetProtocolFeatures,
                    vec![Nl80211Attr::ProtocolFeatures(0)],
                )])))
                .context("Failed to send GetProtocolFeatures")?;
        }
        Nl80211Cmd::TriggerScan => {
            info!("Nl80211Cmd::TriggerScan");
            match find_iface_id(&message.payload.attrs[..]) {
                Some(iface_id) => {
                    let client_iface = iface_manager.get_client_iface(iface_id.try_into()?).await?;
                    responder
                        .send(Ok(nl80211_message_resp(vec![build_nl80211_ack()])))
                        .context("Failed to ack TriggerScan")?;
                    match client_iface.trigger_scan().await {
                        Ok(()) => info!("Passive scan completed successfully"),
                        Err(e) => error!("Failed to run passive scan: {:?}", e),
                    }
                    if let Some(proxy) = state.lock().scan_multicast_proxy.as_ref() {
                        proxy
                            .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                                message: Some(build_nl80211_message(
                                    Nl80211Cmd::NewScanResults,
                                    vec![Nl80211Attr::IfaceIndex(iface_id)],
                                )),
                                ..Default::default()
                            })
                            .context("Failed to send NewScanResults")?;
                    }
                }
                None => bail!("TriggerScan did not include an iface id"),
            }
        }
        Nl80211Cmd::GetScan => {
            info!("Nl80211Cmd::GetScan");
            match find_iface_id(&message.payload.attrs[..]) {
                Some(iface_id) => {
                    let client_iface = iface_manager.get_client_iface(iface_id.try_into()?).await?;
                    let results = client_iface.get_last_scan_results();
                    info!("Processing {} scan results", results.len());
                    let mut resp = vec![];
                    for result in results {
                        resp.push(build_nl80211_message(
                            Nl80211Cmd::NewScanResults,
                            vec![Nl80211Attr::IfaceIndex(iface_id), convert_scan_result(result)],
                        ));
                    }
                    resp.push(build_nl80211_done());
                    responder
                        .send(Ok(nl80211_message_resp(resp)))
                        .context("Failed to send scan results")?;
                }
                None => bail!("TriggerScan did not include an iface id"),
            }
        }
        _ => {
            warn!("Dropping nl80211 message: {:?}", message);
            responder
                .send(Ok(nl80211_message_resp(vec![])))
                .context("Failed to respond to unhandled message")?;
        }
    }
    Ok(())
}

fn convert_scan_result(result: fidl_sme::ScanResult) -> Nl80211Attr {
    use crate::nl80211::{ChainSignalAttr, Nl80211BssAttr};
    let channel = Channel::try_from(result.bss_description.channel).unwrap();
    let center_freq = channel.get_center_freq().expect("Failed to get center freq").into();
    Nl80211Attr::Bss(vec![
        Nl80211BssAttr::Bssid(result.bss_description.bssid),
        Nl80211BssAttr::Frequency(center_freq),
        Nl80211BssAttr::InformationElement(result.bss_description.ies),
        Nl80211BssAttr::LastSeenBoottime(fasync::Time::now().into_nanos() as u64),
        Nl80211BssAttr::SignalMbm(result.bss_description.rssi_dbm as i32 * 100),
        Nl80211BssAttr::Capability(result.bss_description.capability_info),
        Nl80211BssAttr::Status(0),
        // TODO(fxbug.dev/128604): Determine whether we should provide real chain signals.
        Nl80211BssAttr::ChainSignal(vec![ChainSignalAttr {
            id: 0,
            rssi: result.bss_description.rssi_dbm.into(),
        }]),
    ])
}

fn find_iface_id(attrs: &[Nl80211Attr]) -> Option<u32> {
    attrs
        .iter()
        .filter_map(|attr| match attr {
            Nl80211Attr::IfaceIndex(idx) => Some(idx),
            _ => None,
        })
        .cloned()
        .next()
}

async fn serve_nl80211<I: IfaceManager>(
    mut reqs: fidl_wlanix::Nl80211RequestStream,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
) {
    loop {
        let Some(req) = reqs.next().await else {
            warn!("Nl80211 stream terminated. Should only happen during shutdown.");
            return;
        };
        match req {
            Ok(fidl_wlanix::Nl80211Request::Message { payload, responder, .. }) => {
                if let Some(message) = payload.message {
                    if let Err(e) = handle_nl80211_message(
                        message,
                        responder,
                        Arc::clone(&state),
                        Arc::clone(&iface_manager),
                    )
                    .await
                    {
                        error!("Failed to handle Nl80211 message: {}", e);
                    }
                }
            }
            Ok(fidl_wlanix::Nl80211Request::GetMulticast { payload, .. }) => {
                if let Some(multicast) = payload.multicast {
                    if payload.group == Some("scan".to_string()) {
                        match multicast.into_proxy() {
                            Ok(proxy) => {
                                state.lock().scan_multicast_proxy.replace(proxy);
                            }
                            Err(e) => error!("Failed to create multicast proxy: {}", e),
                        }
                    } else {
                        warn!(
                            "Dropping channel for unsupported multicast group {:?}",
                            payload.group
                        );
                    }
                }
            }
            Ok(fidl_wlanix::Nl80211Request::_UnknownMethod { ordinal, .. }) => {
                warn!("Unknown Nl80211Request ordinal: {}", ordinal);
            }
            Err(e) => {
                error!("Nl80211 request stream failed: {}", e);
                return;
            }
        }
    }
}

async fn handle_wlanix_request<I: IfaceManager>(
    req: fidl_wlanix::WlanixRequest,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
) -> Result<(), Error> {
    match req {
        fidl_wlanix::WlanixRequest::GetWifi { payload, .. } => {
            info!("fidl_wlanix::WlanixRequest::GetWifi");
            if let Some(wifi) = payload.wifi {
                let wifi_stream = wifi.into_stream().context("create Wifi stream")?;
                serve_wifi(wifi_stream, Arc::clone(&state)).await;
            }
        }
        fidl_wlanix::WlanixRequest::GetSupplicant { payload, .. } => {
            info!("fidl_wlanix::WlanixRequest::GetSupplicant");
            if let Some(supplicant) = payload.supplicant {
                let supplicant_stream =
                    supplicant.into_stream().context("create Supplicant stream")?;
                serve_supplicant(supplicant_stream, Arc::clone(&iface_manager)).await;
            }
        }
        fidl_wlanix::WlanixRequest::GetNl80211 { payload, .. } => {
            info!("fidl_wlanix::WlanixRequest::GetNl80211");
            if let Some(nl80211) = payload.nl80211 {
                let nl80211_stream = nl80211.into_stream().context("create Nl80211 stream")?;
                serve_nl80211(nl80211_stream, Arc::clone(&state), Arc::clone(&iface_manager)).await;
            }
        }
        fidl_wlanix::WlanixRequest::_UnknownMethod { ordinal, .. } => {
            warn!("Unknown WlanixRequest ordinal: {}", ordinal);
        }
    }
    Ok(())
}

async fn serve_wlanix<I: IfaceManager>(
    reqs: fidl_wlanix::WlanixRequestStream,
    state: Arc<Mutex<WifiState>>,
    iface_manager: Arc<I>,
) {
    reqs.for_each_concurrent(None, |req| async {
        match req {
            Ok(req) => {
                if let Err(e) =
                    handle_wlanix_request(req, Arc::clone(&state), Arc::clone(&iface_manager)).await
                {
                    warn!("Failed to handle WlanixRequest: {}", e);
                }
            }
            Err(e) => {
                error!("Wlanix request stream failed: {}", e);
            }
        }
    })
    .await;
}

async fn serve_fidl<I: IfaceManager>(iface_manager: Arc<I>) -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    let state = Arc::new(Mutex::new(WifiState::default()));
    let _ = fs.dir("svc").add_fidl_service(move |reqs| {
        serve_wlanix(reqs, Arc::clone(&state), Arc::clone(&iface_manager))
    });
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |t| async { t.await }).await;
    Ok(())
}

#[fasync::run_singlethreaded]
async fn main() {
    diagnostics_log::initialize(
        diagnostics_log::PublishOptions::default()
            .tags(&["wlan", "wlanix"])
            .enable_metatag(diagnostics_log::Metatag::Target),
    )
    .expect("Failed to initialize wlanix logs");
    info!("Starting Wlanix");
    let iface_manager = ifaces::DeviceMonitorIfaceManager::new()
        .expect("Failed to connect wlanix to wlandevicemonitor");
    match serve_fidl(Arc::new(iface_manager)).await {
        Ok(()) => info!("Wlanix exiting cleanly"),
        Err(e) => error!("Wlanix exiting with error: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::{create_proxy, create_proxy_and_stream, create_request_stream, Proxy},
        futures::{pin_mut, task::Poll, Future},
        ifaces::test_utils::TestIfaceManager,
        std::pin::Pin,
        wlan_common::assert_variant,
    };

    // This will only work if the message is a parseable nl80211 message. Some
    // attributes are currently write only in our NL80211 implementation. If a
    // write-only attribute is included, this function will panic.
    fn expect_nl80211_message(message: &fidl_wlanix::Nl80211Message) -> GenlMessage<Nl80211> {
        assert_eq!(message.message_type, Some(fidl_wlanix::Nl80211MessageType::Message));
        GenlMessage::deserialize(
            &NetlinkHeader::default(),
            &message.payload.as_ref().expect("Message should always have a payload")[..],
        )
        .expect("Failed to deserialize genetlink message")
    }

    #[test]
    fn test_wifi_get_state_is_started_false_at_beginning() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_state_fut = test_helper.wifi_proxy.get_state();
        pin_mut!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(false));
    }

    #[test]
    fn test_wifi_get_state_is_started_true_after_start() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let start_fut = test_helper.wifi_proxy.start();
        pin_mut!(start_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut start_fut), Poll::Pending);

        let get_state_fut = test_helper.wifi_proxy.get_state();
        pin_mut!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(true));
    }

    #[test]
    fn test_wifi_get_state_is_started_false_after_stop() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let start_fut = test_helper.wifi_proxy.start();
        pin_mut!(start_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut start_fut), Poll::Pending);

        let stop_fut = test_helper.wifi_proxy.stop();
        pin_mut!(stop_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut stop_fut), Poll::Pending);

        let get_state_fut = test_helper.wifi_proxy.get_state();
        pin_mut!(get_state_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_state_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_state_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.is_started, Some(false));
    }

    #[test]
    fn test_wifi_get_chip_ids() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_chip_ids_fut = test_helper.wifi_proxy.get_chip_ids();
        pin_mut!(get_chip_ids_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_chip_ids_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_chip_ids_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.chip_ids, Some(vec![1]));
    }

    #[test]
    fn test_wifi_chip_get_available_modes() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_available_modes_fut = test_helper.wifi_chip_proxy.get_available_modes();
        pin_mut!(get_available_modes_fut);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_available_modes_fut),
            Poll::Pending
        );
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_available_modes_fut),
            Poll::Ready(Ok(response)) => response
        );
        let expected_response = fidl_wlanix::WifiChipGetAvailableModesResponse {
            chip_modes: Some(vec![fidl_wlanix::ChipMode {
                id: Some(1),
                available_combinations: Some(vec![fidl_wlanix::ChipConcurrencyCombination {
                    limits: Some(vec![fidl_wlanix::ChipConcurrencyCombinationLimit {
                        types: Some(vec![fidl_wlanix::IfaceConcurrencyType::Sta]),
                        max_ifaces: Some(1),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }]),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(response, expected_response);
    }

    #[test]
    fn test_wifi_chip_get_mode() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_mode_fut = test_helper.wifi_chip_proxy.get_mode();
        pin_mut!(get_mode_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_mode_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(test_helper.exec.run_until_stalled(&mut get_mode_fut), Poll::Ready(Ok(response)) => response);
        assert_eq!(response.mode, Some(0));
    }

    #[test]
    fn test_wifi_chip_get_capabilities() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_capabilities_fut = test_helper.wifi_chip_proxy.get_capabilities();
        pin_mut!(get_capabilities_fut);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_capabilities_fut),
            Poll::Pending
        );
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_capabilities_fut),
            Poll::Ready(Ok(response)) => response,
        );
        assert_eq!(response.capabilities_mask, Some(0));
    }

    #[test]
    fn test_wifi_sta_iface_get_name() {
        let (mut test_helper, mut test_fut) = setup_wifi_test();

        let get_name_fut = test_helper.wifi_sta_iface_proxy.get_name();
        pin_mut!(get_name_fut);
        assert_variant!(test_helper.exec.run_until_stalled(&mut get_name_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let response = assert_variant!(
            test_helper.exec.run_until_stalled(&mut get_name_fut),
            Poll::Ready(Ok(response)) => response
        );
        assert_eq!(response.iface_name, Some("sta-iface-name".to_string()));
    }

    struct WifiTestHelper {
        _wlanix_proxy: fidl_wlanix::WlanixProxy,
        wifi_proxy: fidl_wlanix::WifiProxy,
        wifi_chip_proxy: fidl_wlanix::WifiChipProxy,
        wifi_sta_iface_proxy: fidl_wlanix::WifiStaIfaceProxy,

        // Note: keep the executor field last in the struct so it gets dropped last.
        exec: fasync::TestExecutor,
    }

    fn setup_wifi_test() -> (WifiTestHelper, Pin<Box<impl Future<Output = ()>>>) {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (wlanix_proxy, wlanix_stream) = create_proxy_and_stream::<fidl_wlanix::WlanixMarker>()
            .expect("create Wlanix proxy should succeed");
        let (wifi_proxy, wifi_server_end) =
            create_proxy::<fidl_wlanix::WifiMarker>().expect("create Wifi proxy should succeed");
        let result = wlanix_proxy.get_wifi(fidl_wlanix::WlanixGetWifiRequest {
            wifi: Some(wifi_server_end),
            ..Default::default()
        });
        assert_variant!(result, Ok(()));

        let (wifi_chip_proxy, wifi_chip_server_end) = create_proxy::<fidl_wlanix::WifiChipMarker>()
            .expect("create WifiChip proxy should succeed");
        let get_chip_fut = wifi_proxy.get_chip(fidl_wlanix::WifiGetChipRequest {
            chip_id: Some(1),
            chip: Some(wifi_chip_server_end),
            ..Default::default()
        });
        pin_mut!(get_chip_fut);
        assert_variant!(exec.run_until_stalled(&mut get_chip_fut), Poll::Pending);

        let (wifi_sta_iface_proxy, wifi_sta_iface_server_end) =
            create_proxy::<fidl_wlanix::WifiStaIfaceMarker>()
                .expect("create WifiStaIface proxy should succeed");
        let create_sta_iface_fut =
            wifi_chip_proxy.create_sta_iface(fidl_wlanix::WifiChipCreateStaIfaceRequest {
                iface: Some(wifi_sta_iface_server_end),
                ..Default::default()
            });
        pin_mut!(create_sta_iface_fut);
        assert_variant!(exec.run_until_stalled(&mut create_sta_iface_fut), Poll::Pending);

        let wifi_state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new());
        let test_fut = serve_wlanix(wlanix_stream, wifi_state, iface_manager);
        let mut test_fut = Box::pin(test_fut);
        assert_eq!(exec.run_until_stalled(&mut test_fut), Poll::Pending);

        assert_variant!(exec.run_until_stalled(&mut get_chip_fut), Poll::Ready(Ok(Ok(()))));
        assert_variant!(exec.run_until_stalled(&mut create_sta_iface_fut), Poll::Ready(Ok(Ok(()))));

        let test_helper = WifiTestHelper {
            _wlanix_proxy: wlanix_proxy,
            wifi_proxy,
            wifi_chip_proxy,
            wifi_sta_iface_proxy,
            exec,
        };
        (test_helper, test_fut)
    }

    #[test]
    fn test_supplicant_sta_network_connect_flow() {
        let (mut test_helper, mut test_fut) = setup_supplicant_test();

        let result = test_helper.supplicant_sta_network_proxy.set_ssid(
            &fidl_wlanix::SupplicantStaNetworkSetSsidRequest {
                ssid: Some(vec![b'f', b'o', b'o']),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let mut network_select_fut = test_helper.supplicant_sta_network_proxy.select();
        assert_variant!(test_helper.exec.run_until_stalled(&mut network_select_fut), Poll::Pending);
        assert_variant!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            test_helper.exec.run_until_stalled(&mut network_select_fut),
            Poll::Ready(Ok(Ok(())))
        );

        assert_eq!(
            *test_helper.iface_manager.client_iface.as_ref().unwrap().connected_ssid.lock(),
            Some(vec![b'f', b'o', b'o'])
        );
        let mut next_callback_fut = test_helper.supplicant_sta_iface_callback_stream.next();
        let on_state_changed = assert_variant!(test_helper.exec.run_until_stalled(&mut next_callback_fut), Poll::Ready(Some(Ok(fidl_wlanix::SupplicantStaIfaceCallbackRequest::OnStateChanged { payload, .. }))) => payload);
        assert_eq!(on_state_changed.new_state, Some(fidl_wlanix::StaIfaceCallbackState::Completed));
        assert_eq!(on_state_changed.bssid, Some([42, 42, 42, 42, 42, 42]));
        assert_eq!(on_state_changed.id, Some(1));
        assert_eq!(on_state_changed.ssid, Some(vec![b'f', b'o', b'o']));
    }

    struct SupplicantTestHelper {
        _wlanix_proxy: fidl_wlanix::WlanixProxy,
        _supplicant_proxy: fidl_wlanix::SupplicantProxy,
        _supplicant_sta_iface_proxy: fidl_wlanix::SupplicantStaIfaceProxy,
        supplicant_sta_network_proxy: fidl_wlanix::SupplicantStaNetworkProxy,
        supplicant_sta_iface_callback_stream: fidl_wlanix::SupplicantStaIfaceCallbackRequestStream,
        iface_manager: Arc<TestIfaceManager>,

        // Note: keep the executor field last in the struct so it gets dropped last.
        exec: fasync::TestExecutor,
    }

    fn setup_supplicant_test() -> (SupplicantTestHelper, Pin<Box<impl Future<Output = ()>>>) {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (wlanix_proxy, wlanix_stream) = create_proxy_and_stream::<fidl_wlanix::WlanixMarker>()
            .expect("create Wlanix proxy should succeed");
        let (supplicant_proxy, supplicant_server_end) =
            create_proxy::<fidl_wlanix::SupplicantMarker>()
                .expect("create Supplicant proxy should succeed");
        let result = wlanix_proxy.get_supplicant(fidl_wlanix::WlanixGetSupplicantRequest {
            supplicant: Some(supplicant_server_end),
            ..Default::default()
        });
        assert_variant!(result, Ok(()));

        let (supplicant_sta_iface_proxy, supplicant_sta_iface_server_end) =
            create_proxy::<fidl_wlanix::SupplicantStaIfaceMarker>()
                .expect("create SupplicantStaIface proxy should succeed");
        let result =
            supplicant_proxy.add_sta_interface(fidl_wlanix::SupplicantAddStaInterfaceRequest {
                iface: Some(supplicant_sta_iface_server_end),
                iface_name: Some("fake-iface-name".to_string()),
                ..Default::default()
            });
        assert_variant!(result, Ok(()));

        let (supplicant_sta_iface_callback_client_end, supplicant_sta_iface_callback_stream) =
            create_request_stream::<fidl_wlanix::SupplicantStaIfaceCallbackMarker>()
                .expect("create SupplicantStaIfaceCallback request stream should succeed");
        let result = supplicant_sta_iface_proxy.register_callback(
            fidl_wlanix::SupplicantStaIfaceRegisterCallbackRequest {
                callback: Some(supplicant_sta_iface_callback_client_end),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let (supplicant_sta_network_proxy, supplicant_sta_network_server_end) =
            create_proxy::<fidl_wlanix::SupplicantStaNetworkMarker>()
                .expect("create SupplicantStaNetwork proxy should succeed");
        let result = supplicant_sta_iface_proxy.add_network(
            fidl_wlanix::SupplicantStaIfaceAddNetworkRequest {
                network: Some(supplicant_sta_network_server_end),
                ..Default::default()
            },
        );
        assert_variant!(result, Ok(()));

        let wifi_state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let test_fut = serve_wlanix(wlanix_stream, wifi_state, Arc::clone(&iface_manager));
        let mut test_fut = Box::pin(test_fut);
        assert_eq!(exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let test_helper = SupplicantTestHelper {
            _wlanix_proxy: wlanix_proxy,
            _supplicant_proxy: supplicant_proxy,
            _supplicant_sta_iface_proxy: supplicant_sta_iface_proxy,
            supplicant_sta_network_proxy,
            supplicant_sta_iface_callback_stream,
            iface_manager,
            exec,
        };
        (test_helper, test_fut)
    }

    #[test]
    fn get_nl80211() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) = create_proxy_and_stream::<fidl_wlanix::WlanixMarker>()
            .expect("Failed to get proxy and req stream");
        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new());
        let wlanix_fut = serve_wlanix(stream, state, iface_manager);
        pin_mut!(wlanix_fut);
        let (nl_proxy, nl_server) =
            create_proxy::<fidl_wlanix::Nl80211Marker>().expect("Failed to get proxy");
        proxy
            .get_nl80211(fidl_wlanix::WlanixGetNl80211Request {
                nl80211: Some(nl_server),
                ..Default::default()
            })
            .expect("Failed to get Nl80211");
        assert_variant!(exec.run_until_stalled(&mut wlanix_fut), Poll::Pending);
        assert!(!nl_proxy.is_closed());
    }

    #[test]
    fn unsupported_mcast_group() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) =
            create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>().expect("Failed to get proxy");

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        pin_mut!(nl80211_fut);

        let (mcast_client, mut mcast_stream) =
            create_request_stream::<fidl_wlanix::Nl80211MulticastMarker>()
                .expect("Failed to create mcast request stream");
        proxy
            .get_multicast(fidl_wlanix::Nl80211GetMulticastRequest {
                group: Some("doesnt_exist".to_string()),
                multicast: Some(mcast_client),
                ..Default::default()
            })
            .expect("Failed to get multicast");
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);

        // The stream should immediately terminate.
        let next_mcast = mcast_stream.next();
        pin_mut!(next_mcast);
        assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Ready(None));

        // serve_nl80211 should complete successfully.
        drop(proxy);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Ready(()));
    }

    #[test]
    fn get_interface() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) =
            create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>().expect("Failed to get proxy");

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        pin_mut!(nl80211_fut);

        let get_interface_message = build_nl80211_message(Nl80211Cmd::GetInterface, vec![]);
        let get_interface_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(get_interface_message),
            ..Default::default()
        });
        pin_mut!(get_interface_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut get_interface_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );

        assert_eq!(responses.len(), 2);
        let message = expect_nl80211_message(&responses[0]);
        assert_eq!(message.payload.cmd, Nl80211Cmd::NewInterface);
        assert!(message.payload.attrs.iter().any(|attr| *attr
            == Nl80211Attr::IfaceIndex(ifaces::test_utils::FAKE_IFACE_RESPONSE.id.into())));
        assert!(message.payload.attrs.iter().any(
            |attr| *attr == Nl80211Attr::Mac(ifaces::test_utils::FAKE_IFACE_RESPONSE.sta_addr)
        ));
        assert_eq!(responses[1].message_type, Some(fidl_wlanix::Nl80211MessageType::Done));
    }

    #[test]
    fn trigger_scan() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) =
            create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>().expect("Failed to get proxy");

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        pin_mut!(nl80211_fut);

        let (mcast_client, mut mcast_stream) =
            create_request_stream::<fidl_wlanix::Nl80211MulticastMarker>()
                .expect("Failed to create mcast request stream");
        proxy
            .get_multicast(fidl_wlanix::Nl80211GetMulticastRequest {
                group: Some("scan".to_string()),
                multicast: Some(mcast_client),
                ..Default::default()
            })
            .expect("Failed to get multicast");
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);

        let next_mcast = mcast_stream.next();
        pin_mut!(next_mcast);
        assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Pending);

        let trigger_scan_message =
            build_nl80211_message(Nl80211Cmd::TriggerScan, vec![Nl80211Attr::IfaceIndex(0)]);
        let trigger_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(trigger_scan_message),
            ..Default::default()
        });

        pin_mut!(trigger_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut trigger_scan_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].message_type, Some(fidl_wlanix::Nl80211MessageType::Ack));

        // With our faked scan results we expect an immediate multicast notification.
        let mcast_req = assert_variant!(exec.run_until_stalled(&mut next_mcast), Poll::Ready(Some(Ok(msg))) => msg);
        let mcast_msg = assert_variant!(mcast_req, fidl_wlanix::Nl80211MulticastRequest::Message {
            payload: fidl_wlanix::Nl80211MulticastMessageRequest {message: Some(m), .. }, ..} => m);
        let payload = assert_variant!(mcast_msg.payload, Some(p) => p);
        let deser_msg =
            GenlMessage::<Nl80211>::deserialize(&NetlinkHeader::default(), &payload[..])
                .expect("Failed to deserialize payload");
        assert_eq!(deser_msg.payload.cmd, Nl80211Cmd::NewScanResults);
    }

    #[test]
    fn get_scan_results() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, stream) =
            create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>().expect("Failed to get proxy");

        let state = Arc::new(Mutex::new(WifiState::default()));
        let iface_manager = Arc::new(TestIfaceManager::new_with_client());
        let nl80211_fut = serve_nl80211(stream, state, iface_manager);
        pin_mut!(nl80211_fut);

        let get_scan_message =
            build_nl80211_message(Nl80211Cmd::GetScan, vec![Nl80211Attr::IfaceIndex(0)]);
        let get_scan_fut = proxy.message(fidl_wlanix::Nl80211MessageRequest {
            message: Some(get_scan_message),
            ..Default::default()
        });

        pin_mut!(get_scan_fut);
        assert_variant!(exec.run_until_stalled(&mut nl80211_fut), Poll::Pending);
        let responses = assert_variant!(
            exec.run_until_stalled(&mut get_scan_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse{responses: Some(r), ..}))) => r,
        );
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].message_type, Some(fidl_wlanix::Nl80211MessageType::Message));
        assert_eq!(responses[1].message_type, Some(fidl_wlanix::Nl80211MessageType::Done));
    }
}
