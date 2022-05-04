// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod link_state;

use {
    crate::{
        client::{
            event::{self, Event},
            internal::Context,
            protection::{build_protection_ie, Protection, ProtectionIe},
            report_connect_finished, AssociationFailure, ClientConfig, ClientSmeStatus,
            ConnectFailure, ConnectResult, ConnectTransactionEvent, ConnectTransactionSink,
            EstablishRsnaFailure, EstablishRsnaFailureReason, ServingApInfo,
        },
        mlme_event_name, MlmeRequest, MlmeSink,
    },
    anyhow::bail,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_mlme::{self as fidl_mlme, MlmeEvent},
    fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_inspect_contrib::{inspect_log, log::InspectBytes},
    fuchsia_zircon as zx,
    ieee80211::{Bssid, MacAddr},
    link_state::LinkState,
    log::{error, info, warn},
    wlan_common::{
        bss::BssDescription,
        capabilities::derive_join_capabilities,
        format::MacFmt as _,
        ie::{
            self,
            rsn::{cipher, suite_selector::OUI},
        },
        security::wep::WepKey,
        timer::EventId,
    },
    wlan_rsn::{
        auth,
        rsna::{AuthStatus, SecAssocUpdate, UpdateSink},
    },
    wlan_statemachine::*,
};
const DEFAULT_JOIN_FAILURE_TIMEOUT: u32 = 20; // beacon intervals
const DEFAULT_AUTH_FAILURE_TIMEOUT: u32 = 20; // beacon intervals
/// Timeout for the MLME connect op, which consists of Join, Auth, and Assoc steps.
/// TODO(fxbug.dev/99620): Consider having a single overall connect timeout that is
///                        managed by SME and also includes the EstablishRsna step.
const DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT: u32 = 60; // beacon intervals

const IDLE_STATE: &str = "IdleState";
const JOINING_STATE: &str = "JoiningState";
const AUTHENTICATING_STATE: &str = "AuthenticatingState";
const ASSOCIATING_STATE: &str = "AssociatingState";
const RSNA_STATE: &str = "EstablishingRsnaState";
const LINK_UP_STATE: &str = "LinkUpState";

#[derive(Debug)]
pub struct ConnectCommand {
    pub bss: Box<BssDescription>,
    pub connect_txn_sink: ConnectTransactionSink,
    pub protection: Protection,
}

#[derive(Debug)]
pub struct Idle {
    cfg: ClientConfig,
}

#[derive(Debug)]
pub struct Joining {
    cfg: ClientConfig,
    cmd: ConnectCommand,
    protection_ie: Option<ProtectionIe>,
}

#[derive(Debug)]
pub struct Authenticating {
    cfg: ClientConfig,
    cmd: ConnectCommand,
    protection_ie: Option<ProtectionIe>,
}

#[derive(Debug)]
pub struct Associating {
    cfg: ClientConfig,
    cmd: ConnectCommand,
    protection_ie: Option<ProtectionIe>,
}

#[derive(Debug)]
pub struct Associated {
    cfg: ClientConfig,
    connect_txn_sink: ConnectTransactionSink,
    latest_ap_state: Box<BssDescription>,
    auth_method: Option<auth::MethodName>,
    last_signal_report_time: zx::Time,
    link_state: LinkState,
    protection_ie: Option<ProtectionIe>,
    // TODO(fxbug.dev/82654): Remove `wmm_param` field when wlanstack telemetry is deprecated.
    wmm_param: Option<ie::WmmParam>,
    last_channel_switch_time: Option<zx::Time>,
}

statemachine!(
    #[derive(Debug)]
    pub enum ClientState,
    () => Idle,
    Idle => Joining,
    Joining => [Authenticating, Idle],
    Authenticating => [Associating, Idle],
    Associating => [Associated, Idle],
    // We transition back to Associating on a disassociation ind.
    Associated => [Idle, Associating],

    // Unified SME connect path
    Idle => Associating,
);

/// Context surrounding the state change, for Inspect logging
pub enum StateChangeContext {
    Disconnect { msg: String, disconnect_source: fidl_sme::DisconnectSource },
    Msg(String),
}

trait StateChangeContextExt {
    fn set_msg(&mut self, msg: String);
}

impl StateChangeContextExt for Option<StateChangeContext> {
    fn set_msg(&mut self, msg: String) {
        match self {
            Some(ctx) => match ctx {
                StateChangeContext::Disconnect { msg: ref mut inner, .. } => *inner = msg,
                StateChangeContext::Msg(inner) => *inner = msg,
            },
            None => {
                self.replace(StateChangeContext::Msg(msg));
            }
        }
    }
}

