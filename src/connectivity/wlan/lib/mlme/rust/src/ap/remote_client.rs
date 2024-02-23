// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        ap::{frame_writer, BufferedFrame, Context, TimedEvent},
        buffer::Buffer,
        device::DeviceOps,
        disconnect::LocallyInitiated,
        error::Error,
    },
    banjo_fuchsia_wlan_softmac as banjo_softmac, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fidl_fuchsia_wlan_softmac as fidl_softmac, fuchsia_trace as trace, fuchsia_zircon as zx,
    ieee80211::{MacAddr, MacAddrBytes, Ssid},
    std::collections::VecDeque,
    tracing::warn,
    wlan_common::{
        appendable::Appendable,
        buffer_writer::BufferWriter,
        ie,
        mac::{self, Aid, AuthAlgorithmNumber, FrameClass, ReasonCode},
        timer::EventId,
        TimeUnit,
    },
    wlan_statemachine::StateMachine,
    wlan_trace as wtrace,
    zerocopy::ByteSlice,
};

/// dot11BssMaxIdlePeriod (IEEE Std 802.11-2016, 11.24.13 and Annex C.3): This attribute indicates
/// that the number of 1000 TUs that pass before an AP disassociates an inactive non-AP STA. This
/// value is transmitted via the BSS Max Idle Period element (IEEE Std 802.11-2016, 9.4.2.79) in
/// Association Response and Reassociation Response frames, which contains a 16-bit integer.
// TODO(https://fxbug.dev/42113580): Move this setting into the SME.
const BSS_MAX_IDLE_PERIOD: u16 = 90;

#[derive(Debug)]
enum PowerSaveState {
    /// The device is awake.
    Awake,

    /// The device is dozing.
    Dozing {
        /// Buffered frames that will be sent once the device wakes up.
        buffered: VecDeque<BufferedFrame>,
    },
}

/// The MLME state machine. The actual state machine transitions are managed and validated in the
/// SME: we only use these states to determine when packets can be sent and received.
#[derive(Debug)]
enum State {
    /// An unknown client is initially placed in the |Authenticating| state. A client may remain in
    /// this state until an MLME-AUTHENTICATE.indication is received, at which point it may either
    /// move to Authenticated or Deauthenticated.
    Authenticating,

    /// The client has successfully authenticated.
    Authenticated,

    /// The client has successfully associated.
    Associated {
        /// The association ID.
        aid: Aid,

        /// The EAPoL controlled port can be in three states:
        /// - Some(Closed): The EAPoL controlled port is closed. Only unprotected EAPoL frames can
        ///   be sent.
        /// - Some(Open): The EAPoL controlled port is open. All frames can be sent, and will be
        ///   protected.
        /// - None: There is no EAPoL authentication required, i.e. the network is not an RSN. All
        ///   frames can be sent, and will NOT be protected.
        eapol_controlled_port: Option<fidl_mlme::ControlledPortState>,

        /// The current active timeout. Should never be None, except during initialization.
        active_timeout_event_id: Option<EventId>,

        /// Power-saving state of the client.
        ps_state: PowerSaveState,
    },

    /// This is a terminal state indicating the client cannot progress any further, and should be
    /// forgotten from the MLME state.
    Deauthenticated,
}

impl State {
    fn max_frame_class(&self) -> FrameClass {
        match self {
            State::Deauthenticated | State::Authenticating => FrameClass::Class1,
            State::Authenticated => FrameClass::Class2,
            State::Associated { .. } => FrameClass::Class3,
        }
    }
}

pub struct RemoteClient {
    pub addr: MacAddr,
    state: StateMachine<State>,
}

#[derive(Debug)]
pub enum ClientRejection {
    /// The frame was not permitted in the client's current state.
    NotPermitted,

    /// The frame does not have a corresponding handler.
    Unsupported,

    /// The client is not authenticated.
    NotAuthenticated,

    /// The client is not associated.
    NotAssociated,

    /// The EAPoL controlled port is closed.
    ControlledPortClosed,

    /// The frame could not be parsed.
    ParseFailed,

    /// A request could not be sent to the SME.
    SmeSendError(Error),

    /// A request could not be sent to the PHY.
    WlanSendError(Error),

    /// A request could not be sent to the netstack.
    EthSendError(Error),

    /// An error occurred on the device.
    DeviceError(Error),
}

impl ClientRejection {
    pub fn log_level(&self) -> tracing::Level {
        match self {
            Self::ParseFailed
            | Self::SmeSendError(..)
            | Self::WlanSendError(..)
            | Self::EthSendError(..) => tracing::Level::ERROR,
            Self::ControlledPortClosed | Self::Unsupported => tracing::Level::WARN,
            _ => tracing::Level::TRACE,
        }
    }
}

#[derive(Debug)]
pub enum ClientEvent {
    /// This is the timeout that fires after dot11BssMaxIdlePeriod (IEEE Std 802.11-2016, 11.24.13
    /// and Annex C.3) elapses and no activity was detected, at which point the client is
    /// disassociated.
    BssIdleTimeout,
}

// TODO(https://fxbug.dev/42113580): Implement capability negotiation in MLME-ASSOCIATE.response.
// TODO(https://fxbug.dev/42113580): Implement action frame handling.
impl RemoteClient {
    pub fn new(addr: MacAddr) -> Self {
        Self { addr, state: StateMachine::new(State::Authenticating) }
    }

    /// Returns if the client is deauthenticated. The caller should use this to check if the client
    /// needs to be forgotten from its state.
    pub fn deauthenticated(&self) -> bool {
        match self.state.as_ref() {
            State::Deauthenticated => true,
            _ => false,
        }
    }

    /// Returns the association ID of the client, or None if it is not associated.
    pub fn aid(&self) -> Option<Aid> {
        match self.state.as_ref() {
            State::Associated { aid, .. } => Some(*aid),
            _ => None,
        }
    }

    /// Returns if the client has buffered frames (i.e. dozing and the queue is not empty).
    pub fn has_buffered_frames(&self) -> bool {
        match self.state.as_ref() {
            State::Associated { ps_state: PowerSaveState::Dozing { buffered }, .. } => {
                !buffered.is_empty()
            }
            _ => false,
        }
    }

    pub fn dozing(&self) -> bool {
        match self.state.as_ref() {
            State::Associated { ps_state: PowerSaveState::Dozing { .. }, .. } => true,
            _ => false,
        }
    }