impl Joining {
    fn on_join_conf(
        mut self,
        conf: fidl_mlme::JoinConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Authenticating, Idle> {
        match conf.result_code {
            fidl_ieee80211::StatusCode::Success => {
                let (auth_type, sae_password) = match &self.cmd.protection {
                    Protection::Rsna(rsna) => match rsna.supplicant.get_auth_cfg() {
                        auth::Config::Sae { .. } => (fidl_mlme::AuthenticationTypes::Sae, None),
                        auth::Config::DriverSae { password } => {
                            (fidl_mlme::AuthenticationTypes::Sae, Some(password.clone()))
                        }
                        auth::Config::ComputedPsk(_) => {
                            (fidl_mlme::AuthenticationTypes::OpenSystem, None)
                        }
                    },
                    Protection::Wep(ref key) => {
                        install_wep_key(context, self.cmd.bss.bssid.clone(), key);
                        (fidl_mlme::AuthenticationTypes::SharedKey, None)
                    }
                    _ => (fidl_mlme::AuthenticationTypes::OpenSystem, None),
                };

                context.mlme_sink.send(MlmeRequest::Authenticate(fidl_mlme::AuthenticateRequest {
                    peer_sta_address: self.cmd.bss.bssid.0,
                    auth_type,
                    auth_failure_timeout: DEFAULT_AUTH_FAILURE_TIMEOUT,
                    sae_password,
                }));

                state_change_ctx.set_msg(format!("Join succeeded"));
                Ok(Authenticating {
                    cfg: self.cfg,
                    cmd: self.cmd,
                    protection_ie: self.protection_ie,
                })
            }
            other => {
                let msg = format!("Join request failed: {:?}", other);
                warn!("{}", msg);
                report_connect_finished(
                    &mut self.cmd.connect_txn_sink,
                    ConnectResult::Failed(ConnectFailure::JoinFailure(other)),
                );
                state_change_ctx.set_msg(msg);
                Err(Idle { cfg: self.cfg })
            }
        }
    }
}

impl Authenticating {
    fn on_authenticate_conf(
        mut self,
        conf: fidl_mlme::AuthenticateConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Associating, Idle> {
        match conf.result_code {
            fidl_ieee80211::StatusCode::Success => {
                send_mlme_assoc_req(
                    self.cmd.bss.bssid.clone(),
                    &self.protection_ie,
                    &context.mlme_sink,
                );
                state_change_ctx.set_msg(format!("Authenticate succeeded"));
                Ok(Associating { cfg: self.cfg, cmd: self.cmd, protection_ie: self.protection_ie })
            }
            other => {
                let msg = format!("Authenticate request failed: {:?}", other);
                warn!("{}", msg);
                report_connect_finished(
                    &mut self.cmd.connect_txn_sink,
                    ConnectResult::Failed(ConnectFailure::AuthenticationFailure(other)),
                );
                state_change_ctx.set_msg(msg);
                Err(Idle { cfg: self.cfg })
            }
        }
    }

    fn on_deauthenticate_ind(
        mut self,
        ind: fidl_mlme::DeauthenticateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Idle {
        let msg = format!("Authenticate request failed due to spurious deauthentication; reason code: {:?}, locally_initiated: {:?}",
            ind.reason_code, ind.locally_initiated);
        warn!("{}", msg);
        report_connect_finished(
            &mut self.cmd.connect_txn_sink,
            ConnectResult::Failed(ConnectFailure::AuthenticationFailure(
                fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
            )),
        );
        state_change_ctx.set_msg(msg);
        Idle { cfg: self.cfg }
    }

    // Sae management functions

    fn on_pmk_available(&mut self, pmk: fidl_mlme::PmkInfo) -> Result<(), anyhow::Error> {
        let supplicant = match &mut self.cmd.protection {
            Protection::Rsna(rsna) => &mut rsna.supplicant,
            _ => bail!("Unexpected SAE handshake indication"),
        };

        let mut updates = UpdateSink::default();
        supplicant.on_pmk_available(&mut updates, &pmk.pmk[..], &pmk.pmkid[..])?;
        // We don't do anything with these updates right now.
        Ok(())
    }

    fn on_sae_handshake_ind(
        &mut self,
        ind: fidl_mlme::SaeHandshakeIndication,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_handshake_ind(&mut self.cmd.protection, ind, context)
    }

    fn on_sae_frame_rx(
        &mut self,
        frame: fidl_mlme::SaeFrame,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_frame_rx(&mut self.cmd.protection, frame, context)
    }

    fn handle_timeout(
        mut self,
        _event_id: EventId,
        event: Event,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Idle> {
        match process_sae_timeout(&mut self.cmd.protection, self.cmd.bss.bssid, event, context) {
            Ok(()) => Ok(self),
            Err(e) => {
                // An error in handling a timeout means that we may have no way to abort a
                // failed handshake. Drop to idle.
                let msg = format!("failed to handle SAE timeout: {:?}", e);
                error!("{}", msg);
                state_change_ctx.set_msg(msg);
                return Err(Idle { cfg: self.cfg });
            }
        }
    }
}

impl Associating {
    fn on_connect_conf(
        mut self,
        conf: fidl_mlme::ConnectConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Associated, Idle> {
        let auth_method = self.cmd.protection.rsn_auth_method();
        let mut wmm_param = None;
        for (id, body) in ie::Reader::new(&conf.association_ies[..]) {
            if id == ie::Id::VENDOR_SPECIFIC {
                if let Ok(ie::VendorIe::WmmParam(wmm_param_body)) = ie::parse_vendor_ie(body) {
                    match ie::parse_wmm_param(wmm_param_body) {
                        Ok(param) => wmm_param = Some(*param),
                        Err(e) => {
                            warn!(
                                "Fail parsing assoc conf WMM param. Bytes: {:?}. Error: {}",
                                wmm_param_body, e
                            );
                        }
                    }
                }
            }
        }
        let link_state = match conf.result_code {
            fidl_ieee80211::StatusCode::Success => {
                match LinkState::new(self.cmd.protection, context) {
                    Ok(link_state) => link_state,
                    Err(failure_reason) => {
                        let msg = format!("Connect terminated; failed to initialized LinkState");
                        error!("{}", msg);
                        state_change_ctx.set_msg(msg);
                        send_deauthenticate_request(&self.cmd.bss, &context.mlme_sink);
                        report_connect_finished(
                            &mut self.cmd.connect_txn_sink,
                            EstablishRsnaFailure { auth_method, reason: failure_reason }.into(),
                        );
                        return Err(Idle { cfg: self.cfg });
                    }
                }
            }
            other => {
                let msg = format!("Connect request failed: {:?}", other);
                warn!("{}", msg);
                send_deauthenticate_request(&self.cmd.bss, &context.mlme_sink);
                report_connect_finished(
                    &mut self.cmd.connect_txn_sink,
                    ConnectResult::Failed(
                        AssociationFailure {
                            bss_protection: self.cmd.bss.protection(),
                            code: other,
                        }
                        .into(),
                    ),
                );
                state_change_ctx.set_msg(msg);
                return Err(Idle { cfg: self.cfg });
            }
        };
        state_change_ctx.set_msg(format!("Connect succeeded"));

        if let LinkState::LinkUp(_) = link_state {
            report_connect_finished(&mut self.cmd.connect_txn_sink, ConnectResult::Success);
        }

        Ok(Associated {
            cfg: self.cfg,
            connect_txn_sink: self.cmd.connect_txn_sink,
            auth_method,
            last_signal_report_time: now(),
            latest_ap_state: self.cmd.bss,
            link_state,
            protection_ie: self.protection_ie,
            wmm_param,
            last_channel_switch_time: None,
        })
    }

    fn on_associate_conf(
        mut self,
        conf: fidl_mlme::AssociateConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Associated, Idle> {
        let auth_method = self.cmd.protection.rsn_auth_method();
        let wmm_param =
            conf.wmm_param.as_ref().and_then(|p| match ie::parse_wmm_param(&p.bytes[..]) {
                Ok(param) => Some(*param),
                Err(e) => {
                    warn!(
                        "Fail parsing assoc conf WMM param. Bytes: {:?}. Error: {}",
                        &p.bytes[..],
                        e
                    );
                    None
                }
            });
        let link_state = match conf.result_code {
            fidl_ieee80211::StatusCode::Success => {
                match LinkState::new(self.cmd.protection, context) {
                    Ok(link_state) => link_state,
                    Err(failure_reason) => {
                        let msg = format!("Associate terminated; failed to initialized LinkState");
                        error!("{}", msg);
                        state_change_ctx.set_msg(msg);
                        send_deauthenticate_request(&self.cmd.bss, &context.mlme_sink);
                        report_connect_finished(
                            &mut self.cmd.connect_txn_sink,
                            EstablishRsnaFailure { auth_method, reason: failure_reason }.into(),
                        );
                        return Err(Idle { cfg: self.cfg });
                    }
                }
            }
            other => {
                let msg = format!("Associate request failed: {:?}", other);
                warn!("{}", msg);
                send_deauthenticate_request(&self.cmd.bss, &context.mlme_sink);
                report_connect_finished(
                    &mut self.cmd.connect_txn_sink,
                    ConnectResult::Failed(
                        AssociationFailure {
                            bss_protection: self.cmd.bss.protection(),
                            code: other,
                        }
                        .into(),
                    ),
                );
                state_change_ctx.set_msg(msg);
                return Err(Idle { cfg: self.cfg });
            }
        };
        state_change_ctx.set_msg(format!("Associate succeeded"));

        if let LinkState::LinkUp(_) = link_state {
            report_connect_finished(&mut self.cmd.connect_txn_sink, ConnectResult::Success);
        }

        Ok(Associated {
            cfg: self.cfg,
            connect_txn_sink: self.cmd.connect_txn_sink,
            auth_method,
            last_signal_report_time: now(),
            latest_ap_state: self.cmd.bss,
            link_state,
            protection_ie: self.protection_ie,
            wmm_param,
            last_channel_switch_time: None,
        })
    }

    fn on_deauthenticate_ind(
        mut self,
        ind: fidl_mlme::DeauthenticateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Idle {
        let msg = format!("Association request failed due to spurious deauthentication; reason code: {:?}, locally_initiated: {:?}",
                          ind.reason_code, ind.locally_initiated);
        warn!("{}", msg);
        report_connect_finished(
            &mut self.cmd.connect_txn_sink,
            ConnectResult::Failed(
                AssociationFailure {
                    bss_protection: self.cmd.bss.protection(),
                    code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
                }
                .into(),
            ),
        );
        state_change_ctx.set_msg(msg);
        Idle { cfg: self.cfg }
    }

    fn on_disassociate_ind(
        mut self,
        ind: fidl_mlme::DisassociateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Idle {
        let msg = format!("Association request failed due to spurious disassociation; reason code: {:?}, locally_initiated: {:?}",
                          ind.reason_code, ind.locally_initiated);
        warn!("{}", msg);
        send_deauthenticate_request(&self.cmd.bss, &context.mlme_sink);
        report_connect_finished(
            &mut self.cmd.connect_txn_sink,
            ConnectResult::Failed(
                AssociationFailure {
                    bss_protection: self.cmd.bss.protection(),
                    code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
                }
                .into(),
            ),
        );
        state_change_ctx.set_msg(msg);
        Idle { cfg: self.cfg }
    }

    // Sae management functions

    fn on_sae_handshake_ind(
        &mut self,
        ind: fidl_mlme::SaeHandshakeIndication,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_handshake_ind(&mut self.cmd.protection, ind, context)
    }

    fn on_sae_frame_rx(
        &mut self,
        frame: fidl_mlme::SaeFrame,
        context: &mut Context,
    ) -> Result<(), anyhow::Error> {
        process_sae_frame_rx(&mut self.cmd.protection, frame, context)
    }

    fn handle_timeout(
        mut self,
        _event_id: EventId,
        event: Event,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Idle> {
        match process_sae_timeout(&mut self.cmd.protection, self.cmd.bss.bssid, event, context) {
            Ok(()) => Ok(self),
            Err(e) => {
                // An error in handling a timeout means that we may have no way to abort a
                // failed handshake. Drop to idle.
                let msg = format!("failed to handle SAE timeout: {:?}", e);
                error!("{}", msg);
                send_deauthenticate_request(&self.cmd.bss, &context.mlme_sink);
                report_connect_finished(
                    &mut self.cmd.connect_txn_sink,
                    ConnectResult::Failed(
                        AssociationFailure {
                            bss_protection: self.cmd.bss.protection(),
                            code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                        }
                        .into(),
                    ),
                );
                state_change_ctx.set_msg(msg);
                return Err(Idle { cfg: self.cfg });
            }
        }
    }
}

impl Associated {
    fn on_disassociate_ind(
        mut self,
        ind: fidl_mlme::DisassociateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Associating {
        let (mut protection, connected_duration) = self.link_state.disconnect();

        let disconnect_reason = fidl_sme::DisconnectCause {
            mlme_event_name: fidl_sme::DisconnectMlmeEventName::DisassociateIndication,
            reason_code: ind.reason_code,
        };
        let disconnect_source = if ind.locally_initiated {
            fidl_sme::DisconnectSource::Mlme(disconnect_reason)
        } else {
            fidl_sme::DisconnectSource::Ap(disconnect_reason)
        };

        if let Some(_duration) = connected_duration {
            let fidl_disconnect_info =
                fidl_sme::DisconnectInfo { is_sme_reconnecting: true, disconnect_source };
            self.connect_txn_sink
                .send(ConnectTransactionEvent::OnDisconnect { info: fidl_disconnect_info });
        }
        let msg = format!("received DisassociateInd msg; reason code {:?}", ind.reason_code);
        state_change_ctx.replace(match connected_duration {
            Some(_) => StateChangeContext::Disconnect { msg, disconnect_source },
            None => StateChangeContext::Msg(msg),
        });

        // Client is disassociating. The ESS-SA must be kept alive but reset.
        if let Protection::Rsna(rsna) = &mut protection {
            // Reset the state of the ESS-SA and its replay counter to zero per IEEE 802.11-2016 12.7.2.
            rsna.supplicant.reset();
        }

        context.att_id += 1;
        let cmd = ConnectCommand {
            bss: self.latest_ap_state,
            connect_txn_sink: self.connect_txn_sink,
            protection,
        };
        // For SoftMAC, don't send associate request because MLME will automatically reassociate.
        // TODO(fxbug.dev/96011): This will be the case until we define and implement Reconnect API.
        if let fidl_common::MacImplementationType::Fullmac =
            context.mac_sublayer_support.device.mac_implementation_type
        {
            send_mlme_assoc_req(cmd.bss.bssid.clone(), &self.protection_ie, &context.mlme_sink);
        }
        Associating { cfg: self.cfg, cmd, protection_ie: self.protection_ie }
    }

    fn on_deauthenticate_ind(
        mut self,
        ind: fidl_mlme::DeauthenticateIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Idle {
        let (_, connected_duration) = self.link_state.disconnect();

        let disconnect_reason = fidl_sme::DisconnectCause {
            mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
            reason_code: ind.reason_code,
        };
        let disconnect_source = if ind.locally_initiated {
            fidl_sme::DisconnectSource::Mlme(disconnect_reason)
        } else {
            fidl_sme::DisconnectSource::Ap(disconnect_reason)
        };

        match connected_duration {
            Some(_duration) => {
                let fidl_disconnect_info = fidl_sme::DisconnectInfo {
                    is_sme_reconnecting: false,
                    disconnect_source: disconnect_source.into(),
                };
                self.connect_txn_sink
                    .send(ConnectTransactionEvent::OnDisconnect { info: fidl_disconnect_info });
            }
            None => {
                let connect_result = EstablishRsnaFailure {
                    auth_method: self.auth_method,
                    reason: EstablishRsnaFailureReason::InternalError,
                }
                .into();
                report_connect_finished(&mut self.connect_txn_sink, connect_result);
            }
        }

        state_change_ctx.replace(StateChangeContext::Disconnect {
            msg: format!("received DeauthenticateInd msg; reason code {:?}", ind.reason_code),
            disconnect_source,
        });
        Idle { cfg: self.cfg }
    }

    fn process_link_state_update<U, H>(
        mut self,
        update: U,
        update_handler: H,
        context: &mut Context,
        state_change_ctx: &mut Option<StateChangeContext>,
    ) -> Result<Self, Idle>
    where
        H: Fn(
            LinkState,
            U,
            &BssDescription,
            &mut Option<StateChangeContext>,
            &mut Context,
        ) -> Result<LinkState, EstablishRsnaFailureReason>,
    {
        let was_establishing_rsna = match &self.link_state {
            LinkState::EstablishingRsna(_) => true,
            LinkState::Init(_) | LinkState::LinkUp(_) => false,
        };

        let link_state = match update_handler(
            self.link_state,
            update,
            &self.latest_ap_state,
            state_change_ctx,
            context,
        ) {
            Ok(link_state) => link_state,
            Err(failure_reason) => {
                report_connect_finished(
                    &mut self.connect_txn_sink,
                    EstablishRsnaFailure { auth_method: self.auth_method, reason: failure_reason }
                        .into(),
                );
                send_deauthenticate_request(&self.latest_ap_state, &context.mlme_sink);
                return Err(Idle { cfg: self.cfg });
            }
        };

        if let LinkState::LinkUp(_) = link_state {
            if was_establishing_rsna {
                report_connect_finished(&mut self.connect_txn_sink, ConnectResult::Success);
            }
        }

        Ok(Self { link_state, ..self })
    }

    fn on_eapol_ind(
        self,
        ind: fidl_mlme::EapolIndication,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Idle> {
        // Ignore unexpected EAPoL frames.
        if !self.latest_ap_state.needs_eapol_exchange() {
            return Ok(self);
        }

        // Reject EAPoL frames from other BSS.
        if ind.src_addr != self.latest_ap_state.bssid.0 {
            let eapol_pdu = &ind.data[..];
            inspect_log!(context.inspect.rsn_events.lock(), {
                rx_eapol_frame: InspectBytes(&eapol_pdu),
                foreign_bssid: ind.src_addr.to_mac_string(),
                foreign_bssid_hash: context.inspect.hasher.hash_mac_addr(&ind.src_addr),
                current_bssid: self.latest_ap_state.bssid.0.to_mac_string(),
                current_bssid_hash: context.inspect.hasher.hash_mac_addr(&self.latest_ap_state.bssid.0),
                status: "rejected (foreign BSS)",
            });
            return Ok(self);
        }

        self.process_link_state_update(ind, LinkState::on_eapol_ind, context, state_change_ctx)
    }

    fn on_eapol_conf(
        self,
        resp: fidl_mlme::EapolConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Idle> {
        self.process_link_state_update(resp, LinkState::on_eapol_conf, context, state_change_ctx)
    }

    fn on_set_keys_conf(
        self,
        conf: fidl_mlme::SetKeysConfirm,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Idle> {
        self.process_link_state_update(conf, LinkState::on_set_keys_conf, context, state_change_ctx)
    }

    fn on_channel_switched(&mut self, info: fidl_internal::ChannelSwitchInfo) {
        self.connect_txn_sink.send(ConnectTransactionEvent::OnChannelSwitched { info });
        self.latest_ap_state.channel.primary = info.new_channel;
        self.last_channel_switch_time.replace(now());
    }

    fn on_wmm_status_resp(
        &mut self,
        status: zx::zx_status_t,
        resp: fidl_internal::WmmStatusResponse,
    ) {
        if status == zx::sys::ZX_OK {
            let wmm_param = self.wmm_param.get_or_insert_with(|| ie::WmmParam::default());
            let mut wmm_info = wmm_param.wmm_info.ap_wmm_info();
            wmm_info.set_uapsd(resp.apsd);
            wmm_param.wmm_info.0 = wmm_info.0;
            update_wmm_ac_param(&mut wmm_param.ac_be_params, &resp.ac_be_params);
            update_wmm_ac_param(&mut wmm_param.ac_bk_params, &resp.ac_bk_params);
            update_wmm_ac_param(&mut wmm_param.ac_vo_params, &resp.ac_vo_params);
            update_wmm_ac_param(&mut wmm_param.ac_vi_params, &resp.ac_vi_params);
        }
    }

    fn handle_timeout(
        mut self,
        event_id: EventId,
        event: Event,
        state_change_ctx: &mut Option<StateChangeContext>,
        context: &mut Context,
    ) -> Result<Self, Idle> {
        match self.link_state.handle_timeout(event_id, event, state_change_ctx, context) {
            Ok(link_state) => Ok(Associated { link_state, ..self }),
            Err(failure_reason) => {
                report_connect_finished(
                    &mut self.connect_txn_sink,
                    EstablishRsnaFailure { auth_method: self.auth_method, reason: failure_reason }
                        .into(),
                );
                send_deauthenticate_request(&self.latest_ap_state, &context.mlme_sink);
                Err(Idle { cfg: self.cfg })
            }
        }
    }
}

impl ClientState {
    pub fn new(cfg: ClientConfig) -> Self {
        Self::from(State::new(Idle { cfg }))
    }

    fn state_name(&self) -> &'static str {
        match self {
            Self::Idle(_) => IDLE_STATE,
            Self::Joining(_) => JOINING_STATE,
            Self::Authenticating(_) => AUTHENTICATING_STATE,
            Self::Associating(_) => ASSOCIATING_STATE,
            Self::Associated(state) => match state.link_state {
                LinkState::EstablishingRsna(_) => RSNA_STATE,
                LinkState::LinkUp(_) => LINK_UP_STATE,
                _ => unreachable!(),
            },
        }
    }

    pub fn on_mlme_event(self, event: MlmeEvent, context: &mut Context) -> Self {
        let start_state = self.state_name();
        let mut state_change_ctx: Option<StateChangeContext> = None;
        let is_softmac = context.mac_sublayer_support.device.mac_implementation_type
            == fidl_common::MacImplementationType::Softmac;

        let new_state = match self {
            Self::Idle(_) => {
                match event {
                    MlmeEvent::OnWmmStatusResp { .. } => (),
                    _ => warn!("Unexpected MLME message while Idle: {:?}", mlme_event_name(&event)),
                }
                self
            }
            Self::Joining(state) => match event {
                MlmeEvent::JoinConf { resp } => {
                    let (transition, joining) = state.release_data();
                    match joining.on_join_conf(resp, &mut state_change_ctx, context) {
                        Ok(authenticating) => transition.to(authenticating).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                _ => state.into(),
            },
            Self::Authenticating(state) => match event {
                MlmeEvent::AuthenticateConf { resp } => {
                    let (transition, authenticating) = state.release_data();
                    match authenticating.on_authenticate_conf(resp, &mut state_change_ctx, context)
                    {
                        Ok(associating) => transition.to(associating).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                MlmeEvent::OnPmkAvailable { info } => {
                    let (transition, mut authenticating) = state.release_data();
                    if let Err(e) = authenticating.on_pmk_available(info) {
                        error!("Failed to process OnPmkAvailable: {:?}", e);
                    }
                    transition.to(authenticating).into()
                }
                MlmeEvent::OnSaeHandshakeInd { ind } => {
                    let (transition, mut authenticating) = state.release_data();
                    if let Err(e) = authenticating.on_sae_handshake_ind(ind, context) {
                        error!("Failed to process SaeHandshakeInd: {:?}", e);
                    }
                    transition.to(authenticating).into()
                }
                MlmeEvent::OnSaeFrameRx { frame } => {
                    let (transition, mut authenticating) = state.release_data();
                    if let Err(e) = authenticating.on_sae_frame_rx(frame, context) {
                        error!("Failed to process SaeFrameRx: {:?}", e);
                    }
                    transition.to(authenticating).into()
                }
                MlmeEvent::DeauthenticateInd { ind } => {
                    let (transition, authenticating) = state.release_data();
                    let idle = authenticating.on_deauthenticate_ind(ind, &mut state_change_ctx);
                    transition.to(idle).into()
                }
                _ => state.into(),
            },
            Self::Associating(state) => match event {
                MlmeEvent::ConnectConf { resp } if is_softmac => {
                    let (transition, associating) = state.release_data();
                    match associating.on_connect_conf(resp, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                MlmeEvent::AssociateConf { resp } => {
                    let (transition, associating) = state.release_data();
                    match associating.on_associate_conf(resp, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                MlmeEvent::DeauthenticateInd { ind } => {
                    let (transition, associating) = state.release_data();
                    let idle = associating.on_deauthenticate_ind(ind, &mut state_change_ctx);
                    transition.to(idle).into()
                }
                MlmeEvent::DisassociateInd { ind } => {
                    let (transition, associating) = state.release_data();
                    let idle = associating.on_disassociate_ind(ind, &mut state_change_ctx, context);
                    transition.to(idle).into()
                }
                MlmeEvent::OnSaeHandshakeInd { ind } => {
                    let (transition, mut associating) = state.release_data();
                    if let Err(e) = associating.on_sae_handshake_ind(ind, context) {
                        error!("Failed to process SaeHandshakeInd: {:?}", e);
                    }
                    transition.to(associating).into()
                }
                MlmeEvent::OnSaeFrameRx { frame } => {
                    let (transition, mut associating) = state.release_data();
                    if let Err(e) = associating.on_sae_frame_rx(frame, context) {
                        error!("Failed to process SaeFrameRx: {:?}", e);
                    }
                    transition.to(associating).into()
                }
                _ => state.into(),
            },
            Self::Associated(mut state) => match event {
                MlmeEvent::DisassociateInd { ind } => {
                    let (transition, associated) = state.release_data();
                    let associating =
                        associated.on_disassociate_ind(ind, &mut state_change_ctx, context);
                    transition.to(associating).into()
                }
                MlmeEvent::DeauthenticateInd { ind } => {
                    let (transition, associated) = state.release_data();
                    let idle = associated.on_deauthenticate_ind(ind, &mut state_change_ctx);
                    transition.to(idle).into()
                }
                MlmeEvent::SignalReport { ind } => {
                    if matches!(state.link_state, LinkState::LinkUp(_)) {
                        state
                            .connect_txn_sink
                            .send(ConnectTransactionEvent::OnSignalReport { ind });
                    }
                    state.latest_ap_state.rssi_dbm = ind.rssi_dbm;
                    state.latest_ap_state.snr_db = ind.snr_db;
                    state.last_signal_report_time = now();
                    state.into()
                }
                MlmeEvent::EapolInd { ind } => {
                    let (transition, associated) = state.release_data();
                    match associated.on_eapol_ind(ind, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                MlmeEvent::EapolConf { resp } => {
                    let (transition, associated) = state.release_data();
                    match associated.on_eapol_conf(resp, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                MlmeEvent::SetKeysConf { conf } => {
                    let (transition, associated) = state.release_data();
                    match associated.on_set_keys_conf(conf, &mut state_change_ctx, context) {
                        Ok(associated) => transition.to(associated).into(),
                        Err(idle) => transition.to(idle).into(),
                    }
                }
                MlmeEvent::OnChannelSwitched { info } => {
                    state.on_channel_switched(info);
                    state.into()
                }
                MlmeEvent::OnWmmStatusResp { status, resp } => {
                    state.on_wmm_status_resp(status, resp);
                    state.into()
                }
                _ => state.into(),
            },
        };

        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    pub fn handle_timeout(self, event_id: EventId, event: Event, context: &mut Context) -> Self {
        let start_state = self.state_name();
        let mut state_change_ctx: Option<StateChangeContext> = None;

        let new_state = match self {
            Self::Authenticating(state) => {
                let (transition, authenticating) = state.release_data();
                match authenticating.handle_timeout(event_id, event, &mut state_change_ctx, context)
                {
                    Ok(authenticating) => transition.to(authenticating).into(),
                    Err(idle) => transition.to(idle).into(),
                }
            }
            Self::Associating(state) => {
                let (transition, associating) = state.release_data();
                match associating.handle_timeout(event_id, event, &mut state_change_ctx, context) {
                    Ok(associating) => transition.to(associating).into(),
                    Err(idle) => transition.to(idle).into(),
                }
            }
            Self::Associated(state) => {
                let (transition, associated) = state.release_data();
                match associated.handle_timeout(event_id, event, &mut state_change_ctx, context) {
                    Ok(associated) => transition.to(associated).into(),
                    Err(idle) => transition.to(idle).into(),
                }
            }
            _ => self,
        };

        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    pub fn connect(self, cmd: ConnectCommand, context: &mut Context) -> Self {
        // For SoftMAC, compatibility of capabilities is checked in MLME, so no need to do it here.
        // TODO(fxbug.dev/96668): We likely don't need this check for FullMAC, either. We can
        //                        remove it when SME state machine is fully transitioned to use
        //                        the new Connect API.
        let is_softmac = context.mac_sublayer_support.device.mac_implementation_type
            == fidl_common::MacImplementationType::Softmac;
        if !is_softmac {
            if let Err(e) =
                derive_join_capabilities(cmd.bss.channel, cmd.bss.rates(), &context.device_info)
            {
                error!("Failed building join capabilities: {}", e);
                return self;
            }
        }

        // Derive RSN (for WPA2) or Vendor IEs (for WPA1) or neither(WEP/non-protected).
        let protection_ie = match build_protection_ie(&cmd.protection) {
            Ok(ie) => ie,
            Err(e) => {
                error!("Failed to build protection IEs: {}", e);
                return self;
            }
        };

        let start_state = self.state_name();
        let cfg = self.disconnect_internal(context);

        let selected_bss = cmd.bss.clone();

        if is_softmac {
            let (auth_type, sae_password) = match &cmd.protection {
                Protection::Rsna(rsna) => match rsna.supplicant.get_auth_cfg() {
                    auth::Config::Sae { .. } => (fidl_mlme::AuthenticationTypes::Sae, vec![]),
                    auth::Config::DriverSae { password } => {
                        (fidl_mlme::AuthenticationTypes::Sae, password.clone())
                    }
                    auth::Config::ComputedPsk(_) => {
                        (fidl_mlme::AuthenticationTypes::OpenSystem, vec![])
                    }
                },
                Protection::Wep(ref key) => {
                    install_wep_key(context, cmd.bss.bssid.clone(), key);
                    (fidl_mlme::AuthenticationTypes::SharedKey, vec![])
                }
                _ => (fidl_mlme::AuthenticationTypes::OpenSystem, vec![]),
            };
            let security_ie = match protection_ie.as_ref() {
                Some(ProtectionIe::Rsne(v)) => v.to_vec(),
                Some(ProtectionIe::VendorIes(v)) => v.to_vec(),
                None => vec![],
            };
            context.mlme_sink.send(MlmeRequest::Connect(fidl_mlme::ConnectRequest {
                selected_bss: (*cmd.bss).clone().into(),
                connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
                auth_type,
                sae_password,
                security_ie,
            }));
        } else {
            context.mlme_sink.send(MlmeRequest::Join(fidl_mlme::JoinRequest {
                selected_bss: (*selected_bss).into(),
                join_failure_timeout: DEFAULT_JOIN_FAILURE_TIMEOUT,
                nav_sync_delay: 0,
                op_rates: vec![],
            }));
        }
        context.att_id += 1;

        let msg = connect_cmd_inspect_summary(&cmd);
        inspect_log!(context.inspect.state_events.lock(), {
            from: start_state,
            to: if is_softmac { ASSOCIATING_STATE } else { JOINING_STATE },
            ctx: msg,
            bssid: cmd.bss.bssid.0.to_mac_string(),
            bssid_hash: context.inspect.hasher.hash_mac_addr(&cmd.bss.bssid.0),
            ssid: cmd.bss.ssid.to_string(),
            ssid_hash: context.inspect.hasher.hash_ssid(&cmd.bss.ssid),
        });
        let state = Self::new(cfg.clone());
        match state {
            Self::Idle(state) => {
                if is_softmac {
                    state.transition_to(Associating { cfg, cmd, protection_ie }).into()
                } else {
                    state.transition_to(Joining { cfg, cmd, protection_ie }).into()
                }
            }
            _ => unreachable!(),
        }
    }

    pub fn disconnect(
        mut self,
        context: &mut Context,
        user_disconnect_reason: fidl_sme::UserDisconnectReason,
    ) -> Self {
        let mut disconnected_from_link_up = false;
        let disconnect_source = fidl_sme::DisconnectSource::User(user_disconnect_reason);
        if let Self::Associated(state) = &mut self {
            if let LinkState::LinkUp(_link_up) = &state.link_state {
                disconnected_from_link_up = true;
                let fidl_disconnect_info =
                    fidl_sme::DisconnectInfo { is_sme_reconnecting: false, disconnect_source };
                state
                    .connect_txn_sink
                    .send(ConnectTransactionEvent::OnDisconnect { info: fidl_disconnect_info });
            }
        }
        let start_state = self.state_name();
        let new_state = Self::new(self.disconnect_internal(context));

        let msg =
            format!("received disconnect command from user; reason {:?}", user_disconnect_reason);
        let state_change_ctx = Some(if disconnected_from_link_up {
            StateChangeContext::Disconnect { msg, disconnect_source }
        } else {
            StateChangeContext::Msg(msg)
        });
        log_state_change(start_state, &new_state, state_change_ctx, context);
        new_state
    }

    fn disconnect_internal(self, context: &mut Context) -> ClientConfig {
        match self {
            Self::Idle(state) => state.cfg,
            Self::Joining(state) => {
                let (_, mut state) = state.release_data();
                report_connect_finished(&mut state.cmd.connect_txn_sink, ConnectResult::Canceled);
                state.cfg
            }
            Self::Authenticating(state) => {
                let (_, mut state) = state.release_data();
                report_connect_finished(&mut state.cmd.connect_txn_sink, ConnectResult::Canceled);
                state.cfg
            }
            Self::Associating(state) => {
                let (_, mut state) = state.release_data();
                report_connect_finished(&mut state.cmd.connect_txn_sink, ConnectResult::Canceled);
                send_deauthenticate_request(&state.cmd.bss, &context.mlme_sink);
                state.cfg
            }
            Self::Associated(state) => {
                send_deauthenticate_request(&state.latest_ap_state, &context.mlme_sink);
                state.cfg
            }
        }
    }

    // Cancel any connect that is in progress. No-op if client is already idle or connected.
    pub fn cancel_ongoing_connect(self, context: &mut Context) -> Self {
        // Only move to idle if client is not already connected. Technically, SME being in
        // transition state does not necessarily mean that a (manual) connect attempt is
        // in progress (since DisassociateInd moves SME to transition state). However, the
        // main thing we are concerned about is that we don't disconnect from an already
        // connected state until the new connect attempt succeeds in selecting BSS.
        if self.in_transition_state() {
            Self::new(self.disconnect_internal(context))
        } else {
            self
        }
    }

    fn in_transition_state(&self) -> bool {
        match self {
            Self::Idle(_) => false,
            Self::Associated(state) => match state.link_state {
                LinkState::LinkUp { .. } => false,
                _ => true,
            },
            _ => true,
        }
    }

    pub fn status(&self) -> ClientSmeStatus {
        match self {
            Self::Idle(_) => ClientSmeStatus::Idle,
            Self::Joining(joining) => ClientSmeStatus::Connecting(joining.cmd.bss.ssid.clone()),
            Self::Authenticating(authenticating) => {
                ClientSmeStatus::Connecting(authenticating.cmd.bss.ssid.clone())
            }
            Self::Associating(associating) => {
                ClientSmeStatus::Connecting(associating.cmd.bss.ssid.clone())
            }
            Self::Associated(associated) => match associated.link_state {
                LinkState::EstablishingRsna { .. } => {
                    ClientSmeStatus::Connecting(associated.latest_ap_state.ssid.clone())
                }
                LinkState::LinkUp { .. } => {
                    let latest_ap_state = &associated.latest_ap_state;
                    ClientSmeStatus::Connected(ServingApInfo {
                        bssid: latest_ap_state.bssid,
                        ssid: latest_ap_state.ssid.clone(),
                        rssi_dbm: latest_ap_state.rssi_dbm,
                        snr_db: latest_ap_state.snr_db,
                        signal_report_time: associated.last_signal_report_time,
                        channel: latest_ap_state.channel.clone(),
                        protection: latest_ap_state.protection(),
                        ht_cap: latest_ap_state.raw_ht_cap(),
                        vht_cap: latest_ap_state.raw_vht_cap(),
                        probe_resp_wsc: latest_ap_state.probe_resp_wsc(),
                        wmm_param: associated.wmm_param,
                    })
                }
                _ => unreachable!(),
            },
        }
    }
}

fn update_wmm_ac_param(ac_params: &mut ie::WmmAcParams, update: &fidl_internal::WmmAcParams) {
    ac_params.aci_aifsn.set_aifsn(update.aifsn);
    ac_params.aci_aifsn.set_acm(update.acm);
    ac_params.ecw_min_max.set_ecw_min(update.ecw_min);
    ac_params.ecw_min_max.set_ecw_max(update.ecw_max);
    ac_params.txop_limit = update.txop_limit;
}

fn process_sae_updates(updates: UpdateSink, peer_sta_address: MacAddr, context: &mut Context) {
    for update in updates {
        match update {
            SecAssocUpdate::TxSaeFrame(frame) => {
                context.mlme_sink.send(MlmeRequest::SaeFrameTx(frame));
            }
            SecAssocUpdate::SaeAuthStatus(status) => context.mlme_sink.send(
                MlmeRequest::SaeHandshakeResp(fidl_mlme::SaeHandshakeResponse {
                    peer_sta_address,
                    status_code: match status {
                        AuthStatus::Success => fidl_ieee80211::StatusCode::Success,
                        AuthStatus::Rejected => {
                            fidl_ieee80211::StatusCode::RefusedReasonUnspecified
                        }
                        AuthStatus::InternalError => {
                            fidl_ieee80211::StatusCode::RefusedReasonUnspecified
                        }
                    },
                }),
            ),
            SecAssocUpdate::ScheduleSaeTimeout(id) => {
                context.timer.schedule(event::SaeTimeout(id));
            }
            _ => (),
        }
    }
}

fn process_sae_handshake_ind(
    protection: &mut Protection,
    ind: fidl_mlme::SaeHandshakeIndication,
    context: &mut Context,
) -> Result<(), anyhow::Error> {
    let supplicant = match protection {
        Protection::Rsna(rsna) => &mut rsna.supplicant,
        _ => bail!("Unexpected SAE handshake indication"),
    };

    let mut updates = UpdateSink::default();
    supplicant.on_sae_handshake_ind(&mut updates)?;
    process_sae_updates(updates, ind.peer_sta_address, context);
    Ok(())
}

fn process_sae_frame_rx(
    protection: &mut Protection,
    frame: fidl_mlme::SaeFrame,
    context: &mut Context,
) -> Result<(), anyhow::Error> {
    let peer_sta_address = frame.peer_sta_address.clone();
    let supplicant = match protection {
        Protection::Rsna(rsna) => &mut rsna.supplicant,
        _ => bail!("Unexpected SAE frame received"),
    };

    let mut updates = UpdateSink::default();
    supplicant.on_sae_frame_rx(&mut updates, frame)?;
    process_sae_updates(updates, peer_sta_address, context);
    Ok(())
}

fn process_sae_timeout(
    protection: &mut Protection,
    bssid: Bssid,
    event: Event,
    context: &mut Context,
) -> Result<(), anyhow::Error> {
    match event {
        Event::SaeTimeout(timer) => {
            let supplicant = match protection {
                Protection::Rsna(rsna) => &mut rsna.supplicant,
                // Ignore timeouts if we're not using SAE.
                _ => return Ok(()),
            };

            let mut updates = UpdateSink::default();
            supplicant.on_sae_timeout(&mut updates, timer.0)?;
            process_sae_updates(updates, bssid.0, context);
        }
        _ => (),
    }
    Ok(())
}

fn log_state_change(
    start_state: &str,
    new_state: &ClientState,
    state_change_ctx: Option<StateChangeContext>,
    context: &mut Context,
) {
    if start_state == new_state.state_name() && state_change_ctx.is_none() {
        return;
    }

    match state_change_ctx {
        Some(inner) => match inner {
            StateChangeContext::Disconnect { msg, disconnect_source } => {
                // Only log the disconnect source if an operation had an effect of moving from
                // non-idle state to idle state.
                if start_state != IDLE_STATE {
                    info!(
                        "{} => {}, ctx: `{}`, disconnect_source: {:?}",
                        start_state,
                        new_state.state_name(),
                        msg,
                        disconnect_source,
                    );
                }

                inspect_log!(context.inspect.state_events.lock(), {
                    from: start_state,
                    to: new_state.state_name(),
                    ctx: msg,
                });
            }
            StateChangeContext::Msg(msg) => {
                inspect_log!(context.inspect.state_events.lock(), {
                    from: start_state,
                    to: new_state.state_name(),
                    ctx: msg,
                });
            }
        },
        None => {
            inspect_log!(context.inspect.state_events.lock(), {
                from: start_state,
                to: new_state.state_name(),
            });
        }
    }
}

fn install_wep_key(context: &mut Context, bssid: Bssid, key: &WepKey) {
    let cipher_suite = match key {
        WepKey::Wep40(_) => cipher::WEP_40,
        WepKey::Wep104(_) => cipher::WEP_104,
    };
    // unwrap() is safe, OUI is defined in RSN and always compatible with ciphers.
    let cipher = cipher::Cipher::new_dot11(cipher_suite);
    inspect_log!(context.inspect.rsn_events.lock(), {
        derived_key: "WEP",
        cipher: format!("{:?}", cipher),
        key_index: 0,
    });
    let request = MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest {
        keylist: vec![fidl_mlme::SetKeyDescriptor {
            key_type: fidl_mlme::KeyType::Pairwise,
            key: key.clone().into(),
            key_id: 0,
            address: bssid.0,
            cipher_suite_oui: OUI.into(),
            cipher_suite_type: cipher_suite,
            rsc: 0,
        }],
    });
    context.mlme_sink.send(request)
}

/// Custom logging for ConnectCommand because its normal full debug string is too large, and we
/// want to reduce how much we log in memory for Inspect. Additionally, in the future, we'd need
/// to anonymize information like BSSID and SSID.
fn connect_cmd_inspect_summary(cmd: &ConnectCommand) -> String {
    let bss = &cmd.bss;
    format!(
        "ConnectCmd {{ \
         capability_info: {capability_info:?}, rates: {rates:?}, \
         protected: {protected:?}, channel: {channel}, \
         rssi: {rssi:?}, ht_cap: {ht_cap:?}, ht_op: {ht_op:?}, \
         vht_cap: {vht_cap:?}, vht_op: {vht_op:?} }}",
        capability_info = bss.capability_info,
        rates = bss.rates(),
        protected = bss.rsne().is_some(),
        channel = bss.channel,
        rssi = bss.rssi_dbm,
        ht_cap = bss.ht_cap().is_some(),
        ht_op = bss.ht_op().is_some(),
        vht_cap = bss.vht_cap().is_some(),
        vht_op = bss.vht_op().is_some()
    )
}

fn send_deauthenticate_request(current_bss: &BssDescription, mlme_sink: &MlmeSink) {
    mlme_sink.send(MlmeRequest::Deauthenticate(fidl_mlme::DeauthenticateRequest {
        peer_sta_address: current_bss.bssid.0,
        reason_code: fidl_ieee80211::ReasonCode::StaLeaving,
    }));
}

fn send_mlme_assoc_req(bssid: Bssid, protection_ie: &Option<ProtectionIe>, mlme_sink: &MlmeSink) {
    let (rsne, vendor_ies) = match protection_ie.as_ref() {
        Some(ProtectionIe::Rsne(vec)) => (Some(vec.to_vec()), None),
        Some(ProtectionIe::VendorIes(vec)) => (None, Some(vec.to_vec())),
        None => (None, None),
    };
    let req = fidl_mlme::AssociateRequest {
        peer_sta_address: bssid.0,
        capability_info: 0,
        rates: vec![],
        // TODO(fxbug.dev/43938): populate `qos_capable` field from device info
        qos_capable: false,
        qos_info: 0,
        ht_cap: None,
        vht_cap: None,
        rsne,
        vendor_ies,
    };
    mlme_sink.send(MlmeRequest::Associate(req))
}

fn now() -> zx::Time {
    zx::Time::get_monotonic()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::format_err;
    use fuchsia_inspect::{assert_data_tree, testing::AnyProperty, Inspector};
    use futures::channel::mpsc;
    use ieee80211::Ssid;
    use link_state::{EstablishingRsna, LinkUp};
    use std::{convert::TryFrom, sync::Arc};
    use wlan_common::{
        assert_variant,
        bss::Protection as BssProtection,
        channel::{Cbw, Channel},
        fake_bss_description,
        hasher::WlanHasher,
        ie::{
            fake_ies::{fake_probe_resp_wsc_ie_bytes, get_vendor_ie_bytes_for_wsc_ie},
            rsn::rsne::Rsne,
        },
        test_utils::{
            fake_features::{fake_mac_sublayer_support, fake_security_support},
            fake_stas::IesOverrides,
        },
        timer,
    };
    use wlan_rsn::{key::exchange::Key, rsna::SecAssocStatus};
    use wlan_rsn::{
        rsna::{SecAssocUpdate, UpdateSink},
        NegotiatedProtection,
    };

    use crate::client::test_utils::{
        create_assoc_conf, create_auth_conf, create_connect_conf, create_join_conf,
        create_on_wmm_status_resp, expect_stream_empty, fake_wmm_param, mock_psk_supplicant,
        MockSupplicant, MockSupplicantController,
    };
    use crate::client::{inspect, rsn::Rsna, ConnectTransactionStream, TimeStream};
    use crate::test_utils::make_wpa1_ie;

    use crate::{test_utils, MlmeStream};

    #[test]
    fn connect_happy_path_unprotected() {
        let mut h = TestHelper::new();
        // TODO(fxbug.dev/96668) - Once FullMAC is transitioned to using Connect API, setting this
        //                         flag will no longer be necessary.
        h.context.mac_sublayer_support.device.mac_implementation_type =
            fidl_common::MacImplementationType::Softmac;

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_one();
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(req))) => {
            assert_eq!(req, fidl_mlme::ConnectRequest {
                selected_bss: bss.clone().into(),
                connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
                sae_password: vec![],
                security_ie: vec![],
            });
        });

        // (mlme->sme) Send a ConnectConf as a response
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(connect_conf, &mut h.context);

        // User should be notified that we are connected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });
    }

    #[test]
    fn connect_happy_path_protected() {
        let mut h = TestHelper::new();
        // TODO(fxbug.dev/96668) - Once FullMAC is transitioned to using Connect API, setting this
        //                         flag will no longer be necessary.
        h.context.mac_sublayer_support.device.mac_implementation_type =
            fidl_common::MacImplementationType::Softmac;
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(req))) => {
            assert_eq!(req, fidl_mlme::ConnectRequest {
                selected_bss: bss.clone().into(),
                connect_failure_timeout: DEFAULT_JOIN_AUTH_ASSOC_FAILURE_TIMEOUT,
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
                sae_password: vec![],
                security_ie: vec![
                    0x30, 18, // Element header
                    1, 0, // Version
                    0x00, 0x0F, 0xAC, 4, // Group Cipher: CCMP-128
                    1, 0, 0x00, 0x0F, 0xAC, 4, // 1 Pairwise Cipher: CCMP-128
                    1, 0, 0x00, 0x0F, 0xAC, 2, // 1 AKM: PSK
                ],
            });
        });

        // (mlme->sme) Send a ConnectConf as a response
        let connect_conf = create_connect_conf(bss.bssid, fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(connect_conf, &mut h.context);

        assert!(suppl_mock.is_supplicant_started());

        // (mlme->sme) Send an EapolInd, mock supplicant with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        };
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_eapol_req(&mut h.mlme_stream, bss.bssid);

        // (mlme->sme) Send an EapolInd, mock supplicant with keys
        let ptk = SecAssocUpdate::Key(Key::Ptk(test_utils::ptk()));
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::gtk()));
        let state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![ptk, gtk]);

        expect_set_ptk(&mut h.mlme_stream, bss.bssid);
        expect_set_gtk(&mut h.mlme_stream);

        let state = on_set_keys_conf(state, &mut h, vec![0, 2]);

        // (mlme->sme) Send an EapolInd, mock supplicant with completion status
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        let _state = on_eapol_ind(state, &mut h, bss.bssid, &suppl_mock, vec![update]);

        expect_set_ctrl_port(&mut h.mlme_stream, bss.bssid, fidl_mlme::ControlledPortState::Open);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });
    }

    #[test]
    fn connect_happy_path_wmm() {
        let mut h = TestHelper::new();
        // TODO(fxbug.dev/96668) - Once FullMAC is transitioned to using Connect API, setting this
        //                         flag will no longer be necessary.
        h.context.mac_sublayer_support.device.mac_implementation_type =
            fidl_common::MacImplementationType::Softmac;

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_one();
        let bss = (*command.bss).clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        // (sme->mlme) Expect a ConnectRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Connect(_req))));

        // (mlme->sme) Send a ConnectConf as a response
        let connect_conf = fidl_mlme::MlmeEvent::ConnectConf {
            resp: fidl_mlme::ConnectConfirm {
                peer_sta_address: bss.bssid.0,
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 42,
                association_ies: vec![
                    0xdd, 0x18, // Vendor IE header
                    0x00, 0x50, 0xf2, 0x02, 0x01, 0x01, // WMM Param header
                    0x80, // Qos Info - U-ASPD enabled
                    0x00, // reserved
                    0x03, 0xa4, 0x00, 0x00, // Best effort AC params
                    0x27, 0xa4, 0x00, 0x00, // Background AC params
                    0x42, 0x43, 0x5e, 0x00, // Video AC params
                    0x62, 0x32, 0x2f, 0x00, // Voice AC params
                ],
            },
        };
        let _state = state.on_mlme_event(connect_conf, &mut h.context);

        // User should be notified that we are connected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });
    }

    #[test]
    fn associate_happy_path_unprotected() {
        let mut h = TestHelper::new();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_one();
        let bssid = command.bss.bssid.clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        expect_join_request(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send a JoinConf as a response
        let join_conf = create_join_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(join_conf, &mut h.context);

        expect_auth_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AuthenticateConf as a response
        let auth_conf = create_auth_conf(bssid.clone(), fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(auth_conf, &mut h.context);

        expect_assoc_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(assoc_conf, &mut h.context);

        // User should be notified that we are connected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });
    }

    #[test]
    fn connect_to_wep_network() {
        let mut h = TestHelper::new();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wep();
        let bssid = command.bss.bssid.clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        expect_join_request(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send a JoinConf as a response
        let join_conf = create_join_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(join_conf, &mut h.context);

        // (sme->mlme) Expect an SetKeysRequest
        expect_set_wep_key(&mut h.mlme_stream, bssid, vec![3; 5]);
        // (sme->mlme) Expect an AuthenticateRequest
        assert_variant!(&mut h.mlme_stream.try_next(),
            Ok(Some(MlmeRequest::Authenticate(req))) => {
                assert_eq!(fidl_mlme::AuthenticationTypes::SharedKey, req.auth_type);
                assert_eq!(bssid.0, req.peer_sta_address);
            }
        );

        // (mlme->sme) Send an AuthenticateConf as a response
        let auth_conf = create_auth_conf(bssid.clone(), fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(auth_conf, &mut h.context);

        expect_assoc_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(assoc_conf, &mut h.context);

        // User should be notified that we are connected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });
    }

    #[test]
    fn connect_to_wpa1_network() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wpa1(supplicant);
        let bssid = command.bss.bssid.clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        expect_join_request(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send a JoinConf as a response
        let join_conf = create_join_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(join_conf, &mut h.context);

        expect_auth_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AuthenticateConf as a response
        let auth_conf = create_auth_conf(bssid.clone(), fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(auth_conf, &mut h.context);

        expect_assoc_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);

        assert!(suppl_mock.is_supplicant_started());

        // (mlme->sme) Send an EapolInd, mock supplicant with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: false,
        };
        let state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update]);

        expect_eapol_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an EapolInd, mock supplicant with keys
        let ptk = SecAssocUpdate::Key(Key::Ptk(test_utils::wpa1_ptk()));
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::wpa1_gtk()));
        let state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![ptk, gtk]);

        expect_set_wpa1_ptk(&mut h.mlme_stream, bssid);
        expect_set_wpa1_gtk(&mut h.mlme_stream);

        let state = on_set_keys_conf(state, &mut h, vec![0, 2]);

        // (mlme->sme) Send an EapolInd, mock supplicant with completion status
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        let _state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update]);

        expect_set_ctrl_port(&mut h.mlme_stream, bssid, fidl_mlme::ControlledPortState::Open);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });
    }

    #[test]
    fn associate_happy_path_protected() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        expect_join_request(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send a JoinConf as a response
        let join_conf = create_join_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(join_conf, &mut h.context);

        expect_auth_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AuthenticateConf as a response
        let auth_conf = create_auth_conf(bssid.clone(), fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(auth_conf, &mut h.context);

        expect_assoc_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);

        assert!(suppl_mock.is_supplicant_started());

        // (mlme->sme) Send an EapolInd, mock supplicant with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        };
        let state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update]);

        expect_eapol_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an EapolInd, mock supplicant with keys
        let ptk = SecAssocUpdate::Key(Key::Ptk(test_utils::ptk()));
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::gtk()));
        let state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![ptk, gtk]);

        expect_set_ptk(&mut h.mlme_stream, bssid);
        expect_set_gtk(&mut h.mlme_stream);

        let state = on_set_keys_conf(state, &mut h, vec![0, 2]);

        // (mlme->sme) Send an EapolInd, mock supplicant with completion status
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        let _state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update]);

        expect_set_ctrl_port(&mut h.mlme_stream, bssid, fidl_mlme::ControlledPortState::Open);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Success);
        });
    }

    #[test]
    fn join_failure() {
        let mut h = TestHelper::new();

        let (cmd, mut connect_txn_stream) = connect_command_one();
        // Start in a "Joining" state
        let state = ClientState::from(testing::new_state(Joining {
            cfg: ClientConfig::default(),
            cmd,
            protection_ie: None,
        }));

        // (mlme->sme) Send an unsuccessful JoinConf
        let join_conf = MlmeEvent::JoinConf {
            resp: fidl_mlme::JoinConfirm { result_code: fidl_ieee80211::StatusCode::JoinFailure },
        };
        let state = state.on_mlme_event(join_conf, &mut h.context);
        assert_idle(state);

        // User should be notified that connection attempt failed
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectFailure::JoinFailure(fidl_ieee80211::StatusCode::JoinFailure).into());
        });
    }

    #[test]
    fn authenticate_failure() {
        let mut h = TestHelper::new();

        let (cmd, mut connect_txn_stream) = connect_command_one();

        // Start in an "Authenticating" state
        let state = ClientState::from(testing::new_state(Authenticating {
            cfg: ClientConfig::default(),
            cmd,
            protection_ie: None,
        }));

        // (mlme->sme) Send an unsuccessful AuthenticateConf
        let auth_conf = MlmeEvent::AuthenticateConf {
            resp: fidl_mlme::AuthenticateConfirm {
                peer_sta_address: connect_command_one().0.bss.bssid.0,
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
                result_code: fidl_ieee80211::StatusCode::DeniedNoMoreStas,
            },
        };
        let state = state.on_mlme_event(auth_conf, &mut h.context);
        assert_idle(state);

        // User should be notified that connection attempt failed
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectFailure::AuthenticationFailure(fidl_ieee80211::StatusCode::DeniedNoMoreStas).into());
        });
    }

    #[test]
    fn associate_failure() {
        let mut h = TestHelper::new();

        let (cmd, mut connect_txn_stream) = connect_command_one();
        let bss_protection = cmd.bss.protection();

        // Start in an "Associating" state
        let state = ClientState::from(testing::new_state(Associating {
            cfg: ClientConfig::default(),
            cmd,
            protection_ie: None,
        }));

        // (mlme->sme) Send an unsuccessful AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::RefusedReasonUnspecified);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);
        assert_idle(state);

        // User should be notified that connection attempt failed
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, AssociationFailure {
                bss_protection,
                code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            }
            .into());
        });
    }

    #[test]
    fn set_keys_failure() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();

        let state = idle_state();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();

        // Issue a "connect" command
        let state = state.connect(command, &mut h.context);

        expect_join_request(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send a JoinConf as a response
        let join_conf = create_join_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(join_conf, &mut h.context);

        expect_auth_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AuthenticateConf as a response
        let auth_conf = create_auth_conf(bssid.clone(), fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(auth_conf, &mut h.context);

        expect_assoc_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);

        assert!(suppl_mock.is_supplicant_started());

        // (mlme->sme) Send an EapolInd, mock supplicant with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: false,
        };
        let state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update]);

        expect_eapol_req(&mut h.mlme_stream, bssid);

        // (mlme->sme) Send an EapolInd, mock supplicant with keys
        let ptk = SecAssocUpdate::Key(Key::Ptk(test_utils::ptk()));
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::gtk()));
        let state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![ptk, gtk]);

        expect_set_ptk(&mut h.mlme_stream, bssid);
        expect_set_gtk(&mut h.mlme_stream);

        // (mlme->sme) Send an EapolInd, mock supplicant with completion status
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        let state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update]);

        // No update until all key confs are received.
        assert!(connect_txn_stream.try_next().is_err());

        // One key fails to set
        state.on_mlme_event(
            MlmeEvent::SetKeysConf {
                conf: fidl_mlme::SetKeysConfirm {
                    results: vec![
                        fidl_mlme::SetKeyResult { key_id: 0, status: zx::Status::OK.into_raw() },
                        fidl_mlme::SetKeyResult {
                            key_id: 2,
                            status: zx::Status::INTERNAL.into_raw(),
                        },
                    ],
                },
            },
            &mut h.context,
        );

        assert_variant!(connect_txn_stream.try_next(),
        Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_variant!(result, ConnectResult::Failed(_))
        });
    }

    #[test]
    fn connect_while_joining() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let state = joining_state(cmd_one);
        let (cmd_two, _connect_txn_stream2) = connect_command_two();
        let state = state.connect(cmd_two, &mut h.context);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Canceled);
        });
        expect_join_request(&mut h.mlme_stream, connect_command_two().0.bss.bssid);
        assert_joining(state, &connect_command_two().0.bss);
    }

    #[test]
    fn connect_while_authenticating() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let state = authenticating_state(cmd_one);
        let (cmd_two, _connect_txn_stream2) = connect_command_two();
        let state = state.connect(cmd_two, &mut h.context);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Canceled);
        });
        expect_join_request(&mut h.mlme_stream, connect_command_two().0.bss.bssid);
        assert_joining(state, &connect_command_two().0.bss);
    }

    #[test]
    fn connect_while_associating() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let state = associating_state(cmd_one);
        let (cmd_two, _connect_txn_stream2) = connect_command_two();
        let state = state.connect(cmd_two, &mut h.context);
        let state = exchange_deauth(state, &mut h);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Canceled);
        });
        expect_join_request(&mut h.mlme_stream, connect_command_two().0.bss.bssid);
        assert_joining(state, &connect_command_two().0.bss);
    }

    #[test]
    fn deauth_while_authing() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let state = authenticating_state(cmd_one);
        let deauth_ind = MlmeEvent::DeauthenticateInd {
            ind: fidl_mlme::DeauthenticateIndication {
                peer_sta_address: [7, 7, 7, 7, 7, 7],
                reason_code: fidl_ieee80211::ReasonCode::UnspecifiedReason,
                locally_initiated: false,
            },
        };
        let state = state.on_mlme_event(deauth_ind, &mut h.context);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectFailure::AuthenticationFailure(fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc).into());
        });
        assert_idle(state);
    }

    #[test]
    fn deauth_while_associating() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let bss_protection = cmd_one.bss.protection();
        let state = associating_state(cmd_one);
        let deauth_ind = MlmeEvent::DeauthenticateInd {
            ind: fidl_mlme::DeauthenticateIndication {
                peer_sta_address: [7, 7, 7, 7, 7, 7],
                reason_code: fidl_ieee80211::ReasonCode::UnspecifiedReason,
                locally_initiated: false,
            },
        };
        let state = state.on_mlme_event(deauth_ind, &mut h.context);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, AssociationFailure {
                bss_protection,
                code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
            }
            .into());
        });
        assert_idle(state);
    }

    #[test]
    fn disassoc_while_associating() {
        let mut h = TestHelper::new();
        let (cmd_one, mut connect_txn_stream1) = connect_command_one();
        let bss_protection = cmd_one.bss.protection();
        let bssid = cmd_one.bss.bssid.clone();
        let state = associating_state(cmd_one);
        let disassoc_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [7, 7, 7, 7, 7, 7],
                reason_code: fidl_ieee80211::ReasonCode::PeerkeyMismatch,
                locally_initiated: false,
            },
        };
        let state = state.on_mlme_event(disassoc_ind, &mut h.context);
        expect_deauth_req(&mut h.mlme_stream, bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream1.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, AssociationFailure {
                bss_protection,
                code: fidl_ieee80211::StatusCode::SpuriousDeauthOrDisassoc,
            }
            .into());
        });
        assert_idle(state);
    }

    #[test]
    fn supplicant_fails_to_start_while_associating() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();
        let state = associating_state(command);

        suppl_mock.set_start_failure(format_err!("failed to start supplicant"));

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(assoc_conf, &mut h.context);

        expect_deauth_req(&mut h.mlme_stream, bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::StartSupplicantFailed,
            }
            .into());
        });
    }

    #[test]
    fn bad_eapol_frame_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();
        let state = establishing_rsna_state(command);

        // doesn't matter what we mock here
        let update = SecAssocUpdate::Status(SecAssocStatus::EssSaEstablished);
        suppl_mock.set_on_eapol_frame_updates(vec![update]);

        // (mlme->sme) Send an EapolInd with bad eapol data
        let eapol_ind = create_eapol_ind(bssid.clone(), vec![1, 2, 3, 4]);
        let s = state.on_mlme_event(eapol_ind, &mut h.context);

        // There should be no message in the connect_txn_stream
        assert_variant!(connect_txn_stream.try_next(), Err(_));
        assert_variant!(s, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::EstablishingRsna { .. })});

        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");
    }

    #[test]
    fn supplicant_fails_to_process_eapol_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();
        let state = establishing_rsna_state(command);

        suppl_mock.set_on_eapol_frame_failure(format_err!("supplicant::on_eapol_frame fails"));

        // (mlme->sme) Send an EapolInd
        let eapol_ind = create_eapol_ind(bssid.clone(), test_utils::eapol_key_frame().into());
        let s = state.on_mlme_event(eapol_ind, &mut h.context);

        // There should be no message in the connect_txn_stream
        assert_variant!(connect_txn_stream.try_next(), Err(_));
        assert_variant!(s, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::EstablishingRsna { .. })});

        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");
    }

    #[test]
    fn reject_foreign_eapol_frames() {
        let mut h = TestHelper::new();
        let (supplicant, mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa2(supplicant);
        let state = link_up_state(cmd);
        mock.set_on_eapol_frame_callback(|| {
            panic!("eapol frame should not have been processed");
        });

        // Send an EapolInd from foreign BSS.
        let eapol_ind = create_eapol_ind(Bssid([1; 6]), test_utils::eapol_key_frame().into());
        let state = state.on_mlme_event(eapol_ind, &mut h.context);

        // Verify state did not change.
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(
                &state.link_state,
                LinkState::LinkUp(state) => assert_variant!(&state.protection, Protection::Rsna(_))
            )
        });
    }

    #[test]
    fn wrong_password_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();
        let state = establishing_rsna_state(command);

        // (mlme->sme) Send an EapolInd, mock supplicant with wrong password status
        let update = SecAssocUpdate::Status(SecAssocStatus::WrongPassword);
        let _state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update]);

        expect_deauth_req(&mut h.mlme_stream, bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::InternalError,
            }
            .into());
        });
    }

    #[test]
    fn overall_timeout_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();

        // Start in an "Associating" state
        let state = ClientState::from(testing::new_state(Associating {
            cfg: ClientConfig::default(),
            cmd: command,
            protection_ie: None,
        }));
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let state = state.on_mlme_event(assoc_conf, &mut h.context);

        let (_, timed_event) = h.time_stream.try_next().unwrap().expect("expect timed event");
        assert_variant!(timed_event.event, Event::EstablishingRsnaTimeout(..));

        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

        suppl_mock.set_on_establishing_rsna_timeout(EstablishRsnaFailureReason::OverallTimeout(
            wlan_rsn::Error::EapolHandshakeNotStarted,
        ));
        let _state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);

        expect_deauth_req(&mut h.mlme_stream, bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::OverallTimeout(wlan_rsn::Error::EapolHandshakeNotStarted),
            }.into());
        });
    }

    #[test]
    fn key_frame_exchange_timeout_while_establishing_rsna() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (command, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = command.bss.bssid.clone();
        let state = establishing_rsna_state(command);

        // (mlme->sme) Send an EapolInd, mock supplication with key frame
        let update = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        };
        let mut state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![update.clone()]);

        for i in 1..=3 {
            expect_eapol_req(&mut h.mlme_stream, bssid);
            expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");

            let (_, timed_event) = h.time_stream.try_next().unwrap().expect("expect timed event");
            assert_variant!(timed_event.event, Event::KeyFrameExchangeTimeout(_));
            if i == 3 {
                suppl_mock.set_on_eapol_key_frame_timeout_failure(format_err!(
                    "Too many key frame timeouts"
                ));
            } else {
                suppl_mock.set_on_eapol_key_frame_timeout_updates(vec![update.clone()]);
            }
            state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
        }

        expect_deauth_req(&mut h.mlme_stream, bssid, fidl_ieee80211::ReasonCode::StaLeaving);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, EstablishRsnaFailure {
                auth_method: Some(auth::MethodName::Psk),
                reason: EstablishRsnaFailureReason::KeyFrameExchangeTimeout,
            }
            .into());
        });
    }

    #[test]
    fn gtk_rotation_during_link_up() {
        let mut h = TestHelper::new();
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, mut connect_txn_stream) = connect_command_wpa2(supplicant);
        let bssid = cmd.bss.bssid;
        let state = link_up_state(cmd);

        // (mlme->sme) Send an EapolInd, mock supplication with key frame and GTK
        let key_frame = SecAssocUpdate::TxEapolKeyFrame {
            frame: test_utils::eapol_key_frame(),
            expect_response: true,
        };
        let gtk = SecAssocUpdate::Key(Key::Gtk(test_utils::gtk()));
        let mut state = on_eapol_ind(state, &mut h, bssid, &suppl_mock, vec![key_frame, gtk]);

        // EAPoL frame is sent out, but state still remains the same
        expect_eapol_req(&mut h.mlme_stream, bssid);
        expect_set_gtk(&mut h.mlme_stream);
        expect_stream_empty(&mut h.mlme_stream, "unexpected event in mlme stream");
        assert_variant!(&state, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::LinkUp { .. });
        });

        // Any timeout is ignored
        let (_, timed_event) = h.time_stream.try_next().unwrap().expect("expect timed event");
        state = state.handle_timeout(timed_event.id, timed_event.event, &mut h.context);
        assert_variant!(&state, ClientState::Associated(state) => {
            assert_variant!(&state.link_state, LinkState::LinkUp { .. });
        });

        // No new ConnectResult is sent
        assert_variant!(connect_txn_stream.try_next(), Err(_));
    }

    #[test]
    fn connect_while_link_up() {
        let mut h = TestHelper::new();
        let (cmd1, mut connect_txn_stream1) = connect_command_one();
        let (cmd2, mut connect_txn_stream2) = connect_command_two();
        let state = link_up_state(cmd1);
        let state = state.connect(cmd2, &mut h.context);
        let state = exchange_deauth(state, &mut h);

        // First stream should be dropped already
        assert_variant!(connect_txn_stream1.try_next(), Ok(None));
        // Second stream should either have event or is empty, but is not dropped
        assert_variant!(connect_txn_stream2.try_next(), Ok(Some(_)) | Err(_));

        expect_join_request(&mut h.mlme_stream, connect_command_two().0.bss.bssid);
        assert_joining(state, &connect_command_two().0.bss);
    }

    #[test]
    fn disconnect_while_idle() {
        let mut h = TestHelper::new();
        let new_state = idle_state()
            .disconnect(&mut h.context, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        assert_idle(new_state);
        // Expect no messages to the MLME
        assert!(h.mlme_stream.try_next().is_err());
    }

    #[test]
    fn disconnect_while_joining() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = joining_state(cmd);
        let state =
            state.disconnect(&mut h.context, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Canceled);
        });
        assert_idle(state);
    }

    #[test]
    fn disconnect_while_authenticating() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = authenticating_state(cmd);
        let state =
            state.disconnect(&mut h.context, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Canceled);
        });
        assert_idle(state);
    }

    #[test]
    fn disconnect_while_associating() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = associating_state(cmd);
        let state =
            state.disconnect(&mut h.context, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        let state = exchange_deauth(state, &mut h);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect: false })) => {
            assert_eq!(result, ConnectResult::Canceled);
        });
        assert_idle(state);

        assert_data_tree!(h._inspector, root: contains {
            state_events: {
                // There's no disconnect_ctx node
                "0": {
                    "@time": AnyProperty,
                    ctx: AnyProperty,
                    from: AnyProperty,
                    to: AnyProperty,
                }
            }
        });
    }

    #[test]
    fn disconnect_while_link_up() {
        let mut h = TestHelper::new();
        let state = link_up_state(connect_command_one().0);
        let state =
            state.disconnect(&mut h.context, fidl_sme::UserDisconnectReason::FailedToConnect);
        let state = exchange_deauth(state, &mut h);
        assert_idle(state);
    }

    #[test]
    fn increment_att_id_on_connect() {
        let mut h = TestHelper::new();
        let state = idle_state();
        assert_eq!(h.context.att_id, 0);

        let state = state.connect(connect_command_one().0, &mut h.context);
        assert_eq!(h.context.att_id, 1);

        let state =
            state.disconnect(&mut h.context, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        assert_eq!(h.context.att_id, 1);

        let state = state.connect(connect_command_two().0, &mut h.context);
        assert_eq!(h.context.att_id, 2);

        let _state = state.connect(connect_command_one().0, &mut h.context);
        assert_eq!(h.context.att_id, 3);
    }

    #[test]
    fn increment_att_id_on_disassociate_ind() {
        let mut h = TestHelper::new();
        let (cmd, _connect_txn_stream) = connect_command_one();
        let bss = cmd.bss.clone();
        let state = link_up_state(cmd);
        assert_eq!(h.context.att_id, 0);

        let disassociate_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::UnspecifiedReason,
                locally_initiated: false,
            },
        };

        let state = state.on_mlme_event(disassociate_ind, &mut h.context);
        assert_associating(state, &bss);
        assert_eq!(h.context.att_id, 1);
    }

    #[test]
    fn do_not_log_disconnect_ctx_on_disassoc_from_non_link_up() {
        let mut h = TestHelper::new();
        let (supplicant, _suppl_mock) = mock_psk_supplicant();
        let (command, _connect_txn_stream) = connect_command_wpa2(supplicant);
        let state = establishing_rsna_state(command);

        let disassociate_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::UnacceptablePowerCapability,
                locally_initiated: true,
            },
        };
        let state = state.on_mlme_event(disassociate_ind, &mut h.context);
        assert_associating(
            state,
            &fake_bss_description!(Wpa2, ssid: Ssid::try_from("wpa2").unwrap()),
        );

        assert_data_tree!(h._inspector, root: contains {
            state_events: {
                // There's no disconnect_ctx node
                "0": {
                    "@time": AnyProperty,
                    ctx: AnyProperty,
                    from: AnyProperty,
                    to: AnyProperty,
                }
            }
        });
    }

    #[test]
    fn disconnect_reported_on_deauth_ind() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = link_up_state(cmd);

        let deauth_ind = MlmeEvent::DeauthenticateInd {
            ind: fidl_mlme::DeauthenticateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                locally_initiated: true,
            },
        };

        let _state = state.on_mlme_event(deauth_ind, &mut h.context);
        let fidl_info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(!fidl_info.is_sme_reconnecting);
        assert_eq!(
            fidl_info.disconnect_source,
            fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
            })
        );
    }

    #[test]
    fn disconnect_reported_on_disassoc_ind_then_reconnect_successfully() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let bss = cmd.bss.clone();
        let state = link_up_state(cmd);

        let deauth_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::ReasonInactivity,
                locally_initiated: true,
            },
        };

        let state = state.on_mlme_event(deauth_ind, &mut h.context);
        let info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(info.is_sme_reconnecting);
        assert_eq!(
            info.disconnect_source,
            fidl_sme::DisconnectSource::Mlme(fidl_sme::DisconnectCause {
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DisassociateIndication,
                reason_code: fidl_ieee80211::ReasonCode::ReasonInactivity,
            })
        );

        // Check that association is attempted
        expect_assoc_req(&mut h.mlme_stream, bss.bssid);

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::Success);
        let _state = state.on_mlme_event(assoc_conf, &mut h.context);

        // User should be notified that we are reconnected
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect })) => {
            assert_eq!(result, ConnectResult::Success);
            assert!(is_reconnect);
        });
    }

    #[test]
    fn disconnect_reported_on_disassoc_ind_then_reconnect_unsuccessfully() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let bss = cmd.bss.clone();
        let state = link_up_state(cmd);

        let deauth_ind = MlmeEvent::DisassociateInd {
            ind: fidl_mlme::DisassociateIndication {
                peer_sta_address: [0, 0, 0, 0, 0, 0],
                reason_code: fidl_ieee80211::ReasonCode::ReasonInactivity,
                locally_initiated: true,
            },
        };

        let state = state.on_mlme_event(deauth_ind, &mut h.context);
        assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { .. }))
        );

        // Check that association is attempted
        expect_assoc_req(&mut h.mlme_stream, bss.bssid);

        // (mlme->sme) Send an AssociateConf
        let assoc_conf = create_assoc_conf(fidl_ieee80211::StatusCode::RefusedReasonUnspecified);
        let _state = state.on_mlme_event(assoc_conf, &mut h.context);

        // User should be notified that reconnection attempt failed
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnConnectResult { result, is_reconnect })) => {
            assert_eq!(result, AssociationFailure {
                bss_protection: BssProtection::Open,
                code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
            }.into());
            assert!(is_reconnect);
        });
    }

    #[test]
    fn disconnect_reported_on_manual_disconnect() {
        let mut h = TestHelper::new();
        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = link_up_state(cmd);

        let _state =
            state.disconnect(&mut h.context, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        let info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(!info.is_sme_reconnecting);
        assert_eq!(
            info.disconnect_source,
            fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::WlanSmeUnitTesting)
        );
    }

    #[test]
    fn disconnect_reported_on_manual_disconnect_with_wsc() {
        let mut h = TestHelper::new();
        let (mut cmd, mut connect_txn_stream) = connect_command_one();
        cmd.bss = Box::new(fake_bss_description!(Open,
            ssid: Ssid::try_from("bar").unwrap(),
            bssid: [8; 6],
            rssi_dbm: 60,
            snr_db: 30,
            ies_overrides: IesOverrides::new().set_raw(
                get_vendor_ie_bytes_for_wsc_ie(&fake_probe_resp_wsc_ie_bytes()).expect("getting vendor ie bytes")
        )));
        println!("{:02x?}", cmd.bss);

        let state = link_up_state(cmd);

        let _state =
            state.disconnect(&mut h.context, fidl_sme::UserDisconnectReason::WlanSmeUnitTesting);
        let info = assert_variant!(
            connect_txn_stream.try_next(),
            Ok(Some(ConnectTransactionEvent::OnDisconnect { info })) => info
        );
        assert!(!info.is_sme_reconnecting);
        assert_eq!(
            info.disconnect_source,
            fidl_sme::DisconnectSource::User(fidl_sme::UserDisconnectReason::WlanSmeUnitTesting)
        );
    }

    #[test]
    fn bss_channel_switch_ind() {
        let mut h = TestHelper::new();
        let (mut cmd, mut connect_txn_stream) = connect_command_one();
        cmd.bss = Box::new(fake_bss_description!(Open,
            ssid: Ssid::try_from("bar").unwrap(),
            bssid: [8; 6],
            channel: Channel::new(1, Cbw::Cbw20),
        ));
        let state = link_up_state(cmd);

        let input_info = fidl_internal::ChannelSwitchInfo { new_channel: 36 };
        let switch_ind = MlmeEvent::OnChannelSwitched { info: input_info.clone() };

        assert_variant!(&state, ClientState::Associated(state) => {
            assert_eq!(state.latest_ap_state.channel.primary, 1);
        });
        let state = state.on_mlme_event(switch_ind, &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_eq!(state.latest_ap_state.channel.primary, 36);
        });

        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnChannelSwitched { info })) => {
            assert_eq!(info, input_info);
        });
    }

    #[test]
    fn join_failure_capabilities_incompatible_fullmac() {
        let (mut command, _connect_txn_stream) = connect_command_one();
        command.bss = Box::new(fake_bss_description!(Open,
            ssid: Ssid::try_from("foo").unwrap(),
            bssid: [7, 7, 7, 7, 7, 7],
            // Set a fake basic rate that our mocked client can't support, causing
            // `derive_join_and_capabilities` to fail, which in turn fails the join.
            ies_overrides: IesOverrides::new()
                .set(ie::IeType::SUPPORTED_RATES, vec![0xff])
        ));

        let mut h = TestHelper::new();
        // set as full mac
        h.context.mac_sublayer_support.device.mac_implementation_type =
            fidl_common::MacImplementationType::Fullmac;

        let state = idle_state().connect(command, &mut h.context);

        // State did not change to Joining because the command was ignored due to incompatibility.
        assert_variant!(state, ClientState::Idle(_));
    }

    #[test]
    fn join_success_fullmac() {
        let (command, _connect_txn_stream) = connect_command_one();
        let mut h = TestHelper::new();
        // set full mac
        h.context.mac_sublayer_support.device.mac_implementation_type =
            fidl_common::MacImplementationType::Fullmac;
        let state = idle_state().connect(command, &mut h.context);

        // State changed to Joining, capabilities discarded as FullMAC ignore them anyway.
        assert_variant!(&state, ClientState::Joining(_state));
    }

    #[test]
    fn join_failure_rsne_wrapped_in_legacy_wpa() {
        let (supplicant, _suppl_mock) = mock_psk_supplicant();

        let (mut command, _connect_txn_stream) = connect_command_wpa2(supplicant);
        // Take the RSNA and wrap it in LegacyWpa to make it invalid.
        if let Protection::Rsna(rsna) = command.protection {
            command.protection = Protection::LegacyWpa(rsna);
        } else {
            panic!("command is guaranteed to be contain legacy wpa");
        };

        let mut h = TestHelper::new();
        let state = idle_state().connect(command, &mut h.context);

        // State did not change to Joining because command is invalid, thus ignored.
        assert_variant!(state, ClientState::Idle(_));
    }

    #[test]
    fn join_failure_legacy_wpa_wrapped_in_rsna() {
        let (supplicant, _suppl_mock) = mock_psk_supplicant();

        let (mut command, _connect_txn_stream) = connect_command_wpa1(supplicant);
        // Take the LegacyWpa RSNA and wrap it in Rsna to make it invalid.
        if let Protection::LegacyWpa(rsna) = command.protection {
            command.protection = Protection::Rsna(rsna);
        } else {
            panic!("command is guaranteed to be contain legacy wpa");
        };

        let mut h = TestHelper::new();
        let state = idle_state();
        let state = state.connect(command, &mut h.context);

        // State did not change to Joining because command is invalid, thus ignored.
        assert_variant!(state, ClientState::Idle(_));
    }

    #[test]
    fn fill_wmm_ie_associating() {
        let mut h = TestHelper::new();
        let (cmd, _connect_txn_stream) = connect_command_one();
        let resp = fidl_mlme::AssociateConfirm {
            result_code: fidl_ieee80211::StatusCode::Success,
            association_id: 1,
            capability_info: 0,
            rates: vec![0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c],
            ht_cap: cmd.bss.raw_ht_cap().map(Box::new),
            vht_cap: cmd.bss.raw_vht_cap().map(Box::new),
            wmm_param: Some(Box::new(fake_wmm_param())),
        };

        let state = associating_state(cmd);
        let state = state.on_mlme_event(MlmeEvent::AssociateConf { resp }, &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert!(state.wmm_param.is_some());
        });
    }

    #[test]
    fn status_returns_last_rssi_snr() {
        let mut h = TestHelper::new();
        let time_a = now();

        let (cmd, mut connect_txn_stream) = connect_command_one();
        let state = link_up_state(cmd);
        let input_ind = fidl_internal::SignalReportIndication { rssi_dbm: -42, snr_db: 20 };
        let state =
            state.on_mlme_event(MlmeEvent::SignalReport { ind: input_ind.clone() }, &mut h.context);
        let serving_ap_info = assert_variant!(state.status(),
                                                     ClientSmeStatus::Connected(serving_ap_info) =>
                                                     serving_ap_info);
        assert_eq!(serving_ap_info.rssi_dbm, -42);
        assert_eq!(serving_ap_info.snr_db, 20);
        assert!(serving_ap_info.signal_report_time > time_a);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnSignalReport { ind })) => {
            assert_eq!(input_ind, ind);
        });

        let time_b = now();
        let signal_report_time = assert_variant!(state.status(),
                                                 ClientSmeStatus::Connected(serving_ap_info) =>
                                                 serving_ap_info.signal_report_time);
        assert!(signal_report_time < time_b);

        let input_ind = fidl_internal::SignalReportIndication { rssi_dbm: -24, snr_db: 10 };
        let state =
            state.on_mlme_event(MlmeEvent::SignalReport { ind: input_ind.clone() }, &mut h.context);
        let serving_ap_info = assert_variant!(state.status(),
                                                     ClientSmeStatus::Connected(serving_ap_info) =>
                                                     serving_ap_info);
        assert_eq!(serving_ap_info.rssi_dbm, -24);
        assert_eq!(serving_ap_info.snr_db, 10);
        let signal_report_time = assert_variant!(state.status(),
                                                 ClientSmeStatus::Connected(serving_ap_info) =>
                                                 serving_ap_info.signal_report_time);
        assert!(signal_report_time > time_b);
        assert_variant!(connect_txn_stream.try_next(), Ok(Some(ConnectTransactionEvent::OnSignalReport { ind })) => {
            assert_eq!(input_ind, ind);
        });

        let time_c = now();
        let signal_report_time = assert_variant!(state.status(),
                                                 ClientSmeStatus::Connected(serving_ap_info) =>
                                                 serving_ap_info.signal_report_time);
        assert!(signal_report_time < time_c);
    }

    fn test_sae_frame_rx_tx(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) -> ClientState {
        let mut h = TestHelper::new();
        let frame_rx = fidl_mlme::SaeFrame {
            peer_sta_address: [0xaa; 6],
            status_code: fidl_ieee80211::StatusCode::Success,
            seq_num: 1,
            sae_fields: vec![1, 2, 3, 4, 5],
        };
        let frame_tx = fidl_mlme::SaeFrame {
            peer_sta_address: [0xbb; 6],
            status_code: fidl_ieee80211::StatusCode::Success,
            seq_num: 2,
            sae_fields: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        mock_supplicant_controller
            .set_on_sae_frame_rx_updates(vec![SecAssocUpdate::TxSaeFrame(frame_tx)]);
        let state =
            state.on_mlme_event(MlmeEvent::OnSaeFrameRx { frame: frame_rx }, &mut h.context);
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::SaeFrameTx(_))));
        state
    }

    #[test]
    fn sae_sends_frame_in_authenticating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = authenticating_state(cmd);
        let end_state = test_sae_frame_rx_tx(suppl_mock, state);
        assert_variant!(end_state, ClientState::Authenticating(_))
    }

    #[test]
    fn sae_sends_frame_in_associating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = associating_state(cmd);
        let end_state = test_sae_frame_rx_tx(suppl_mock, state);
        assert_variant!(end_state, ClientState::Associating(_))
    }

    fn test_sae_frame_ind_resp(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) -> ClientState {
        let mut h = TestHelper::new();
        let ind = fidl_mlme::SaeHandshakeIndication { peer_sta_address: [0xaa; 6] };
        // For the purposes of the test, skip the rx/tx and just say we succeeded.
        mock_supplicant_controller.set_on_sae_handshake_ind_updates(vec![
            SecAssocUpdate::SaeAuthStatus(AuthStatus::Success),
        ]);
        let state = state.on_mlme_event(MlmeEvent::OnSaeHandshakeInd { ind }, &mut h.context);

        let resp = assert_variant!(
            h.mlme_stream.try_next(),
            Ok(Some(MlmeRequest::SaeHandshakeResp(resp))) => resp);
        assert_eq!(resp.status_code, fidl_ieee80211::StatusCode::Success);
        state
    }

    #[test]
    fn sae_ind_in_authenticating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = authenticating_state(cmd);
        let end_state = test_sae_frame_ind_resp(suppl_mock, state);
        assert_variant!(end_state, ClientState::Authenticating(_))
    }

    #[test]
    fn sae_ind_in_associating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = associating_state(cmd);
        let end_state = test_sae_frame_ind_resp(suppl_mock, state);
        assert_variant!(end_state, ClientState::Associating(_))
    }

    fn test_sae_timeout(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) -> ClientState {
        let mut h = TestHelper::new();
        let frame_tx = fidl_mlme::SaeFrame {
            peer_sta_address: [0xbb; 6],
            status_code: fidl_ieee80211::StatusCode::Success,
            seq_num: 2,
            sae_fields: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        mock_supplicant_controller
            .set_on_sae_timeout_updates(vec![SecAssocUpdate::TxSaeFrame(frame_tx)]);
        let state = state.handle_timeout(1, event::SaeTimeout(2).into(), &mut h.context);
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::SaeFrameTx(_))));
        state
    }

    fn test_sae_timeout_failure(
        mock_supplicant_controller: MockSupplicantController,
        state: ClientState,
    ) {
        let mut h = TestHelper::new();
        mock_supplicant_controller
            .set_on_sae_timeout_failure(anyhow::anyhow!("Failed to process timeout"));
        let state = state.handle_timeout(1, event::SaeTimeout(2).into(), &mut h.context);
        assert_variant!(state, ClientState::Idle(_))
    }

    #[test]
    fn sae_timeout_in_authenticating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = authenticating_state(cmd);
        let end_state = test_sae_timeout(suppl_mock, state);
        assert_variant!(end_state, ClientState::Authenticating(_));
    }

    #[test]
    fn sae_timeout_in_associating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = associating_state(cmd);
        let end_state = test_sae_timeout(suppl_mock, state);
        assert_variant!(end_state, ClientState::Associating(_));
    }

    #[test]
    fn sae_timeout_failure_in_authenticating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = authenticating_state(cmd);
        test_sae_timeout_failure(suppl_mock, state);
    }

    #[test]
    fn sae_timeout_failure_in_associating() {
        let (supplicant, suppl_mock) = mock_psk_supplicant();
        let (cmd, _connect_txn_stream) = connect_command_wpa3(supplicant);
        let state = associating_state(cmd);
        test_sae_timeout_failure(suppl_mock, state);
    }

    #[test]
    fn update_wmm_ac_params_new() {
        let mut h = TestHelper::new();
        let wmm_param = None;
        let state = link_up_state_with_wmm(connect_command_one().0, wmm_param);

        let state = state.on_mlme_event(create_on_wmm_status_resp(zx::sys::ZX_OK), &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(state.wmm_param, Some(wmm_param) => {
                assert!(wmm_param.wmm_info.ap_wmm_info().uapsd());
                assert_wmm_param_acs(&wmm_param);
            })
        });
    }

    #[test]
    fn update_wmm_ac_params_existing() {
        let mut h = TestHelper::new();

        let existing_wmm_param =
            *ie::parse_wmm_param(&fake_wmm_param().bytes[..]).expect("parse wmm");
        existing_wmm_param.wmm_info.ap_wmm_info().set_uapsd(false);
        let state = link_up_state_with_wmm(connect_command_one().0, Some(existing_wmm_param));

        let state = state.on_mlme_event(create_on_wmm_status_resp(zx::sys::ZX_OK), &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(state.wmm_param, Some(wmm_param) => {
                assert!(wmm_param.wmm_info.ap_wmm_info().uapsd());
                assert_wmm_param_acs(&wmm_param);
            })
        });
    }

    #[test]
    fn update_wmm_ac_params_fails() {
        let mut h = TestHelper::new();

        let existing_wmm_param =
            *ie::parse_wmm_param(&fake_wmm_param().bytes[..]).expect("parse wmm");
        let state = link_up_state_with_wmm(connect_command_one().0, Some(existing_wmm_param));

        let state = state
            .on_mlme_event(create_on_wmm_status_resp(zx::sys::ZX_ERR_UNAVAILABLE), &mut h.context);
        assert_variant!(state, ClientState::Associated(state) => {
            assert_variant!(state.wmm_param, Some(wmm_param) => {
                assert_eq!(wmm_param, existing_wmm_param);
            })
        });
    }

    fn assert_wmm_param_acs(wmm_param: &ie::WmmParam) {
        assert_eq!(wmm_param.ac_be_params.aci_aifsn.aifsn(), 1);
        assert!(!wmm_param.ac_be_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_be_params.ecw_min_max.ecw_min(), 2);
        assert_eq!(wmm_param.ac_be_params.ecw_min_max.ecw_max(), 3);
        assert_eq!({ wmm_param.ac_be_params.txop_limit }, 4);

        assert_eq!(wmm_param.ac_bk_params.aci_aifsn.aifsn(), 5);
        assert!(!wmm_param.ac_bk_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_bk_params.ecw_min_max.ecw_min(), 6);
        assert_eq!(wmm_param.ac_bk_params.ecw_min_max.ecw_max(), 7);
        assert_eq!({ wmm_param.ac_bk_params.txop_limit }, 8);

        assert_eq!(wmm_param.ac_vi_params.aci_aifsn.aifsn(), 9);
        assert!(wmm_param.ac_vi_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_vi_params.ecw_min_max.ecw_min(), 10);
        assert_eq!(wmm_param.ac_vi_params.ecw_min_max.ecw_max(), 11);
        assert_eq!({ wmm_param.ac_vi_params.txop_limit }, 12);

        assert_eq!(wmm_param.ac_vo_params.aci_aifsn.aifsn(), 13);
        assert!(wmm_param.ac_vo_params.aci_aifsn.acm());
        assert_eq!(wmm_param.ac_vo_params.ecw_min_max.ecw_min(), 14);
        assert_eq!(wmm_param.ac_vo_params.ecw_min_max.ecw_max(), 15);
        assert_eq!({ wmm_param.ac_vo_params.txop_limit }, 16);
    }

    // Helper functions and data structures for tests
    struct TestHelper {
        mlme_stream: MlmeStream,
        time_stream: TimeStream,
        context: Context,
        // Inspector is kept so that root node doesn't automatically get removed from VMO
        _inspector: Inspector,
        // Executor is needed as a time provider for the [`inspect_log!`] macro which panics
        // without a fuchsia_async executor set up
        _executor: fuchsia_async::TestExecutor,
    }

    impl TestHelper {
        fn new() -> Self {
            let executor = fuchsia_async::TestExecutor::new().unwrap();
            let (mlme_sink, mlme_stream) = mpsc::unbounded();
            let (timer, time_stream) = timer::create_timer();
            let inspector = Inspector::new();
            let hasher = WlanHasher::new([88, 77, 66, 55, 44, 33, 22, 11]);
            let mut context = Context {
                device_info: Arc::new(fake_device_info()),
                mlme_sink: MlmeSink::new(mlme_sink),
                timer,
                att_id: 0,
                inspect: Arc::new(inspect::SmeTree::new(inspector.root(), hasher)),
                mac_sublayer_support: fake_mac_sublayer_support(),
                security_support: fake_security_support(),
            };
            // TODO(fxbug.dev/96668) - FullMAC still uses the old state machine. Once FullMAC is
            //                         fully transitioned, this override will no longer be
            //                         necessary.
            context.mac_sublayer_support.device.mac_implementation_type =
                fidl_common::MacImplementationType::Fullmac;
            TestHelper {
                mlme_stream,
                time_stream,
                context,
                _inspector: inspector,
                _executor: executor,
            }
        }
    }

    fn on_eapol_ind(
        state: ClientState,
        helper: &mut TestHelper,
        bssid: Bssid,
        suppl_mock: &MockSupplicantController,
        update_sink: UpdateSink,
    ) -> ClientState {
        suppl_mock.set_on_eapol_frame_updates(update_sink);
        // (mlme->sme) Send an EapolInd
        let eapol_ind = create_eapol_ind(bssid.clone(), test_utils::eapol_key_frame().into());
        state.on_mlme_event(eapol_ind, &mut helper.context)
    }

    fn on_set_keys_conf(
        state: ClientState,
        helper: &mut TestHelper,
        key_ids: Vec<u16>,
    ) -> ClientState {
        state.on_mlme_event(
            MlmeEvent::SetKeysConf {
                conf: fidl_mlme::SetKeysConfirm {
                    results: key_ids
                        .into_iter()
                        .map(|key_id| fidl_mlme::SetKeyResult {
                            key_id,
                            status: zx::Status::OK.into_raw(),
                        })
                        .collect(),
                },
            },
            &mut helper.context,
        )
    }

    fn create_eapol_ind(bssid: Bssid, data: Vec<u8>) -> MlmeEvent {
        MlmeEvent::EapolInd {
            ind: fidl_mlme::EapolIndication {
                src_addr: bssid.0,
                dst_addr: fake_device_info().sta_addr,
                data,
            },
        }
    }

    fn exchange_deauth(state: ClientState, h: &mut TestHelper) -> ClientState {
        // (sme->mlme) Expect a DeauthenticateRequest
        assert_variant!(h.mlme_stream.try_next(), Ok(Some(MlmeRequest::Deauthenticate(req))) => {
            assert_eq!(connect_command_one().0.bss.bssid.0, req.peer_sta_address);
        });

        // (mlme->sme) Send a DeauthenticateConf as a response
        let deauth_conf = MlmeEvent::DeauthenticateConf {
            resp: fidl_mlme::DeauthenticateConfirm {
                peer_sta_address: connect_command_one().0.bss.bssid.0,
            },
        };
        state.on_mlme_event(deauth_conf, &mut h.context)
    }

    fn expect_join_request(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        // (sme->mlme) Expect a JoinRequest
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::Join(req))) => {
            assert_eq!(bssid.0, req.selected_bss.bssid)
        });
    }

    fn expect_set_ctrl_port(
        mlme_stream: &mut MlmeStream,
        bssid: Bssid,
        state: fidl_mlme::ControlledPortState,
    ) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetCtrlPort(req))) => {
            assert_eq!(req.peer_sta_address, bssid.0);
            assert_eq!(req.state, state);
        });
    }

    fn expect_auth_req(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        // (sme->mlme) Expect an AuthenticateRequest
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::Authenticate(req))) => {
            assert_eq!(bssid.0, req.peer_sta_address)
        });
    }

    fn expect_deauth_req(
        mlme_stream: &mut MlmeStream,
        bssid: Bssid,
        reason_code: fidl_ieee80211::ReasonCode,
    ) {
        // (sme->mlme) Expect a DeauthenticateRequest
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::Deauthenticate(req))) => {
            assert_eq!(bssid.0, req.peer_sta_address);
            assert_eq!(reason_code, req.reason_code);
        });
    }

    fn expect_assoc_req(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::Associate(req))) => {
            assert_eq!(bssid.0, req.peer_sta_address);
        });
    }

    #[track_caller]
    fn expect_eapol_req(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::Eapol(req))) => {
            assert_eq!(req.src_addr, fake_device_info().sta_addr);
            assert_eq!(req.dst_addr, bssid.0);
            assert_eq!(req.data, Vec::<u8>::from(test_utils::eapol_key_frame()));
        });
    }

    fn expect_set_ptk(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.get(0).expect("expect key descriptor");
            assert_eq!(k.key, vec![0xCCu8; test_utils::cipher().tk_bytes().unwrap()]);
            assert_eq!(k.key_id, 0);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Pairwise);
            assert_eq!(k.address, bssid.0);
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x0F, 0xAC]);
            assert_eq!(k.cipher_suite_type, 4);
        });
    }

    fn expect_set_gtk(mlme_stream: &mut MlmeStream) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.get(0).expect("expect key descriptor");
            assert_eq!(k.key, test_utils::gtk_bytes());
            assert_eq!(k.key_id, 2);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Group);
            assert_eq!(k.address, [0xFFu8; 6]);
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x0F, 0xAC]);
            assert_eq!(k.cipher_suite_type, 4);
        });
    }

    fn expect_set_wpa1_ptk(mlme_stream: &mut MlmeStream, bssid: Bssid) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.get(0).expect("expect key descriptor");
            assert_eq!(k.key, vec![0xCCu8; test_utils::wpa1_cipher().tk_bytes().unwrap()]);
            assert_eq!(k.key_id, 0);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Pairwise);
            assert_eq!(k.address, bssid.0);
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x50, 0xF2]);
            assert_eq!(k.cipher_suite_type, 2);
        });
    }

    fn expect_set_wpa1_gtk(mlme_stream: &mut MlmeStream) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.get(0).expect("expect key descriptor");
            assert_eq!(k.key, test_utils::wpa1_gtk_bytes());
            assert_eq!(k.key_id, 2);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Group);
            assert_eq!(k.address, [0xFFu8; 6]);
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x50, 0xF2]);
            assert_eq!(k.cipher_suite_type, 2);
        });
    }

    fn expect_set_wep_key(mlme_stream: &mut MlmeStream, bssid: Bssid, key_bytes: Vec<u8>) {
        assert_variant!(mlme_stream.try_next(), Ok(Some(MlmeRequest::SetKeys(set_keys_req))) => {
            assert_eq!(set_keys_req.keylist.len(), 1);
            let k = set_keys_req.keylist.get(0).expect("expect key descriptor");
            assert_eq!(k.key, &key_bytes[..]);
            assert_eq!(k.key_id, 0);
            assert_eq!(k.key_type, fidl_mlme::KeyType::Pairwise);
            assert_eq!(k.address, bssid.0);
            assert_eq!(k.rsc, 0);
            assert_eq!(k.cipher_suite_oui, [0x00, 0x0F, 0xAC]);
            assert_eq!(k.cipher_suite_type, 1);
        });
    }

    fn connect_command_one() -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let cmd = ConnectCommand {
            bss: Box::new(fake_bss_description!(Open,
                ssid: Ssid::try_from("foo").unwrap(),
                bssid: [7, 7, 7, 7, 7, 7],
                rssi_dbm: 60,
                snr_db: 30
            )),
            connect_txn_sink,
            protection: Protection::Open,
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_two() -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let cmd = ConnectCommand {
            bss: Box::new(
                fake_bss_description!(Open, ssid: Ssid::try_from("bar").unwrap(), bssid: [8, 8, 8, 8, 8, 8]),
            ),
            connect_txn_sink,
            protection: Protection::Open,
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wep() -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let cmd = ConnectCommand {
            bss: Box::new(fake_bss_description!(Wep, ssid: Ssid::try_from("wep").unwrap())),
            connect_txn_sink,
            protection: Protection::Wep(WepKey::Wep40([3; 5])),
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wpa1(
        supplicant: MockSupplicant,
    ) -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let wpa_ie = make_wpa1_ie();
        let cmd = ConnectCommand {
            bss: Box::new(fake_bss_description!(Wpa1, ssid: Ssid::try_from("wpa1").unwrap())),
            connect_txn_sink,
            protection: Protection::LegacyWpa(Rsna {
                negotiated_protection: NegotiatedProtection::from_legacy_wpa(&wpa_ie)
                    .expect("invalid NegotiatedProtection"),
                supplicant: Box::new(supplicant),
            }),
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wpa2(
        supplicant: MockSupplicant,
    ) -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let bss = fake_bss_description!(Wpa2, ssid: Ssid::try_from("wpa2").unwrap());
        let rsne = Rsne::wpa2_rsne();
        let cmd = ConnectCommand {
            bss: Box::new(bss),
            connect_txn_sink,
            protection: Protection::Rsna(Rsna {
                negotiated_protection: NegotiatedProtection::from_rsne(&rsne)
                    .expect("invalid NegotiatedProtection"),
                supplicant: Box::new(supplicant),
            }),
        };
        (cmd, connect_txn_stream)
    }

    fn connect_command_wpa3(
        supplicant: MockSupplicant,
    ) -> (ConnectCommand, ConnectTransactionStream) {
        let (connect_txn_sink, connect_txn_stream) = ConnectTransactionSink::new_unbounded();
        let bss = fake_bss_description!(Wpa3, ssid: Ssid::try_from("wpa3").unwrap());
        let rsne = Rsne::wpa3_rsne();
        let cmd = ConnectCommand {
            bss: Box::new(bss),
            connect_txn_sink,
            protection: Protection::Rsna(Rsna {
                negotiated_protection: NegotiatedProtection::from_rsne(&rsne)
                    .expect("invalid NegotiatedProtection"),
                supplicant: Box::new(supplicant),
            }),
        };
        (cmd, connect_txn_stream)
    }

    fn idle_state() -> ClientState {
        testing::new_state(Idle { cfg: ClientConfig::default() }).into()
    }

    fn assert_idle(state: ClientState) {
        assert_variant!(&state, ClientState::Idle(_));
    }

    fn joining_state(cmd: ConnectCommand) -> ClientState {
        testing::new_state(Joining { cfg: ClientConfig::default(), cmd, protection_ie: None })
            .into()
    }

    fn assert_joining(state: ClientState, bss: &BssDescription) {
        assert_variant!(&state, ClientState::Joining(joining) => {
            assert_eq!(joining.cmd.bss.as_ref(), bss);
        });
    }

    fn authenticating_state(cmd: ConnectCommand) -> ClientState {
        testing::new_state(Authenticating {
            cfg: ClientConfig::default(),
            cmd,
            protection_ie: None,
        })
        .into()
    }

    fn associating_state(cmd: ConnectCommand) -> ClientState {
        testing::new_state(Associating { cfg: ClientConfig::default(), cmd, protection_ie: None })
            .into()
    }

    fn assert_associating(state: ClientState, bss: &BssDescription) {
        assert_variant!(&state, ClientState::Associating(associating) => {
            assert_eq!(associating.cmd.bss.as_ref(), bss);
        });
    }

    fn establishing_rsna_state(cmd: ConnectCommand) -> ClientState {
        let auth_method = cmd.protection.rsn_auth_method();
        let rsna = assert_variant!(cmd.protection, Protection::Rsna(rsna) => rsna);
        let link_state = testing::new_state(EstablishingRsna {
            rsna,
            rsna_timeout: None,
            resp_timeout: None,
            handshake_complete: false,
            pending_key_ids: Default::default(),
        })
        .into();
        testing::new_state(Associated {
            cfg: ClientConfig::default(),
            latest_ap_state: cmd.bss,
            auth_method,
            connect_txn_sink: cmd.connect_txn_sink,
            last_signal_report_time: zx::Time::ZERO,
            link_state,
            protection_ie: None,
            wmm_param: None,
            last_channel_switch_time: None,
        })
        .into()
    }

    fn link_up_state(cmd: ConnectCommand) -> ClientState {
        link_up_state_with_wmm(cmd, None)
    }

    fn link_up_state_with_wmm(cmd: ConnectCommand, wmm_param: Option<ie::WmmParam>) -> ClientState {
        let auth_method = cmd.protection.rsn_auth_method();
        let link_state =
            testing::new_state(LinkUp { protection: cmd.protection, since: now() }).into();
        testing::new_state(Associated {
            cfg: ClientConfig::default(),
            connect_txn_sink: cmd.connect_txn_sink,
            latest_ap_state: cmd.bss,
            auth_method,
            last_signal_report_time: zx::Time::ZERO,
            link_state,
            protection_ie: None,
            wmm_param,
            last_channel_switch_time: None,
        })
        .into()
    }

    fn fake_device_info() -> fidl_mlme::DeviceInfo {
        test_utils::fake_device_info([0, 1, 2, 3, 4, 5])
    }
}