    fn change_state<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        next_state: State,
    ) -> Result<(), Error> {
        match self.state.as_mut() {
            State::Associated { .. } => {
                ctx.device
                    .clear_association(&fidl_softmac::WlanSoftmacBaseClearAssociationRequest {
                        peer_addr: Some(self.addr.to_array()),
                        ..Default::default()
                    })
                    .map_err(|s| Error::Status(format!("failed to clear association"), s))?;
            }
            _ => (),
        }
        self.state.replace_state_with(next_state);
        Ok(())
    }

    fn schedule_after<D>(
        &self,
        ctx: &mut Context<D>,
        duration: zx::Duration,
        event: ClientEvent,
    ) -> EventId {
        ctx.schedule_after(duration, TimedEvent::ClientEvent(self.addr, event))
    }

    fn schedule_bss_idle_timeout<D>(&self, ctx: &mut Context<D>) -> EventId {
        self.schedule_after(
            ctx,
            // dot11BssMaxIdlePeriod (IEEE Std 802.11-2016, 11.24.13 and Annex C.3) is measured in
            // increments of 1000 TUs, with a range from 1-65535. We therefore need do this
            // conversion to zx::Duration in a 64-bit number space to avoid any overflow that might
            // occur, as 65535 * 1000 > 2^sizeof(TimeUnit).
            zx::Duration::from(TimeUnit(1000)) * (BSS_MAX_IDLE_PERIOD as i64),
            ClientEvent::BssIdleTimeout,
        )
    }

    fn handle_bss_idle_timeout<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        event_id: EventId,
    ) -> Result<(), ClientRejection> {
        match self.state.as_ref() {
            State::Associated { active_timeout_event_id, .. } => {
                if *active_timeout_event_id != Some(event_id) {
                    // This is not the right timeout.
                    return Ok(());
                }
            }
            _ => {
                // This is not the right state.
                return Ok(());
            }
        }

        self.change_state(ctx, State::Authenticated).map_err(ClientRejection::DeviceError)?;

        // On BSS idle timeout, we need to tell the client that they've been disassociated, and the
        // SME to transition the client to Authenticated.
        let (buffer, written) = ctx
            .make_disassoc_frame(
                self.addr.clone(),
                fidl_ieee80211::ReasonCode::ReasonInactivity.into(),
            )
            .map_err(ClientRejection::WlanSendError)?;
        self.send_wlan_frame(ctx, buffer, written, 0, None).map_err(|s| {
            ClientRejection::WlanSendError(Error::Status(
                format!("error sending disassoc frame on BSS idle timeout"),
                s,
            ))
        })?;
        ctx.send_mlme_disassoc_ind(
            self.addr.clone(),
            fidl_ieee80211::ReasonCode::ReasonInactivity,
            LocallyInitiated(true),
        )
        .map_err(ClientRejection::SmeSendError)?;
        Ok(())
    }

    /// Resets the BSS max idle timeout.
    ///
    /// If we receive a WLAN frame, we need to reset the clock on disassociating the client after
    /// timeout.
    fn reset_bss_max_idle_timeout<D>(&mut self, ctx: &mut Context<D>) {
        // TODO(https://fxbug.dev/42113580): IEEE Std 802.11-2016, 9.4.2.79 specifies a "Protected Keep-Alive Required"
        // option that indicates that only a protected frame indicates activity. It is unclear how
        // this interacts with open networks.

        // We need to do this in two parts: we can't schedule the timeout while also borrowing the
        // state, because it results in two simultaneous mutable borrows.
        let new_active_timeout_event_id = match self.state.as_ref() {
            State::Associated { .. } => Some(self.schedule_bss_idle_timeout(ctx)),
            _ => None,
        };

        match self.state.as_mut() {
            State::Associated { active_timeout_event_id, .. } => {
                *active_timeout_event_id = new_active_timeout_event_id;
            }
            _ => (),
        }
    }

    fn is_frame_class_permitted(&self, frame_class: FrameClass) -> bool {
        frame_class <= self.state.as_ref().max_frame_class()
    }

    pub fn handle_event<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        event_id: EventId,
        event: ClientEvent,
    ) -> Result<(), ClientRejection> {
        match event {
            ClientEvent::BssIdleTimeout => self.handle_bss_idle_timeout(ctx, event_id),
        }
    }

    // MLME SAP handlers.

    /// Handles MLME-AUTHENTICATE.response (IEEE Std 802.11-2016, 6.3.5.5) from the SME.
    ///
    /// If result_code is Success, the SME will have authenticated this client.
    ///
    /// Otherwise, the MLME should forget about this client.
    pub fn handle_mlme_auth_resp<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        result_code: fidl_mlme::AuthenticateResultCode,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/42172646) - Added to help investigate hw-sim test. Remove later
        tracing::info!("enter handle_mlme_auth_resp");
        self.change_state(
            ctx,
            if result_code == fidl_mlme::AuthenticateResultCode::Success {
                State::Authenticated
            } else {
                State::Deauthenticated
            },
        )?;

        // TODO(https://fxbug.dev/42172646) - Added to help investigate hw-sim test. Remove later
        tracing::info!("creating auth frame");

        // We only support open system auth in the SME.
        // IEEE Std 802.11-2016, 12.3.3.2.3 & Table 9-36: Sequence number 2 indicates the response
        // and final part of Open System authentication.
        let (buffer, written) = ctx.make_auth_frame(
            self.addr.clone(),
            AuthAlgorithmNumber::OPEN,
            2,
            match result_code {
                fidl_mlme::AuthenticateResultCode::Success => {
                    fidl_ieee80211::StatusCode::Success.into()
                }
                fidl_mlme::AuthenticateResultCode::Refused => {
                    fidl_ieee80211::StatusCode::RefusedReasonUnspecified.into()
                }
                fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired => {
                    fidl_ieee80211::StatusCode::AntiCloggingTokenRequired.into()
                }
                fidl_mlme::AuthenticateResultCode::FiniteCyclicGroupNotSupported => {
                    fidl_ieee80211::StatusCode::UnsupportedFiniteCyclicGroup.into()
                }
                fidl_mlme::AuthenticateResultCode::AuthenticationRejected => {
                    fidl_ieee80211::StatusCode::ChallengeFailure.into()
                }
                fidl_mlme::AuthenticateResultCode::AuthFailureTimeout => {
                    fidl_ieee80211::StatusCode::RejectedSequenceTimeout.into()
                }
            },
        )?;
        // TODO(https://fxbug.dev/42172646) - Added to help investigate hw-sim test. Remove later
        tracing::info!("Sending auth frame to driver: {} bytes", written);
        self.send_wlan_frame(ctx, buffer, written, 0, None)
            .map_err(|s| Error::Status(format!("error sending auth frame"), s))
    }

    /// Handles MLME-DEAUTHENTICATE.request (IEEE Std 802.11-2016, 6.3.6.2) from the SME.
    ///
    /// The SME has already deauthenticated this client.
    ///
    /// After this function is called, the MLME must forget about this client.
    pub fn handle_mlme_deauth_req<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        reason_code: fidl_ieee80211::ReasonCode,
    ) -> Result<(), Error> {
        self.change_state(ctx, State::Deauthenticated)?;

        // IEEE Std 802.11-2016, 6.3.6.3.3 states that we should send MLME-DEAUTHENTICATE.confirm
        // to the SME on success. However, our SME only sends MLME-DEAUTHENTICATE.request when it
        // has already forgotten about the client on its side, so sending
        // MLME-DEAUTHENTICATE.confirm is redundant.

        let (buffer, written) = ctx.make_deauth_frame(self.addr.clone(), reason_code.into())?;
        self.send_wlan_frame(ctx, buffer, written, 0, None)
            .map_err(|s| Error::Status(format!("error sending deauth frame"), s))
    }

    /// Handles MLME-ASSOCIATE.response (IEEE Std 802.11-2016, 6.3.7.5) from the SME.
    ///
    /// If the result code is Success, the SME will have associated this client.
    ///
    /// Otherwise, the SME has not associated this client. However, the SME has not forgotten about
    /// the client either until MLME-DEAUTHENTICATE.request is received.
    pub fn handle_mlme_assoc_resp<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        is_rsn: bool,
        channel: u8,
        capabilities: mac::CapabilityInfo,
        result_code: fidl_mlme::AssociateResultCode,
        aid: Aid,
        rates: &[u8],
    ) -> Result<(), Error> {
        self.change_state(
            ctx,
            if result_code == fidl_mlme::AssociateResultCode::Success {
                State::Associated {
                    aid,
                    eapol_controlled_port: if is_rsn {
                        Some(fidl_mlme::ControlledPortState::Closed)
                    } else {
                        None
                    },
                    active_timeout_event_id: None,
                    ps_state: PowerSaveState::Awake,
                }
            } else {
                State::Authenticated
            },
        )?;

        if let State::Associated { .. } = self.state.as_ref() {
            // Reset the client's activeness as soon as it is associated, kicking off the BSS max
            // idle timer.
            self.reset_bss_max_idle_timeout(ctx);
            ctx.device
                .notify_association_complete(fidl_softmac::WlanAssociationConfig {
                    bssid: Some(self.addr.to_array()),
                    aid: Some(aid),
                    listen_interval: None, // This field is not used for AP.
                    channel: Some(fidl_common::WlanChannel {
                        primary: channel,
                        // TODO(https://fxbug.dev/42116942): Correctly support this.
                        cbw: fidl_common::ChannelBandwidth::Cbw20,
                        secondary80: 0,
                    }),

                    qos: Some(false),
                    wmm_params: None,

                    rates: Some(rates.to_vec()),
                    capability_info: Some(capabilities.raw()),

                    // TODO(https://fxbug.dev/42116942): Correctly support all of this.
                    ht_cap: None,
                    ht_op: None,
                    vht_cap: None,
                    vht_op: None,
                    ..Default::default()
                })
                .map_err(|s| Error::Status(format!("failed to configure association"), s))?;
        }

        let (buffer, written) = match result_code {
            fidl_mlme::AssociateResultCode::Success => ctx.make_assoc_resp_frame(
                self.addr,
                capabilities,
                aid,
                rates,
                Some(BSS_MAX_IDLE_PERIOD),
            ),
            _ => ctx.make_assoc_resp_frame_error(
                self.addr,
                capabilities,
                match result_code {
                    fidl_mlme::AssociateResultCode::Success => {
                        panic!("Success should have already been handled");
                    }
                    fidl_mlme::AssociateResultCode::RefusedReasonUnspecified => {
                        fidl_ieee80211::StatusCode::RefusedReasonUnspecified.into()
                    }
                    fidl_mlme::AssociateResultCode::RefusedNotAuthenticated => {
                        fidl_ieee80211::StatusCode::RefusedUnauthenticatedAccessNotSupported.into()
                    }
                    fidl_mlme::AssociateResultCode::RefusedCapabilitiesMismatch => {
                        fidl_ieee80211::StatusCode::RefusedCapabilitiesMismatch.into()
                    }
                    fidl_mlme::AssociateResultCode::RefusedExternalReason => {
                        fidl_ieee80211::StatusCode::RefusedExternalReason.into()
                    }
                    fidl_mlme::AssociateResultCode::RefusedApOutOfMemory => {
                        fidl_ieee80211::StatusCode::RefusedApOutOfMemory.into()
                    }
                    fidl_mlme::AssociateResultCode::RefusedBasicRatesMismatch => {
                        fidl_ieee80211::StatusCode::RefusedBasicRatesMismatch.into()
                    }
                    fidl_mlme::AssociateResultCode::RejectedEmergencyServicesNotSupported => {
                        fidl_ieee80211::StatusCode::RejectedEmergencyServicesNotSupported.into()
                    }
                    fidl_mlme::AssociateResultCode::RefusedTemporarily => {
                        fidl_ieee80211::StatusCode::RefusedTemporarily.into()
                    }
                },
            ),
        }?;
        self.send_wlan_frame(ctx, buffer, written, 0, None)
            .map_err(|s| Error::Status(format!("error sending assoc frame"), s))
    }

    /// Handles MLME-DISASSOCIATE.request (IEEE Std 802.11-2016, 6.3.9.1) from the SME.
    ///
    /// The SME has already disassociated this client.
    ///
    /// The MLME doesn't have to do anything other than change its state to acknowledge the
    /// disassociation.
    pub fn handle_mlme_disassoc_req<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        reason_code: u16,
    ) -> Result<(), Error> {
        self.change_state(ctx, State::Authenticated)?;

        // IEEE Std 802.11-2016, 6.3.9.2.3 states that we should send MLME-DISASSOCIATE.confirm
        // to the SME on success. Like MLME-DEAUTHENTICATE.confirm, our SME has already forgotten
        // about this client, so sending MLME-DISASSOCIATE.confirm is redundant.

        let (buffer, written) =
            ctx.make_disassoc_frame(self.addr.clone(), ReasonCode(reason_code))?;
        self.send_wlan_frame(ctx, buffer, written, 0, None)
            .map_err(|s| Error::Status(format!("error sending disassoc frame"), s))
    }

    /// Handles SET_CONTROLLED_PORT.request (fuchsia.wlan.mlme.SetControlledPortRequest) from the
    /// SME.
    pub fn handle_mlme_set_controlled_port_req(
        &mut self,
        state: fidl_mlme::ControlledPortState,
    ) -> Result<(), Error> {
        match self.state.as_mut() {
            State::Associated {
                eapol_controlled_port: eapol_controlled_port @ Some(_), ..
            } => {
                eapol_controlled_port.replace(state);
                Ok(())
            }
            State::Associated { eapol_controlled_port: None, .. } => {
                Err(Error::Status(format!("client is not in an RSN"), zx::Status::BAD_STATE))
            }
            _ => Err(Error::Status(format!("client is not associated"), zx::Status::BAD_STATE)),
        }
    }

    /// Handles MLME-EAPOL.request (IEEE Std 802.11-2016, 6.3.22.1) from the SME.
    ///
    /// The MLME should forward these frames to the PHY layer.
    pub fn handle_mlme_eapol_req<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        src_addr: MacAddr,
        data: &[u8],
    ) -> Result<(), Error> {
        // IEEE Std 802.11-2016, 6.3.22.2.3 states that we should send MLME-EAPOL.confirm to the
        // SME on success. Our SME employs a timeout for EAPoL negotiation, so MLME-EAPOL.confirm is
        // redundant.
        let (buffer, written) = ctx.make_eapol_frame(self.addr, src_addr, false, data)?;
        self.send_wlan_frame(
            ctx,
            buffer,
            written,
            banjo_softmac::WlanTxInfoFlags::FAVOR_RELIABILITY.0,
            None,
        )
        .map_err(|s| Error::Status(format!("error sending eapol frame"), s))
    }

    // WLAN frame handlers.

    /// Handles disassociation frames (IEEE Std 802.11-2016, 9.3.3.5) from the PHY.
    ///
    /// self is mutable here as receiving a disassociation immediately disassociates us.
    fn handle_disassoc_frame<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        reason_code: ReasonCode,
    ) -> Result<(), ClientRejection> {
        self.change_state(ctx, State::Authenticated).map_err(ClientRejection::DeviceError)?;
        ctx.send_mlme_disassoc_ind(
            self.addr.clone(),
            Option::<fidl_ieee80211::ReasonCode>::from(reason_code)
                .unwrap_or(fidl_ieee80211::ReasonCode::UnspecifiedReason),
            LocallyInitiated(false),
        )
        .map_err(ClientRejection::SmeSendError)
    }

    /// Handles association request frames (IEEE Std 802.11-2016, 9.3.3.6) from the PHY.
    fn handle_assoc_req_frame<D: DeviceOps>(
        &self,
        ctx: &mut Context<D>,
        capabilities: mac::CapabilityInfo,
        listen_interval: u16,
        ssid: Option<Ssid>,
        rates: Vec<ie::SupportedRate>,
        rsne: Option<Vec<u8>>,
    ) -> Result<(), ClientRejection> {
        ctx.send_mlme_assoc_ind(self.addr.clone(), listen_interval, ssid, capabilities, rates, rsne)
            .map_err(ClientRejection::SmeSendError)
    }

    /// Handles authentication frames (IEEE Std 802.11-2016, 9.3.3.12) from the PHY.
    ///
    /// self is mutable here as we may deauthenticate without even getting to the SME if we don't
    /// recognize the authentication algorithm.
    fn handle_auth_frame<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        auth_alg_num: AuthAlgorithmNumber,
    ) -> Result<(), ClientRejection> {
        ctx.send_mlme_auth_ind(
            self.addr.clone(),
            match auth_alg_num {
                AuthAlgorithmNumber::OPEN => fidl_mlme::AuthenticationTypes::OpenSystem,
                AuthAlgorithmNumber::SHARED_KEY => fidl_mlme::AuthenticationTypes::SharedKey,
                AuthAlgorithmNumber::FAST_BSS_TRANSITION => {
                    fidl_mlme::AuthenticationTypes::FastBssTransition
                }
                AuthAlgorithmNumber::SAE => fidl_mlme::AuthenticationTypes::Sae,
                _ => {
                    self.change_state(ctx, State::Deauthenticated)
                        .map_err(ClientRejection::DeviceError)?;

                    // Don't even bother sending this to the SME if we don't understand the auth
                    // algorithm.
                    let (buffer, written) = ctx
                        .make_auth_frame(
                            self.addr.clone(),
                            auth_alg_num,
                            2,
                            fidl_ieee80211::StatusCode::UnsupportedAuthAlgorithm.into(),
                        )
                        .map_err(ClientRejection::WlanSendError)?;
                    return self.send_wlan_frame(ctx, buffer, written, 0, None).map_err(|s| {
                        ClientRejection::WlanSendError(Error::Status(
                            format!("failed to send auth frame"),
                            s,
                        ))
                    });
                }
            },
        )
        .map_err(ClientRejection::SmeSendError)
    }

    /// Handles deauthentication frames (IEEE Std 802.11-2016, 9.3.3.13) from the PHY.
    ///
    /// self is mutable here as receiving a deauthentication immediately deauthenticates us.
    fn handle_deauth_frame<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        reason_code: ReasonCode,
    ) -> Result<(), ClientRejection> {
        self.change_state(ctx, State::Deauthenticated).map_err(ClientRejection::DeviceError)?;
        ctx.send_mlme_deauth_ind(
            self.addr.clone(),
            Option::<fidl_ieee80211::ReasonCode>::from(reason_code)
                .unwrap_or(fidl_ieee80211::ReasonCode::UnspecifiedReason),
            LocallyInitiated(false),
        )
        .map_err(ClientRejection::SmeSendError)
    }

    /// Handles action frames (IEEE Std 802.11-2016, 9.3.3.14) from the PHY.
    fn handle_action_frame<D>(&self, _ctx: &mut Context<D>) -> Result<(), ClientRejection> {
        // TODO(https://fxbug.dev/42113580): Implement me!
        Ok(())
    }

    /// Handles PS-Poll (IEEE Std 802.11-2016, 9.3.1.5) from the PHY.
    pub fn handle_ps_poll<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        aid: Aid,
    ) -> Result<(), ClientRejection> {
        // All PS-Poll frames are Class 3.
        self.reject_frame_class_if_not_permitted(ctx, mac::FrameClass::Class3)?;

        match self.state.as_mut() {
            State::Associated { aid: current_aid, ps_state, .. } => {
                if aid != *current_aid {
                    return Err(ClientRejection::NotPermitted);
                }

                match ps_state {
                    PowerSaveState::Dozing { buffered } => {
                        let BufferedFrame { mut buffer, written, tx_flags, async_id } =
                            match buffered.pop_front() {
                                Some(buffered) => buffered,
                                None => {
                                    // No frames available for the client to PS-Poll, just return
                                    // OK.
                                    return Ok(());
                                }
                            };

                        if !buffered.is_empty() {
                            frame_writer::set_more_data(&mut buffer[..written]).map_err(|e| {
                                wtrace::async_end_wlansoftmac_tx(async_id, zx::Status::INTERNAL);
                                ClientRejection::WlanSendError(e)
                            })?;
                        }

                        ctx.device
                            .send_wlan_frame(buffer.finalize(written), tx_flags, None)
                            .map_err(|s| {
                                wtrace::async_end_wlansoftmac_tx(async_id, s);
                                ClientRejection::WlanSendError(Error::Status(
                                    format!("error sending buffered frame on PS-Poll"),
                                    s,
                                ))
                            })?;
                    }
                    _ => {
                        return Err(ClientRejection::NotPermitted);
                    }
                }
            }
            _ => {
                return Err(ClientRejection::NotAssociated);
            }
        };
        Ok(())
    }

    /// Moves an associated remote client's power saving state into Dozing.
    fn doze(&mut self) -> Result<(), ClientRejection> {
        match self.state.as_mut() {
            State::Associated { ps_state, .. } => match ps_state {
                PowerSaveState::Awake => {
                    *ps_state = PowerSaveState::Dozing {
                        // TODO(https://fxbug.dev/42117877): Impose some kind of limit on this.
                        buffered: VecDeque::new(),
                    }
                }
                PowerSaveState::Dozing { .. } => {}
            },
            _ => {
                // Unassociated clients are never allowed to doze.
                return Err(ClientRejection::NotAssociated);
            }
        };
        Ok(())
    }

    /// Moves an associated remote client's power saving state into Awake.
    ///
    /// This will also send all buffered frames.
    fn wake<D: DeviceOps>(&mut self, ctx: &mut Context<D>) -> Result<(), ClientRejection> {
        match self.state.as_mut() {
            State::Associated { ps_state, .. } => {
                let mut old_ps_state = PowerSaveState::Awake;
                std::mem::swap(ps_state, &mut old_ps_state);

                let mut buffered = match old_ps_state {
                    PowerSaveState::Awake => {
                        // It is not an error to go from awake to awake.
                        return Ok(());
                    }
                    PowerSaveState::Dozing { buffered } => buffered.into_iter().peekable(),
                };

                while let Some(BufferedFrame { mut buffer, written, tx_flags, async_id }) =
                    buffered.next()
                {
                    if buffered.peek().is_some() {
                        // We need to mark all except the last of these frames' frame control fields
                        // with More Data, as per IEEE Std 802.11-2016, 11.2.3.2: The Power
                        // Management subfield(s) in the Frame Control field of the frame(s) sent by
                        // the STA in this exchange indicates the power management mode that the STA
                        // shall adopt upon successful completion of the entire frame exchange.
                        //
                        // As the client does not complete the entire frame exchange until all
                        // buffered frames are sent, we consider the client to be dozing until we
                        // finish sending it all its frames. As per IEEE Std 802.11-2016, 9.2.4.1.8,
                        // we need to mark all frames except the last frame with More Data.
                        frame_writer::set_more_data(&mut buffer[..written])
                            .map_err(ClientRejection::WlanSendError)?;
                    }
                    ctx.device
                        .send_wlan_frame(buffer.finalize(written), tx_flags, Some(async_id))
                        .map_err(|s| {
                            ClientRejection::WlanSendError(Error::Status(
                                format!("error sending buffered frame on wake"),
                                s,
                            ))
                        })?;
                }
            }
            _ => {
                // Unassociated clients are always awake.
                return Ok(());
            }
        };
        Ok(())
    }

    pub fn set_power_state<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        power_state: mac::PowerState,
    ) -> Result<(), ClientRejection> {
        match power_state {
            mac::PowerState::AWAKE => self.wake(ctx),
            mac::PowerState::DOZE => self.doze(),
        }
    }

    /// Handles EAPoL requests (IEEE Std 802.1X-2010, 11.3) from PHY data frames.
    fn handle_eapol_llc_frame<D: DeviceOps>(
        &self,
        ctx: &mut Context<D>,
        dst_addr: MacAddr,
        src_addr: MacAddr,
        body: &[u8],
    ) -> Result<(), ClientRejection> {
        ctx.send_mlme_eapol_ind(dst_addr, src_addr, &body).map_err(ClientRejection::SmeSendError)
    }

    // Handles LLC frames from PHY data frames.
    fn handle_llc_frame<D: DeviceOps>(
        &self,
        ctx: &mut Context<D>,
        dst_addr: MacAddr,
        src_addr: MacAddr,
        ether_type: u16,
        body: &[u8],
    ) -> Result<(), ClientRejection> {
        ctx.deliver_eth_frame(dst_addr, src_addr, ether_type, body)
            .map_err(ClientRejection::EthSendError)
    }

    /// Checks if a given frame class is permitted, and sends an appropriate deauthentication or
    /// disassociation frame if it is not.
    ///
    /// If a frame is sent, the client's state is not in sync with the AP's, e.g. the AP may have
    /// been restarted and the client needs to reset its state.
    fn reject_frame_class_if_not_permitted<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        frame_class: FrameClass,
    ) -> Result<(), ClientRejection> {
        if self.is_frame_class_permitted(frame_class) {
            return Ok(());
        }

        let reason_code = match frame_class {
            FrameClass::Class1 => panic!("class 1 frames should always be permitted"),
            FrameClass::Class2 => fidl_ieee80211::ReasonCode::InvalidClass2Frame,
            FrameClass::Class3 => fidl_ieee80211::ReasonCode::InvalidClass3Frame,
        };

        // Safe: |state| is never None and always replaced with Some(..).
        match self.state.as_ref() {
            State::Deauthenticated | State::Authenticating => {
                let (buffer, written) = ctx
                    .make_deauth_frame(self.addr, reason_code.into())
                    .map_err(ClientRejection::WlanSendError)?;
                self.send_wlan_frame(ctx, buffer, written, 0, None).map_err(|s| {
                    ClientRejection::WlanSendError(Error::Status(
                        format!("failed to send deauth frame"),
                        s,
                    ))
                })?;

                ctx.send_mlme_deauth_ind(self.addr, reason_code, LocallyInitiated(true))
                    .map_err(ClientRejection::SmeSendError)?;
            }
            State::Authenticated => {
                let (buffer, written) = ctx
                    .make_disassoc_frame(self.addr, reason_code.into())
                    .map_err(ClientRejection::WlanSendError)?;
                self.send_wlan_frame(ctx, buffer, written, 0, None).map_err(|s| {
                    ClientRejection::WlanSendError(Error::Status(
                        format!("failed to send disassoc frame"),
                        s,
                    ))
                })?;

                ctx.send_mlme_disassoc_ind(self.addr, reason_code, LocallyInitiated(true))
                    .map_err(ClientRejection::SmeSendError)?;
            }
            State::Associated { .. } => {
                panic!("all frames should be permitted for an associated client")
            }
        };

        return Err(ClientRejection::NotPermitted);
    }

    // Public handler functions.

    /// Handles management frames (IEEE Std 802.11-2016, 9.3.3) from the PHY.
    pub fn handle_mgmt_frame<B: ByteSlice, D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        capabilities: mac::CapabilityInfo,
        ssid: Option<Ssid>,
        mgmt_hdr: mac::MgmtHdr,
        body: B,
    ) -> Result<(), ClientRejection> {
        let mgmt_subtype = *&{ mgmt_hdr.frame_ctrl }.mgmt_subtype();

        self.reject_frame_class_if_not_permitted(ctx, mac::frame_class(&{ mgmt_hdr.frame_ctrl }))?;

        self.reset_bss_max_idle_timeout(ctx);

        match mac::MgmtBody::parse(mgmt_subtype, body).ok_or(ClientRejection::ParseFailed)? {
            mac::MgmtBody::Authentication { auth_hdr, .. } => {
                self.handle_auth_frame(ctx, auth_hdr.auth_alg_num)
            }
            mac::MgmtBody::AssociationReq { assoc_req_hdr, elements } => {
                let mut rates = vec![];
                let mut rsne = None;

                // TODO(https://fxbug.dev/42164332): This should probably use IeSummaryIter instead.
                for (id, ie_body) in ie::Reader::new(&elements[..]) {
                    match id {
                        // We don't try too hard to verify the supported rates and extended
                        // supported rates provided. A warning is logged if parsing of either
                        // did not succeed, but otherwise whatever rates are parsed, even if
                        // none, are passed on to SME.
                        ie::Id::SUPPORTED_RATES => {
                            match ie::parse_supported_rates(ie_body) {
                                Ok(supported_rates) => rates.extend(&*supported_rates),
                                Err(e) => warn!("{:?}", e),
                            };
                        }
                        ie::Id::EXTENDED_SUPPORTED_RATES => {
                            match ie::parse_extended_supported_rates(ie_body) {
                                Ok(extended_supported_rates) => {
                                    rates.extend(&*extended_supported_rates)
                                }
                                Err(e) => warn!("{:?}", e),
                            };
                        }
                        ie::Id::RSNE => {
                            rsne = Some({
                                // TODO(https://fxbug.dev/42117156): Stop passing RSNEs around like this.
                                let mut buffer =
                                    vec![0; std::mem::size_of::<ie::Header>() + ie_body.len()];
                                let mut w = BufferWriter::new(&mut buffer[..]);
                                w.append_value(&ie::Header {
                                    id: ie::Id::RSNE,
                                    body_len: ie_body.len() as u8,
                                })
                                .expect("expected enough room in buffer for IE header");
                                w.append_bytes(ie_body)
                                    .expect("expected enough room in buffer for IE body");
                                buffer
                            });
                        }
                        _ => {}
                    }
                }

                self.handle_assoc_req_frame(
                    ctx,
                    capabilities,
                    assoc_req_hdr.listen_interval,
                    ssid,
                    rates,
                    rsne,
                )
            }
            mac::MgmtBody::Deauthentication { deauth_hdr, .. } => {
                self.handle_deauth_frame(ctx, deauth_hdr.reason_code)
            }
            mac::MgmtBody::Disassociation { disassoc_hdr, .. } => {
                self.handle_disassoc_frame(ctx, disassoc_hdr.reason_code)
            }
            mac::MgmtBody::Action { action_hdr: _, .. } => self.handle_action_frame(ctx),
            _ => Err(ClientRejection::Unsupported),
        }
    }

    /// Handles data frames (IEEE Std 802.11-2016, 9.3.2) from the PHY.
    ///
    /// These data frames may be in A-MSDU format (IEEE Std 802.11-2016, 9.3.2.2). However, the
    /// individual frames will be passed to |handle_msdu| and we don't need to care what format
    /// they're in.
    pub fn handle_data_frame<B: ByteSlice, D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        fixed_data_fields: mac::FixedDataHdrFields,
        addr4: Option<mac::Addr4>,
        qos_ctrl: Option<mac::QosControl>,
        body: B,
    ) -> Result<(), ClientRejection> {
        self.reject_frame_class_if_not_permitted(
            ctx,
            mac::frame_class(&{ fixed_data_fields.frame_ctrl }),
        )?;

        self.reset_bss_max_idle_timeout(ctx);

        for msdu in
            mac::MsduIterator::from_data_frame_parts(fixed_data_fields, addr4, qos_ctrl, body)
        {
            let mac::Msdu { dst_addr, src_addr, llc_frame } = &msdu;
            match llc_frame.hdr.protocol_id.to_native() {
                // Handle EAPOL LLC frames.
                mac::ETHER_TYPE_EAPOL => {
                    self.handle_eapol_llc_frame(ctx, *dst_addr, *src_addr, &llc_frame.body)?
                }
                // Non-EAPOL frames...
                _ => match self.state.as_ref() {
                    // Drop all non-EAPoL MSDUs if the controlled port is closed.
                    State::Associated {
                        eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Closed),
                        ..
                    } => (),
                    // Handle LLC frames only if the controlled port is not closed and the frame type
                    // is not EAPOL. If there is no controlled port, sending frames is OK.
                    _ => self.handle_llc_frame(
                        ctx,
                        *dst_addr,
                        *src_addr,
                        llc_frame.hdr.protocol_id.to_native(),
                        &llc_frame.body,
                    )?,
                },
            }
        }
        Ok(())
    }

    /// Handles Ethernet II frames from the netstack.
    pub fn handle_eth_frame<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        dst_addr: MacAddr,
        src_addr: MacAddr,
        ether_type: u16,
        body: &[u8],
        async_id: trace::Id,
    ) -> Result<(), ClientRejection> {
        let eapol_controlled_port = match self.state.as_ref() {
            State::Associated { eapol_controlled_port, .. } => eapol_controlled_port,
            _ => {
                return Err(ClientRejection::NotAssociated);
            }
        };

        let protection = match eapol_controlled_port {
            None => false,
            Some(fidl_mlme::ControlledPortState::Open) => true,
            Some(fidl_mlme::ControlledPortState::Closed) => {
                return Err(ClientRejection::ControlledPortClosed);
            }
        };

        let (buffer, written) = ctx
            .make_data_frame(
                dst_addr, src_addr, protection,
                false, // TODO(https://fxbug.dev/42113580): Support QoS.
                ether_type, body,
            )
            .map_err(ClientRejection::WlanSendError)?;

        self.send_wlan_frame(ctx, buffer, written, 0, Some(async_id)).map_err(move |s| {
            ClientRejection::WlanSendError(Error::Status(format!("error sending eapol frame"), s))
        })
    }

    pub fn send_wlan_frame<D: DeviceOps>(
        &mut self,
        ctx: &mut Context<D>,
        buffer: Buffer,
        written: usize,
        tx_flags: u32,
        async_id: Option<trace::Id>,
    ) -> Result<(), zx::Status> {
        let async_id = async_id.unwrap_or_else(|| {
            let async_id = trace::Id::new();
            wtrace::async_begin_wlansoftmac_tx(async_id, "mlme");
            async_id
        });

        match self.state.as_mut() {
            State::Associated { ps_state, .. } => match ps_state {
                PowerSaveState::Awake => {
                    ctx.device.send_wlan_frame(buffer.finalize(written), tx_flags, Some(async_id))
                }
                PowerSaveState::Dozing { buffered } => {
                    buffered.push_back(BufferedFrame { buffer, written, tx_flags, async_id });
                    Ok(())
                }
            },
            _ => ctx.device.send_wlan_frame(buffer.finalize(written), tx_flags, Some(async_id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{ap::TimedEvent, buffer::FakeCBufferProvider, device::FakeDevice},
        fuchsia_async as fasync,
        ieee80211::Bssid,
        lazy_static::lazy_static,
        std::convert::TryFrom,
        test_case::test_case,
        wlan_common::{
            assert_variant,
            mac::CapabilityInfo,
            test_utils::fake_frames::*,
            timer::{self, create_timer},
        },
    };

    lazy_static! {
        static ref CLIENT_ADDR: MacAddr = [1; 6].into();
        static ref AP_ADDR: Bssid = [2; 6].into();
        static ref CLIENT_ADDR2: MacAddr = [3; 6].into();
    }
    fn make_remote_client() -> RemoteClient {
        RemoteClient::new(*CLIENT_ADDR)
    }

    fn make_context(
        fake_device: FakeDevice,
    ) -> (Context<FakeDevice>, timer::EventStream<TimedEvent>) {
        let (timer, time_stream) = create_timer();
        (Context::new(fake_device, FakeCBufferProvider::new(), timer, *AP_ADDR), time_stream)
    }

    #[test]
    fn handle_mlme_auth_resp() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_mlme_auth_resp(&mut ctx, fidl_mlme::AuthenticateResultCode::Success)
            .expect("expected OK");
        assert_variant!(r_sta.state.as_ref(), State::Authenticated);
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b10110000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Auth header:
            0, 0, // auth algorithm
            2, 0, // auth txn seq num
            0, 0, // status code
        ][..]);
    }

    #[test]
    fn handle_mlme_auth_resp_failure() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_mlme_auth_resp(
                &mut ctx,
                fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired,
            )
            .expect("expected OK");
        assert_variant!(r_sta.state.as_ref(), State::Deauthenticated);
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b10110000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Auth header:
            0, 0, // auth algorithm
            2, 0, // auth txn seq num
            76, 0, // status code
        ][..]);
    }

    #[test_case(State::Authenticating; "in authenticating state")]
    #[test_case(State::Authenticated; "in authenticated state")]
    #[test_case(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        }; "in associated state")]
    fn handle_mlme_deauth_req(init_state: State) {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(init_state);
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_mlme_deauth_req(&mut ctx, fidl_ieee80211::ReasonCode::LeavingNetworkDeauth)
            .expect("expected OK");
        assert_variant!(r_sta.state.as_ref(), State::Deauthenticated);
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b11000000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Deauth header:
            3, 0, // reason code
        ][..]);
    }

    #[test]
    fn handle_mlme_assoc_resp() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, mut time_stream) = make_context(fake_device);
        r_sta
            .handle_mlme_assoc_resp(
                &mut ctx,
                true,
                1,
                CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::Success,
                1,
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10][..],
            )
            .expect("expected OK");

        assert_variant!(
            r_sta.state.as_ref(),
            State::Associated {
                eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Closed),
                ..
            }
        );

        assert_variant!(r_sta.aid(), Some(aid) => {
            assert_eq!(aid, 1);
        });

        let active_timeout_event_id = match r_sta.state.as_ref() {
            State::Associated {
                active_timeout_event_id: Some(active_timeout_event_id), ..
            } => active_timeout_event_id,
            _ => panic!("no active timeout?"),
        };

        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b00010000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Association response header:
            0, 0, // Capabilities
            0, 0, // status code
            1, 0, // AID
            // IEs
            1, 8, 1, 2, 3, 4, 5, 6, 7, 8, // Rates
            50, 2, 9, 10, // Extended rates
            90, 3, 90, 0, 0, // BSS max idle period
        ][..]);
        let (_, timed_event) =
            time_stream.try_next().unwrap().expect("Should have scheduled a timeout");
        assert_eq!(timed_event.id, *active_timeout_event_id);

        assert!(fake_device_state.lock().assocs.contains_key(&CLIENT_ADDR));
    }

    #[test]
    fn handle_mlme_assoc_resp_then_handle_mlme_disassoc_req() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta
            .handle_mlme_assoc_resp(
                &mut ctx,
                true,
                1,
                CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::Success,
                1,
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10][..],
            )
            .expect("expected OK");
        assert!(fake_device_state.lock().assocs.contains_key(&CLIENT_ADDR));

        r_sta
            .handle_mlme_disassoc_req(
                &mut ctx,
                fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc.into_primitive(),
            )
            .expect("expected OK");
        assert!(!fake_device_state.lock().assocs.contains_key(&CLIENT_ADDR));
    }

    #[test]
    fn handle_mlme_assoc_resp_then_handle_mlme_deauth_req() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta
            .handle_mlme_assoc_resp(
                &mut ctx,
                true,
                1,
                CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::Success,
                1,
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10][..],
            )
            .expect("expected OK");
        assert!(fake_device_state.lock().assocs.contains_key(&CLIENT_ADDR));

        r_sta
            .handle_mlme_deauth_req(&mut ctx, fidl_ieee80211::ReasonCode::LeavingNetworkDeauth)
            .expect("expected OK");
        assert!(!fake_device_state.lock().assocs.contains_key(&CLIENT_ADDR));
    }

    #[test]
    fn handle_mlme_assoc_resp_no_rsn() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_mlme_assoc_resp(
                &mut ctx,
                false,
                1,
                CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::Success,
                1,
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10][..],
            )
            .expect("expected OK");
        assert_variant!(
            r_sta.state.as_ref(),
            State::Associated { eapol_controlled_port: None, active_timeout_event_id: Some(_), .. }
        );
    }

    #[test]
    fn handle_mlme_assoc_resp_failure_reason_unspecified() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_mlme_assoc_resp(
                &mut ctx,
                false,
                1,
                CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::RefusedReasonUnspecified,
                1, // This AID is ignored in the case of an error.
                &[][..],
            )
            .expect("expected OK");
        assert_variant!(r_sta.state.as_ref(), State::Authenticated);
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b00010000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Association response header:
            0, 0, // Capabilities
            1, 0, // status code
            0, 0, // AID
        ][..]);
    }

    #[test]
    fn handle_mlme_assoc_resp_failure_emergency_services_not_supported() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_mlme_assoc_resp(
                &mut ctx,
                false,
                1,
                CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::RejectedEmergencyServicesNotSupported,
                1, // This AID is ignored in the case of an error.
                &[][..],
            )
            .expect("expected OK");
        assert_variant!(r_sta.state.as_ref(), State::Authenticated);
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b00010000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Association response header:
            0, 0, // Capabilities
            94, 0, // status code
            0, 0, // AID
        ][..]);
    }

    #[test]
    fn handle_mlme_disassoc_req() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_mlme_disassoc_req(
                &mut ctx,
                fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc.into_primitive(),
            )
            .expect("expected OK");
        assert_variant!(r_sta.state.as_ref(), State::Authenticated);
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b10100000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Disassoc header:
            8, 0, // reason code
        ][..]);
    }

    #[test]
    fn handle_mlme_set_controlled_port_req() {
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Closed),
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        r_sta
            .handle_mlme_set_controlled_port_req(fidl_mlme::ControlledPortState::Open)
            .expect("expected OK");
        assert_variant!(
            r_sta.state.as_ref(),
            State::Associated {
                eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Open),
                ..
            }
        );
    }

    #[test]
    fn handle_mlme_set_controlled_port_req_closed() {
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Open),
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        r_sta
            .handle_mlme_set_controlled_port_req(fidl_mlme::ControlledPortState::Closed)
            .expect("expected OK");
        assert_variant!(
            r_sta.state.as_ref(),
            State::Associated {
                eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Closed),
                ..
            }
        );
    }

    #[test]
    fn handle_mlme_set_controlled_port_req_no_rsn() {
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        assert_eq!(
            zx::Status::from(
                r_sta
                    .handle_mlme_set_controlled_port_req(fidl_mlme::ControlledPortState::Open)
                    .expect_err("expected err")
            ),
            zx::Status::BAD_STATE
        );
        assert_variant!(
            r_sta.state.as_ref(),
            State::Associated { eapol_controlled_port: None, .. }
        );
    }

    #[test]
    fn handle_mlme_set_controlled_port_req_wrong_state() {
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticating);
        assert_eq!(
            zx::Status::from(
                r_sta
                    .handle_mlme_set_controlled_port_req(fidl_mlme::ControlledPortState::Open)
                    .expect_err("expected err")
            ),
            zx::Status::BAD_STATE
        );
    }

    #[test]
    fn handle_mlme_eapol_req() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta.handle_mlme_eapol_req(&mut ctx, *CLIENT_ADDR2, &[1, 2, 3][..]).expect("expected OK");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b00001000, 0b00000010, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            3, 3, 3, 3, 3, 3, // addr3
            0x10, 0, // Sequence Control
            0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
            0, 0, 0, // OUI
            0x88, 0x8E, // EAPOL protocol ID
            // Data
            1, 2, 3,
        ][..]);
    }

    #[test]
    fn handle_disassoc_frame() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_disassoc_frame(
                &mut ctx,
                ReasonCode(fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc.into_primitive()),
            )
            .expect("expected OK");

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DisassociateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::DisassociateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc,
                locally_initiated: false,
            },
        );
        assert_variant!(r_sta.state.as_ref(), State::Authenticated);
    }

    #[test_case(State::Authenticating; "in authenticating state")]
    #[test_case(State::Authenticated; "in authenticated state")]
    #[test_case(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        }; "in associated state")]
    fn handle_assoc_req_frame(init_state: State) {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(init_state);
        let (mut ctx, _) = make_context(fake_device);
        r_sta
            .handle_assoc_req_frame(
                &mut ctx,
                CapabilityInfo(0).with_short_preamble(true),
                1,
                Some(Ssid::try_from("coolnet").unwrap()),
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10].iter().map(|r| ie::SupportedRate(*r)).collect(),
                None,
            )
            .expect("expected OK");

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::AssociateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::AssociateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                listen_interval: 1,
                ssid: Some(Ssid::try_from("coolnet").unwrap().into()),
                capability_info: CapabilityInfo(0).with_short_preamble(true).raw(),
                rates: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
                rsne: None,
            },
        );
    }

    #[test_case(State::Authenticating; "in authenticating state")]
    #[test_case(State::Authenticated; "in authenticated state")]
    #[test_case(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        }; "in associated state")]
    fn handle_auth_frame(init_state: State) {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(init_state);
        let (mut ctx, _) = make_context(fake_device);

        r_sta.handle_auth_frame(&mut ctx, AuthAlgorithmNumber::SHARED_KEY).expect("expected OK");
        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::AuthenticateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::AuthenticateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                auth_type: fidl_mlme::AuthenticationTypes::SharedKey,
            },
        );
    }

    #[test]
    fn handle_auth_frame_unknown_algorithm() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta.handle_auth_frame(&mut ctx, AuthAlgorithmNumber(0xffff)).expect("expected OK");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b10110000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Auth header:
            0xff, 0xff, // auth algorithm
            2, 0, // auth txn seq num
            13, 0, // status code
        ][..]);
        assert_variant!(r_sta.state.as_ref(), State::Deauthenticated);
    }

    #[test_case(false; "from idle state")]
    #[test_case(true; "while already authenticated")]
    fn handle_deauth_frame(already_authenticated: bool) {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        if already_authenticated {
            r_sta.state = StateMachine::new(State::Authenticated);
        }
        let (mut ctx, _) = make_context(fake_device);

        r_sta
            .handle_deauth_frame(
                &mut ctx,
                ReasonCode(fidl_ieee80211::ReasonCode::LeavingNetworkDeauth.into_primitive()),
            )
            .expect("expected OK");
        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DeauthenticateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::DeauthenticateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                locally_initiated: false,
            }
        );
        assert_variant!(r_sta.state.as_ref(), State::Deauthenticated);
    }

    #[test]
    fn handle_action_frame() {
        // TODO(https://fxbug.dev/42113580): Implement me!
    }

    #[test]
    fn handle_ps_poll() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });

        r_sta.set_power_state(&mut ctx, mac::PowerState::DOZE).expect("expected doze OK");

        // Send a bunch of Ethernet frames.
        r_sta
            .handle_eth_frame(
                &mut ctx,
                *CLIENT_ADDR2,
                *CLIENT_ADDR,
                0x1234,
                &[1, 2, 3, 4, 5][..],
                0.into(),
            )
            .expect("expected OK");
        r_sta
            .handle_eth_frame(
                &mut ctx,
                *CLIENT_ADDR2,
                *CLIENT_ADDR,
                0x1234,
                &[6, 7, 8, 9, 0][..],
                0.into(),
            )
            .expect("expected OK");

        // Make sure nothing has been actually sent to the WLAN queue.
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 0);

        r_sta.handle_ps_poll(&mut ctx, 1).expect("expected handle_ps_poll OK");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b00001000, 0b00100010, // Frame Control
                0, 0, // Duration
                3, 3, 3, 3, 3, 3, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                1, 1, 1, 1, 1, 1, // addr3
                0x10, 0, // Sequence Control
                0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
                0, 0, 0, // OUI
                0x12, 0x34, // Protocol ID
                // Data
                1, 2, 3, 4, 5,
            ][..]
        );

        r_sta.handle_ps_poll(&mut ctx, 1).expect("expected handle_ps_poll OK");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 2);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[1].0[..],
            &[
                // Mgmt header
                0b00001000, 0b00000010, // Frame Control
                0, 0, // Duration
                3, 3, 3, 3, 3, 3, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                1, 1, 1, 1, 1, 1, // addr3
                0x20, 0, // Sequence Control
                0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
                0, 0, 0, // OUI
                0x12, 0x34, // Protocol ID
                // Data
                6, 7, 8, 9, 0,
            ][..]
        );

        r_sta.handle_ps_poll(&mut ctx, 1).expect("expected handle_ps_poll OK");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 2);
    }

    #[test]
    fn handle_ps_poll_not_buffered() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });

        r_sta.set_power_state(&mut ctx, mac::PowerState::DOZE).expect("expected doze OK");

        r_sta.handle_ps_poll(&mut ctx, 1).expect("expected handle_ps_poll OK");
    }

    #[test]
    fn handle_ps_poll_wrong_aid() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });

        r_sta.set_power_state(&mut ctx, mac::PowerState::DOZE).expect("expected doze OK");

        assert_variant!(
            r_sta.handle_ps_poll(&mut ctx, 2).expect_err("expected handle_ps_poll error"),
            ClientRejection::NotPermitted
        );
    }

    #[test]
    fn handle_ps_poll_not_dozing() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });

        assert_variant!(
            r_sta.handle_ps_poll(&mut ctx, 1).expect_err("expected handle_ps_poll error"),
            ClientRejection::NotPermitted
        );
    }

    #[test]
    fn handle_eapol_llc_frame() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        r_sta
            .handle_eapol_llc_frame(&mut ctx, *CLIENT_ADDR2, *CLIENT_ADDR, &[1, 2, 3, 4, 5][..])
            .expect("expected OK");
        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::EapolIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::EapolIndication {
                dst_addr: CLIENT_ADDR2.to_array(),
                src_addr: CLIENT_ADDR.to_array(),
                data: vec![1, 2, 3, 4, 5],
            },
        );
    }

    #[test]
    fn handle_llc_frame() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        r_sta
            .handle_llc_frame(&mut ctx, *CLIENT_ADDR2, *CLIENT_ADDR, 0x1234, &[1, 2, 3, 4, 5][..])
            .expect("expected OK");
        assert_eq!(fake_device_state.lock().eth_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().eth_queue[0][..], &[
            3, 3, 3, 3, 3, 3,  // dest
            1, 1, 1, 1, 1, 1,  // src
            0x12, 0x34,        // ether_type
            // Data
            1, 2, 3, 4, 5,
        ][..]);
    }

    #[test]
    fn handle_eth_frame_no_eapol_controlled_port() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        r_sta
            .handle_eth_frame(
                &mut ctx,
                *CLIENT_ADDR2,
                *CLIENT_ADDR,
                0x1234,
                &[1, 2, 3, 4, 5][..],
                0.into(),
            )
            .expect("expected OK");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b00001000, 0b00000010, // Frame Control
            0, 0, // Duration
            3, 3, 3, 3, 3, 3, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            1, 1, 1, 1, 1, 1, // addr3
            0x10, 0, // Sequence Control
            0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
            0, 0, 0, // OUI
            0x12, 0x34, // Protocol ID
            // Data
            1, 2, 3, 4, 5,
        ][..]);
    }

    #[test]
    fn handle_eth_frame_not_associated() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta.state = StateMachine::new(State::Authenticated);
        assert_variant!(
            r_sta
                .handle_eth_frame(
                    &mut ctx,
                    *CLIENT_ADDR2,
                    *CLIENT_ADDR,
                    0x1234,
                    &[1, 2, 3, 4, 5][..],
                    0.into()
                )
                .expect_err("expected error"),
            ClientRejection::NotAssociated
        );
    }

    #[test]
    fn handle_eth_frame_eapol_controlled_port_closed() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Closed),
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        assert_variant!(
            r_sta
                .handle_eth_frame(
                    &mut ctx,
                    *CLIENT_ADDR2,
                    *CLIENT_ADDR,
                    0x1234,
                    &[1, 2, 3, 4, 5][..],
                    0.into()
                )
                .expect_err("expected error"),
            ClientRejection::ControlledPortClosed
        );
    }

    #[test]
    fn handle_eth_frame_eapol_controlled_port_open() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        let (mut ctx, _) = make_context(fake_device);

        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: Some(fidl_mlme::ControlledPortState::Open),
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        r_sta
            .handle_eth_frame(
                &mut ctx,
                *CLIENT_ADDR2,
                *CLIENT_ADDR,
                0x1234,
                &[1, 2, 3, 4, 5][..],
                0.into(),
            )
            .expect("expected OK");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b00001000, 0b01000010, // Frame Control
            0, 0, // Duration
            3, 3, 3, 3, 3, 3, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            1, 1, 1, 1, 1, 1, // addr3
            0x10, 0, // Sequence Control
            0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
            0, 0, 0, // OUI
            0x12, 0x34, // Protocol ID
            // Data
            1, 2, 3, 4, 5,
        ][..]);
    }

    #[test]
    fn handle_data_frame_not_permitted() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticating);
        let (mut ctx, _) = make_context(fake_device);

        assert_variant!(
            r_sta
                .handle_data_frame(
                    &mut ctx,
                    mac::FixedDataHdrFields {
                        frame_ctrl: mac::FrameControl(0b000000010_00001000),
                        duration: 0,
                        addr1: *CLIENT_ADDR,
                        addr2: (*AP_ADDR).into(),
                        addr3: *CLIENT_ADDR2,
                        seq_ctrl: mac::SequenceControl(10),
                    },
                    None,
                    None,
                    &[
                        7, 7, 7, // DSAP, SSAP & control
                        8, 8, 8, // OUI
                        9, 10, // eth type
                        // Trailing bytes
                        11, 11, 11,
                    ][..],
                )
                .expect_err("expected err"),
            ClientRejection::NotPermitted
        );

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DeauthenticateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::DeauthenticateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::InvalidClass3Frame,
                locally_initiated: true,
            },
        );

        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            fake_device_state.lock().wlan_queue[0].0,
            &[
                // Mgmt header
                0b11000000, 0b00000000, // Frame Control
                0, 0, // Duration
                1, 1, 1, 1, 1, 1, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Deauth header:
                7, 0, // reason code
            ][..]
        );
    }

    #[test]
    fn handle_data_frame_not_permitted_disassoc() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticated);
        let (mut ctx, _) = make_context(fake_device);

        assert_variant!(
            r_sta
                .handle_data_frame(
                    &mut ctx,
                    mac::FixedDataHdrFields {
                        frame_ctrl: mac::FrameControl(0b000000010_00001000),
                        duration: 0,
                        addr1: *CLIENT_ADDR,
                        addr2: (*AP_ADDR).into(),
                        addr3: *CLIENT_ADDR2,
                        seq_ctrl: mac::SequenceControl(10),
                    },
                    None,
                    None,
                    &[
                        7, 7, 7, // DSAP, SSAP & control
                        8, 8, 8, // OUI
                        9, 10, // eth type
                        // Trailing bytes
                        11, 11, 11,
                    ][..],
                )
                .expect_err("expected err"),
            ClientRejection::NotPermitted
        );

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DisassociateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::DisassociateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::InvalidClass3Frame,
                locally_initiated: true,
            },
        );

        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            fake_device_state.lock().wlan_queue[0].0,
            &[
                // Mgmt header
                0b10100000, 0b00000000, // Frame Control
                0, 0, // Duration
                1, 1, 1, 1, 1, 1, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Deauth header:
                7, 0, // reason code
            ][..]
        );
    }

    #[test]
    fn handle_data_frame_single_llc() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        let (mut ctx, _) = make_context(fake_device);

        r_sta
            .handle_data_frame(
                &mut ctx,
                mac::FixedDataHdrFields {
                    frame_ctrl: mac::FrameControl(0b000000010_00001000),
                    duration: 0,
                    addr1: *CLIENT_ADDR,
                    addr2: (*AP_ADDR).into(),
                    addr3: *CLIENT_ADDR2,
                    seq_ctrl: mac::SequenceControl(10),
                },
                None,
                None,
                &[
                    7, 7, 7, // DSAP, SSAP & control
                    8, 8, 8, // OUI
                    9, 10, // eth type
                    // Trailing bytes
                    11, 11, 11,
                ][..],
            )
            .expect("expected OK");

        assert_eq!(fake_device_state.lock().eth_queue.len(), 1);
        assert_ne!(
            match r_sta.state.as_ref() {
                State::Associated { active_timeout_event_id, .. } => *active_timeout_event_id,
                _ => panic!("expected Associated"),
            },
            None
        )
    }

    #[test]
    fn handle_data_frame_amsdu() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        let (mut ctx, _) = make_context(fake_device);

        let mut amsdu_data_frame_body = vec![];
        amsdu_data_frame_body.extend(&[
            // A-MSDU Subframe #1
            0x78, 0x8a, 0x20, 0x0d, 0x67, 0x03, // dst_addr
            0xb4, 0xf7, 0xa1, 0xbe, 0xb9, 0xab, // src_addr
            0x00, 0x74, // MSDU length
        ]);
        amsdu_data_frame_body.extend(MSDU_1_LLC_HDR);
        amsdu_data_frame_body.extend(MSDU_1_PAYLOAD);
        amsdu_data_frame_body.extend(&[
            // Padding
            0x00, 0x00, // A-MSDU Subframe #2
            0x78, 0x8a, 0x20, 0x0d, 0x67, 0x04, // dst_addr
            0xb4, 0xf7, 0xa1, 0xbe, 0xb9, 0xac, // src_addr
            0x00, 0x66, // MSDU length
        ]);
        amsdu_data_frame_body.extend(MSDU_2_LLC_HDR);
        amsdu_data_frame_body.extend(MSDU_2_PAYLOAD);

        r_sta
            .handle_data_frame(
                &mut ctx,
                mac::FixedDataHdrFields {
                    frame_ctrl: mac::FrameControl(0b000000010_00001000),
                    duration: 0,
                    addr1: *CLIENT_ADDR,
                    addr2: (*AP_ADDR).into(),
                    addr3: *CLIENT_ADDR2,
                    seq_ctrl: mac::SequenceControl(10),
                },
                None,
                Some(mac::QosControl(0).with_amsdu_present(true)),
                &amsdu_data_frame_body[..],
            )
            .expect("expected OK");

        assert_eq!(fake_device_state.lock().eth_queue.len(), 2);
        assert_ne!(
            match r_sta.state.as_ref() {
                State::Associated { active_timeout_event_id, .. } => *active_timeout_event_id,
                _ => panic!("expected Associated"),
            },
            None
        )
    }

    #[test]
    fn handle_mgmt_frame() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticating);
        let (mut ctx, _) = make_context(fake_device);

        r_sta
            .handle_mgmt_frame(
                &mut ctx,
                mac::CapabilityInfo(0),
                None,
                mac::MgmtHdr {
                    frame_ctrl: mac::FrameControl(0b00000000_10110000), // Auth frame
                    duration: 0,
                    addr1: [1; 6].into(),
                    addr2: [2; 6].into(),
                    addr3: [3; 6].into(),
                    seq_ctrl: mac::SequenceControl(10),
                },
                &[
                    0, 0, // Auth algorithm number
                    1, 0, // Auth txn seq number
                    0, 0, // Status code
                ][..],
            )
            .expect("expected OK");
    }

    #[test_case(Ssid::try_from("coolnet").unwrap(), true; "with ssid and rsne")]
    #[test_case(Ssid::try_from("").unwrap(), true; "with empty ssid")]
    #[test_case(Ssid::try_from("coolnet").unwrap(), false; "without rsne")]
    fn handle_mgmt_frame_assoc_req(ssid: Ssid, has_rsne: bool) {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticated);
        let (mut ctx, _) = make_context(fake_device);

        let mut assoc_frame_body = vec![
            0, 0, // Capability info
            10, 0, // Listen interval
            // IEs
            1, 8, 1, 2, 3, 4, 5, 6, 7, 8, // Rates
            50, 2, 9, 10, // Extended rates
        ];
        if has_rsne {
            assoc_frame_body.extend(&[48, 2, 77, 88][..]); // RSNE
        }

        r_sta
            .handle_mgmt_frame(
                &mut ctx,
                mac::CapabilityInfo(0),
                Some(ssid.clone()),
                mac::MgmtHdr {
                    frame_ctrl: mac::FrameControl(0b00000000_00000000), // Assoc req frame
                    duration: 0,
                    addr1: [1; 6].into(),
                    addr2: [2; 6].into(),
                    addr3: [3; 6].into(),
                    seq_ctrl: mac::SequenceControl(10),
                },
                &assoc_frame_body[..],
            )
            .expect("expected OK");

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::AssociateIndication>()
            .expect("expected MLME message");
        let expected_rsne = if has_rsne { Some(vec![48, 2, 77, 88]) } else { None };
        assert_eq!(
            msg,
            fidl_mlme::AssociateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                listen_interval: 10,
                ssid: Some(ssid.into()),
                capability_info: CapabilityInfo(0).raw(),
                rates: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
                rsne: expected_rsne,
            },
        );
    }

    #[test_case(vec![1, 0],
                vec![50, 2, 9, 10],
                vec![9, 10] ; "when no supported rates")]
    #[test_case(vec![1, 8, 1, 2, 3, 4, 5, 6, 7, 8],
                vec![50, 0],
                vec![1, 2, 3, 4, 5, 6, 7, 8] ; "when no extended supported rates")]
    #[test_case(vec![1, 0],
                vec![50, 0],
                vec![] ; "when no rates")]
    // This case expects the Supported Rates to reach SME successfully despite the number of rates
    // exceeding the limit of eight specified in IEEE Std 802.11-2016 9.2.4.3. This limit is
    // ignored while parsing rates to improve interoperability with devices that overload the IE.
    #[test_case(vec![1, 9, 1, 2, 3, 4, 5, 6, 7, 8, 9],
                vec![50, 9, 10],
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9] ; "when too many supported rates")]
    #[fuchsia::test]
    fn assoc_req_with_bad_rates_still_passed_to_sme(
        supported_rates_ie: Vec<u8>,
        extended_supported_rates_ie: Vec<u8>,
        expected_rates: Vec<u8>,
    ) {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticated);
        let (mut ctx, _) = make_context(fake_device);
        let mut ies = vec![
            0, 0, // Capability info
            10, 0, // Listen interval
        ];
        ies.extend(supported_rates_ie);
        ies.extend(extended_supported_rates_ie);

        r_sta
            .handle_mgmt_frame(
                &mut ctx,
                mac::CapabilityInfo(0),
                Some(Ssid::try_from("coolnet").unwrap()),
                mac::MgmtHdr {
                    frame_ctrl: mac::FrameControl(0b00000000_00000000), // Assoc req frame
                    duration: 0,
                    addr1: [1; 6].into(),
                    addr2: [2; 6].into(),
                    addr3: [3; 6].into(),
                    seq_ctrl: mac::SequenceControl(10),
                },
                &ies[..],
            )
            .expect("parsing should not fail");

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::AssociateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::AssociateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                listen_interval: 10,
                ssid: Some(Ssid::try_from("coolnet").unwrap().into()),
                capability_info: CapabilityInfo(0).raw(),
                rates: expected_rates,
                rsne: None,
            },
        );
    }

    #[test]
    fn handle_mgmt_frame_not_permitted() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticating);
        let (mut ctx, _) = make_context(fake_device);

        assert_variant!(
            r_sta
                .handle_mgmt_frame(
                    &mut ctx,
                    mac::CapabilityInfo(0),
                    None,
                    mac::MgmtHdr {
                        frame_ctrl: mac::FrameControl(0b00000000_00000000), // Assoc req frame
                        duration: 0,
                        addr1: [1; 6].into(),
                        addr2: [2; 6].into(),
                        addr3: [3; 6].into(),
                        seq_ctrl: mac::SequenceControl(10),
                    },
                    &[
                        0, 0, // Capability info
                        10, 0, // Listen interval
                    ][..],
                )
                .expect_err("expected error"),
            ClientRejection::NotPermitted
        );

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DeauthenticateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::DeauthenticateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::InvalidClass2Frame,
                locally_initiated: true,
            },
        );

        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            fake_device_state.lock().wlan_queue[0].0,
            &[
                // Mgmt header
                0b11000000, 0b00000000, // Frame Control
                0, 0, // Duration
                1, 1, 1, 1, 1, 1, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Deauth header:
                6, 0, // reason code
            ][..]
        );
    }

    #[test]
    fn handle_mgmt_frame_not_handled() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        let (mut ctx, _) = make_context(fake_device);

        assert_variant!(
            r_sta
                .handle_mgmt_frame(
                    &mut ctx,
                    mac::CapabilityInfo(0),
                    None,
                    mac::MgmtHdr {
                        frame_ctrl: mac::FrameControl(0b00000000_00010000), // Assoc resp frame
                        duration: 0,
                        addr1: [1; 6].into(),
                        addr2: [2; 6].into(),
                        addr3: [3; 6].into(),
                        seq_ctrl: mac::SequenceControl(10),
                    },
                    &[
                        0, 0, // Capability info
                        0, 0, // Status code
                        1, 0, // AID
                    ][..],
                )
                .expect_err("expected error"),
            ClientRejection::Unsupported
        );
    }

    #[test]
    fn handle_mgmt_frame_resets_active_timer() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });
        let (mut ctx, _) = make_context(fake_device);

        r_sta
            .handle_mgmt_frame(
                &mut ctx,
                mac::CapabilityInfo(0),
                None,
                mac::MgmtHdr {
                    frame_ctrl: mac::FrameControl(0b00000000_00000000), // Assoc req frame
                    duration: 0,
                    addr1: [1; 6].into(),
                    addr2: [2; 6].into(),
                    addr3: [3; 6].into(),
                    seq_ctrl: mac::SequenceControl(10),
                },
                &[
                    0, 0, // Capability info
                    10, 0, // Listen interval
                ][..],
            )
            .expect("expected OK");
        assert_ne!(
            match r_sta.state.as_ref() {
                State::Associated { active_timeout_event_id, .. } => *active_timeout_event_id,
                _ => panic!("expected Associated"),
            },
            None
        )
    }

    #[test]
    fn handle_bss_idle_timeout() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        let event_id = r_sta.schedule_bss_idle_timeout(&mut ctx);
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: Some(event_id),
            ps_state: PowerSaveState::Awake,
        });

        r_sta.handle_bss_idle_timeout(&mut ctx, event_id).expect("expected OK");
        assert_variant!(r_sta.state.as_ref(), State::Authenticated);
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        #[rustfmt::skip]
        assert_eq!(&fake_device_state.lock().wlan_queue[0].0[..], &[
            // Mgmt header
            0b10100000, 0, // Frame Control
            0, 0, // Duration
            1, 1, 1, 1, 1, 1, // addr1
            2, 2, 2, 2, 2, 2, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // Disassoc header:
            4, 0, // reason code
        ][..]);
        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DisassociateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::DisassociateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::ReasonInactivity,
                locally_initiated: true,
            },
        );
    }

    #[test]
    fn doze_then_wake() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });

        r_sta.set_power_state(&mut ctx, mac::PowerState::DOZE).expect("expected doze OK");

        // Send a bunch of Ethernet frames.
        r_sta
            .handle_eth_frame(
                &mut ctx,
                *CLIENT_ADDR2,
                *CLIENT_ADDR,
                0x1234,
                &[1, 2, 3, 4, 5][..],
                0.into(),
            )
            .expect("expected OK");
        r_sta
            .handle_eth_frame(
                &mut ctx,
                *CLIENT_ADDR2,
                *CLIENT_ADDR,
                0x1234,
                &[6, 7, 8, 9, 0][..],
                0.into(),
            )
            .expect("expected OK");

        assert!(r_sta.has_buffered_frames());

        // Make sure nothing has been actually sent to the WLAN queue.
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 0);

        r_sta.set_power_state(&mut ctx, mac::PowerState::AWAKE).expect("expected wake OK");
        assert!(!r_sta.has_buffered_frames());
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 2);

        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b00001000, 0b00100010, // Frame Control
                0, 0, // Duration
                3, 3, 3, 3, 3, 3, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                1, 1, 1, 1, 1, 1, // addr3
                0x10, 0, // Sequence Control
                0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
                0, 0, 0, // OUI
                0x12, 0x34, // Protocol ID
                // Data
                1, 2, 3, 4, 5,
            ][..]
        );
        assert_eq!(
            &fake_device_state.lock().wlan_queue[1].0[..],
            &[
                // Mgmt header
                0b00001000, 0b00000010, // Frame Control
                0, 0, // Duration
                3, 3, 3, 3, 3, 3, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                1, 1, 1, 1, 1, 1, // addr3
                0x20, 0, // Sequence Control
                0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
                0, 0, 0, // OUI
                0x12, 0x34, // Protocol ID
                // Data
                6, 7, 8, 9, 0,
            ][..]
        );
    }

    #[test]
    fn doze_then_doze() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });

        r_sta.set_power_state(&mut ctx, mac::PowerState::DOZE).expect("expected doze OK");
        r_sta.set_power_state(&mut ctx, mac::PowerState::DOZE).expect("expected doze OK");
    }

    #[test]
    fn wake_then_wake() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Associated {
            aid: 1,
            eapol_controlled_port: None,
            active_timeout_event_id: None,
            ps_state: PowerSaveState::Awake,
        });

        r_sta.set_power_state(&mut ctx, mac::PowerState::AWAKE).expect("expected wake OK");
        r_sta.set_power_state(&mut ctx, mac::PowerState::AWAKE).expect("expected wake OK");
    }

    #[test]
    fn doze_not_associated() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticating);

        assert_variant!(
            r_sta
                .set_power_state(&mut ctx, mac::PowerState::DOZE)
                .expect_err("expected doze error"),
            ClientRejection::NotAssociated
        );
    }

    #[test]
    fn wake_not_associated() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, _) = FakeDevice::new(&exec);
        let (mut ctx, _) = make_context(fake_device);

        let mut r_sta = make_remote_client();
        r_sta.state = StateMachine::new(State::Authenticating);

        r_sta.set_power_state(&mut ctx, mac::PowerState::AWAKE).expect("expected wake OK");
    }
}
