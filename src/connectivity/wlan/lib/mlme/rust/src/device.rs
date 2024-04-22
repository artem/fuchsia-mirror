// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        buffer::FinalizedBuffer, common::mac::WlanGi, error::Error, DriverEvent, DriverEventSink,
    },
    anyhow::format_err,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fidl_fuchsia_wlan_softmac as fidl_softmac, fuchsia_trace as trace, fuchsia_zircon as zx,
    futures::{channel::mpsc, Future},
    ieee80211::MacAddr,
    std::{
        ffi::c_void,
        fmt::Display,
        marker::PhantomPinned,
        pin::Pin,
        sync::{atomic::AtomicPtr, Arc},
    },
    trace::Id as TraceId,
    tracing::error,
    wlan_common::{mac::FrameControl, tx_vector, TimeUnit},
    wlan_fidl_ext::{TryUnpack, WithName},
    wlan_trace as wtrace,
};

pub mod completers {
    use {fuchsia_zircon as zx, tracing::error};

    pub struct InitCompleter<F>
    where
        F: FnOnce(Result<(), zx::Status>) + Send,
    {
        completer: Option<F>,
    }

    impl<F> InitCompleter<F>
    where
        F: FnOnce(Result<(), zx::Status>) + Send,
    {
        pub fn new(completer: F) -> Self {
            Self { completer: Some(completer) }
        }

        pub fn complete(mut self, result: Result<(), zx::Status>) {
            let completer = match self.completer.take() {
                None => {
                    error!("Failed to call completer because it is None.");
                    return;
                }
                Some(completer) => completer,
            };
            completer(result)
        }
    }

    impl<F> Drop for InitCompleter<F>
    where
        F: FnOnce(Result<(), zx::Status>) + Send,
    {
        fn drop(&mut self) {
            if let Some(completer) = self.completer.take() {
                error!(
                    "About to drop start_sta() completer without calling it!\n\
                     Calling start_sta() completer from drop() to mitigate potential deadlock."
                );
                completer(Err(zx::Status::BAD_STATE))
            }
        }
    }

    pub struct StopCompleter {
        // TODO(42075638): Using dynamic dispatch since otherwise we would need to plumb generics
        // everywhere MLME uses a DriverEventSink. Since we will remove DriverEventSink soon, this
        // is not worthwhile.
        completer: Option<Box<dyn FnOnce() + Send>>,
    }

    impl StopCompleter {
        pub fn new(completer: Box<dyn FnOnce() + Send>) -> Self {
            Self { completer: Some(completer) }
        }

        pub fn complete(mut self) {
            let completer = match self.completer.take() {
                None => {
                    error!("Failed to call completer because it is None.");
                    return;
                }
                Some(completer) => completer,
            };
            completer()
        }
    }

    impl Drop for StopCompleter {
        fn drop(&mut self) {
            if let Some(completer) = self.completer.take() {
                error!(
                    "About to drop StopCompleter without calling it!\n\
                     Calling StopCompleter completer from drop() to mitigate potential deadlock."
                );
                completer()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use futures::channel::oneshot;

        #[test]
        fn init_completer_sends_ok() {
            let (sender, mut receiver) = oneshot::channel::<Result<(), zx::Status>>();
            let init_completer = InitCompleter::new(move |result: Result<(), zx::Status>| {
                sender.send(result).expect("Failed to send result.");
            });
            init_completer.complete(Ok(()));
            assert_eq!(Ok(Some(Ok(()))), receiver.try_recv());
        }

        #[test]
        fn init_completer_sends_err() {
            let (sender, mut receiver) = oneshot::channel::<Result<(), zx::Status>>();
            let init_completer = InitCompleter::new(move |result: Result<(), zx::Status>| {
                sender.send(result).expect("Failed to send result.");
            });
            init_completer.complete(Err(zx::Status::INTERNAL));
            assert_eq!(Ok(Some(Err(zx::Status::INTERNAL))), receiver.try_recv());
        }

        #[test]
        fn init_completer_sends_err_on_drop() {
            let (sender, mut receiver) = oneshot::channel::<Result<(), zx::Status>>();
            let init_completer = InitCompleter::new(move |result: Result<(), zx::Status>| {
                sender.send(result).expect("Failed to send result.");
            });
            drop(init_completer);
            assert_eq!(Ok(Some(Err(zx::Status::BAD_STATE))), receiver.try_recv());
        }

        #[test]
        fn stop_completer_sends_value() {
            let (sender, mut receiver) = oneshot::channel();
            let stop_completer = StopCompleter::new(Box::new(move || {
                sender.send(()).expect("Failed to send.");
            }));
            stop_completer.complete();
            assert_eq!(Ok(Some(())), receiver.try_recv());
        }

        #[test]
        fn stop_completer_sends_value_on_drop() {
            let (sender, mut receiver) = oneshot::channel();
            let stop_completer = StopCompleter::new(Box::new(move || {
                sender.send(()).expect("Failed to send.");
            }));
            drop(stop_completer);
            assert_eq!(Ok(Some(())), receiver.try_recv());
        }
    }
}

pub use test_utils::*;

#[derive(Debug, PartialEq)]
pub struct LinkStatus(u32);
impl LinkStatus {
    pub const DOWN: Self = Self(0);
    pub const UP: Self = Self(1);
}

impl From<fidl_mlme::ControlledPortState> for LinkStatus {
    fn from(state: fidl_mlme::ControlledPortState) -> Self {
        match state {
            fidl_mlme::ControlledPortState::Open => Self::UP,
            fidl_mlme::ControlledPortState::Closed => Self::DOWN,
        }
    }
}

pub struct Device {
    raw_device: DeviceInterface,
    frame_processor: Option<Pin<Box<FrameProcessor>>>,
    frame_sender: FrameSender,
    wlan_softmac_bridge_proxy: fidl_softmac::WlanSoftmacBridgeSynchronousProxy,
    minstrel: Option<crate::MinstrelWrapper>,
    event_receiver: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
    event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
}

impl Device {
    pub fn new(
        raw_device: DeviceInterface,
        wlan_softmac_bridge_proxy: fidl_softmac::WlanSoftmacBridgeSynchronousProxy,
        frame_sender: FrameSender,
    ) -> Device {
        let (event_sink, event_receiver) = mpsc::unbounded();
        Device {
            raw_device,
            frame_processor: None,
            frame_sender,
            wlan_softmac_bridge_proxy,
            minstrel: None,
            event_receiver: Some(event_receiver),
            event_sink,
        }
    }

    fn flatten_and_log_error<T>(
        method_name: impl Display,
        result: Result<Result<T, zx::zx_status_t>, fidl::Error>,
    ) -> Result<T, zx::Status> {
        result
            .map_err(|fidl_error| {
                error!("FIDL error during {}: {:?}", method_name, fidl_error);
                zx::Status::INTERNAL
            })?
            .map_err(|status| {
                error!("{} failed: {:?}", method_name, status);
                zx::Status::from_raw(status)
            })
    }
}

const REQUIRED_WLAN_HEADER_LEN: usize = 10;
const PEER_ADDR_OFFSET: usize = 4;

/// This trait abstracts how Device accomplish operations. Test code
/// can then implement trait methods instead of mocking an underlying DeviceInterface
/// and FIDL proxy.
pub trait DeviceOps {
    fn start(
        &mut self,
        driver_event_sink: DriverEventSink,
        wlan_softmac_ifc_bridge_client_handle: zx::sys::zx_handle_t,
    ) -> Result<zx::Handle, zx::Status>;
    fn wlan_softmac_query_response(
        &mut self,
    ) -> impl Future<Output = Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status>> + Send;
    fn discovery_support(
        &mut self,
    ) -> impl Future<Output = Result<fidl_common::DiscoverySupport, zx::Status>> + Send;
    fn mac_sublayer_support(
        &mut self,
    ) -> impl Future<Output = Result<fidl_common::MacSublayerSupport, zx::Status>> + Send;
    fn security_support(
        &mut self,
    ) -> impl Future<Output = Result<fidl_common::SecuritySupport, zx::Status>> + Send;
    fn spectrum_management_support(
        &mut self,
    ) -> impl Future<Output = Result<fidl_common::SpectrumManagementSupport, zx::Status>> + Send;
    fn deliver_eth_frame(&mut self, packet: &[u8]) -> Result<(), zx::Status>;
    /// Sends the given |buffer| as a frame over the air. If the caller does not pass an |async_id| to this
    /// function, then this function will generate its own |async_id| and end the trace if an error occurs.
    fn send_wlan_frame(
        &mut self,
        buffer: FinalizedBuffer,
        tx_flags: fidl_softmac::WlanTxInfoFlags,
        async_id: Option<TraceId>,
    ) -> Result<(), zx::Status>;

    fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status>;
    fn set_ethernet_up(&mut self) -> Result<(), zx::Status> {
        self.set_ethernet_status(LinkStatus::UP)
    }
    fn set_ethernet_down(&mut self) -> Result<(), zx::Status> {
        self.set_ethernet_status(LinkStatus::DOWN)
    }
    fn set_channel(
        &mut self,
        channel: fidl_common::WlanChannel,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> impl Future<
        Output = Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
    > + Send;
    fn start_active_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> impl Future<Output = Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status>>
           + Send;
    fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn join_bss(
        &mut self,
        request: &fidl_common::JoinBssRequest,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn enable_beaconing(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseEnableBeaconingRequest,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn disable_beaconing(&mut self) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn install_key(
        &mut self,
        key_configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn notify_association_complete(
        &mut self,
        assoc_cfg: fidl_softmac::WlanAssociationConfig,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
    fn update_wmm_parameters(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> impl Future<Output = Result<(), zx::Status>>;
    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>;
    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error>;
    fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper);
    fn minstrel(&mut self) -> Option<crate::MinstrelWrapper>;
    fn tx_vector_idx(
        &mut self,
        frame_control: &FrameControl,
        peer_addr: &MacAddr,
        flags: fidl_softmac::WlanTxInfoFlags,
    ) -> tx_vector::TxVecIdx {
        self.minstrel()
            .as_ref()
            .and_then(|minstrel| {
                minstrel.lock().get_tx_vector_idx(frame_control, &peer_addr, flags)
            })
            .unwrap_or_else(|| {
                // We either don't have minstrel, or minstrel failed to generate a tx vector.
                // Use a reasonable default value instead.
                // Note: This is only effective if the underlying device meets both criteria below:
                // 1. Does not support tx status report.
                // 2. Honors our instruction on tx_vector to use.
                // TODO(https://fxbug.dev/42103583): Choose an optimal MCS for management frames
                // TODO(https://fxbug.dev/42119762): Log stats about minstrel usage vs default tx vector.
                let mcs_idx = if frame_control.is_data() { 7 } else { 3 };
                tx_vector::TxVector::new(
                    fidl_common::WlanPhyType::Erp,
                    WlanGi::G_800NS,
                    fidl_common::ChannelBandwidth::Cbw20,
                    mcs_idx,
                )
                .unwrap()
                .to_idx()
            })
    }
}

pub async fn try_query(
    device: &mut impl DeviceOps,
) -> Result<fidl_softmac::WlanSoftmacQueryResponse, Error> {
    device
        .wlan_softmac_query_response()
        .await
        .map_err(|status| Error::Status(String::from("Failed to query device."), status))
}

pub async fn try_query_iface_mac(device: &mut impl DeviceOps) -> Result<MacAddr, Error> {
    try_query(device).await.and_then(|query_response| {
        query_response.sta_addr.map(From::from).ok_or_else(|| {
            Error::Internal(format_err!(
                "Required field not set in device query response: iface MAC"
            ))
        })
    })
}

pub async fn try_query_discovery_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::DiscoverySupport, Error> {
    device.discovery_support().await.map_err(|status| {
        Error::Status(String::from("Failed to query discovery support for device."), status)
    })
}

pub async fn try_query_mac_sublayer_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::MacSublayerSupport, Error> {
    device.mac_sublayer_support().await.map_err(|status| {
        Error::Status(String::from("Failed to query MAC sublayer support for device."), status)
    })
}

pub async fn try_query_security_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::SecuritySupport, Error> {
    device.security_support().await.map_err(|status| {
        Error::Status(String::from("Failed to query security support for device."), status)
    })
}

pub async fn try_query_spectrum_management_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::SpectrumManagementSupport, Error> {
    device.spectrum_management_support().await.map_err(|status| {
        Error::Status(
            String::from("Failed to query spectrum management support for device."),
            status,
        )
    })
}

impl DeviceOps for Device {
    fn start(
        &mut self,
        driver_event_sink: DriverEventSink,
        wlan_softmac_ifc_bridge_client_handle: zx::sys::zx_handle_t,
    ) -> Result<zx::Handle, zx::Status> {
        self.frame_processor = Some(FrameProcessor::new(driver_event_sink));
        let frame_processor = self.frame_processor.as_mut().unwrap().as_mut();
        // Safety: This call is safe because `self.frame_processor` will outlive all uses of the
        // constructed `CFrameProcessor` across the FFI boundary. This includes during unbind when
        // the C++ portion of wlansoftmac will ensure no additional calls will be made through
        // `CFrameProcessor` after unbind begins.
        let frame_processor = unsafe { frame_processor.to_c_binding() };

        let mut out_channel = 0;

        // Safety: This call to `self.raw_device.start` is safe because `frame_processor` was
        // constructed in accordance with the safety documentation of
        // `FrameProcessor::to_c_binding`.
        let status = unsafe {
            (self.raw_device.start)(
                *self.raw_device.device.get_mut(),
                wlan_softmac_ifc_bridge_client_handle,
                &frame_processor,
                &mut out_channel as *mut u32,
            )
        };
        // Safety: Constructing a `zx::Handle` from the returned `out_channel` is safe
        // because returning an invalid handle violates the FFI.
        //
        // An unsafe block is necessary because a `zx::Handle` cannot be passed across
        // the FFI boundary.
        zx::ok(status).map(|_| unsafe { zx::Handle::from_raw(out_channel) })
    }

    async fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
        Self::flatten_and_log_error(
            "Query",
            self.wlan_softmac_bridge_proxy.query(zx::Time::INFINITE),
        )
    }

    async fn discovery_support(&mut self) -> Result<fidl_common::DiscoverySupport, zx::Status> {
        Self::flatten_and_log_error(
            "QueryDiscoverySupport",
            self.wlan_softmac_bridge_proxy.query_discovery_support(zx::Time::INFINITE),
        )
    }

    async fn mac_sublayer_support(
        &mut self,
    ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
        Self::flatten_and_log_error(
            "QueryMacSublayerSupport",
            self.wlan_softmac_bridge_proxy.query_mac_sublayer_support(zx::Time::INFINITE),
        )
    }

    async fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
        Self::flatten_and_log_error(
            "QuerySecuritySupport",
            self.wlan_softmac_bridge_proxy.query_security_support(zx::Time::INFINITE),
        )
    }

    async fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
        Self::flatten_and_log_error(
            "QuerySpectrumManagementSupport",
            self.wlan_softmac_bridge_proxy.query_spectrum_management_support(zx::Time::INFINITE),
        )
    }

    fn deliver_eth_frame(&mut self, packet: &[u8]) -> Result<(), zx::Status> {
        wtrace::duration!(c"Device::deliver_eth_frame");
        self.frame_sender.ethernet_rx(&fidl_softmac::FrameSenderEthernetRxRequest {
            packet_address: Some(packet.as_ptr() as u64),
            packet_size: Some(packet.len() as u64),
            ..Default::default()
        })
    }

    fn send_wlan_frame(
        &mut self,
        buffer: FinalizedBuffer,
        mut tx_flags: fidl_softmac::WlanTxInfoFlags,
        async_id: Option<TraceId>,
    ) -> Result<(), zx::Status> {
        let async_id_provided = async_id.is_some();
        let async_id = async_id.unwrap_or_else(|| {
            let async_id = TraceId::new();
            wtrace::async_begin_wlansoftmac_tx(async_id, "mlme");
            async_id
        });
        wtrace::duration!(c"Device::send_data_frame");

        if buffer.len() < REQUIRED_WLAN_HEADER_LEN {
            let status = zx::Status::BUFFER_TOO_SMALL;
            if !async_id_provided {
                wtrace::async_end_wlansoftmac_tx(async_id, status);
            }
            return Err(status);
        }
        // Unwrap is safe since the byte slice is always the same size.
        let frame_control =
            zerocopy::Ref::<&[u8], FrameControl>::new(&buffer[0..=1]).unwrap().into_ref();
        if frame_control.protected() {
            tx_flags |= fidl_softmac::WlanTxInfoFlags::PROTECTED;
        }
        let peer_addr: MacAddr = {
            let mut peer_addr = [0u8; 6];
            peer_addr.copy_from_slice(&buffer[PEER_ADDR_OFFSET..PEER_ADDR_OFFSET + 6]);
            peer_addr.into()
        };
        let tx_vector_idx = self.tx_vector_idx(frame_control, &peer_addr, tx_flags);

        let tx_info = wlan_common::tx_vector::TxVector::from_idx(tx_vector_idx)
            .to_fidl_tx_info(tx_flags, self.minstrel.is_some());
        // Safety: This call to `FinalizedBuffer::release` is safe because the `packet_address` is
        // being sent to the C++ portion of wlansoftmac. If there is a FIDL error sending
        // `packet_address`, indicating it was not sent, then `free(packet_address)` will be called.
        let (packet_address, _free, packet_size) = unsafe { buffer.release() };
        self.frame_sender
            .wlan_tx(&fidl_softmac::FrameSenderWlanTxRequest {
                packet_address: Some(packet_address as u64),
                packet_size: Some(packet_size as u64),
                packet_info: Some(tx_info),
                async_id: Some(async_id.into()),
                ..Default::default()
            })
            .map_err(|s| {
                if !async_id_provided {
                    wtrace::async_end_wlansoftmac_tx(async_id, s);
                }
                s
            })
    }

    fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.set_ethernet_status)(*self.raw_device.device.get_mut(), status.0))
    }

    async fn set_channel(&mut self, channel: fidl_common::WlanChannel) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .set_channel(
                &fidl_softmac::WlanSoftmacBaseSetChannelRequest {
                    channel: Some(channel),
                    ..Default::default()
                },
                zx::Time::INFINITE,
            )
            .map_err(|error| {
                error!("SetChannel failed with FIDL error: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    async fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        Self::flatten_and_log_error(
            "StartPassiveScan",
            self.wlan_softmac_bridge_proxy.start_passive_scan(request, zx::Time::INFINITE),
        )
    }

    async fn start_active_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        Self::flatten_and_log_error(
            "StartActiveScan",
            self.wlan_softmac_bridge_proxy.start_active_scan(request, zx::Time::INFINITE),
        )
    }

    async fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
    ) -> Result<(), zx::Status> {
        Self::flatten_and_log_error(
            "CancelScan",
            self.wlan_softmac_bridge_proxy.cancel_scan(request, zx::Time::INFINITE),
        )
    }

    async fn join_bss(&mut self, request: &fidl_common::JoinBssRequest) -> Result<(), zx::Status> {
        Self::flatten_and_log_error(
            "JoinBss",
            self.wlan_softmac_bridge_proxy.join_bss(request, zx::Time::INFINITE),
        )
    }

    async fn enable_beaconing(
        &mut self,
        request: fidl_softmac::WlanSoftmacBaseEnableBeaconingRequest,
    ) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .enable_beaconing(&request, zx::Time::INFINITE)
            .map_err(|error| {
                error!("FIDL error during EnableBeaconing: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    async fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .disable_beaconing(zx::Time::INFINITE)
            .map_err(|error| {
                error!("DisableBeaconing failed with FIDL error: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    async fn install_key(
        &mut self,
        key_configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .install_key(&key_configuration, zx::Time::INFINITE)
            .map_err(|error| {
                error!("FIDL error during InstallKey: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    async fn notify_association_complete(
        &mut self,
        assoc_cfg: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().add_peer(&assoc_cfg)?;
        }
        Self::flatten_and_log_error(
            "NotifyAssociationComplete",
            self.wlan_softmac_bridge_proxy
                .notify_association_complete(&assoc_cfg, zx::Time::INFINITE),
        )
    }

    async fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        let addr: MacAddr = request
            .peer_addr
            .ok_or_else(|| {
                error!("ClearAssociation called with no peer_addr field.");
                zx::Status::INVALID_ARGS
            })?
            .into();
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().remove_peer(&addr);
        }
        Self::flatten_and_log_error(
            "ClearAssociation",
            self.wlan_softmac_bridge_proxy.clear_association(request, zx::Time::INFINITE),
        )
    }

    async fn update_wmm_parameters(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Self::flatten_and_log_error(
            "UpdateWmmParameters",
            self.wlan_softmac_bridge_proxy.update_wmm_parameters(request, zx::Time::INFINITE),
        )
    }

    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        self.event_receiver.take()
    }

    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        self.event_sink.unbounded_send(event).map_err(|e| e.into())
    }

    fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper) {
        self.minstrel.replace(minstrel);
    }

    fn minstrel(&mut self) -> Option<crate::MinstrelWrapper> {
        self.minstrel.as_ref().map(Arc::clone)
    }
}

#[repr(C)]
pub struct CFrameProcessorOps {
    wlan_rx: unsafe extern "C" fn(ctx: &DriverEventSink, request: *const u8, request_size: usize),
    ethernet_tx: unsafe extern "C" fn(
        ctx: &DriverEventSink,
        request: *const u8,
        request_size: usize,
    ) -> zx::zx_status_t,
}

/// Queues a WLAN MAC frame into a `DriverEventSink` for processing.
///
/// # Safety
///
/// Behavior is undefined unless `payload` points to a persisted
/// `fuchsia.wlan.softmac/FrameProcessor.WlanRx` request of length `payload_len` that is properly
/// aligned.
#[no_mangle]
unsafe extern "C" fn wlan_rx(ctx: &DriverEventSink, payload: *const u8, payload_len: usize) {
    wtrace::duration!(c"wlan_rx");

    // Safety: This call is safe because the caller promises `payload` points to a persisted
    // `fuchsia.wlan.softmac/FrameProcessor.WlanRx` request of length `payload_len` that is properly
    // aligned.
    let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) };
    let payload = match fidl::unpersist::<fidl_softmac::FrameProcessorWlanRxRequest>(payload) {
        Ok(payload) => payload,
        Err(e) => {
            error!("Unable to unpersist FrameProcessor.WlanRx request: {}", e);
            return;
        }
    };

    let async_id = match payload.async_id.with_name("async_id").try_unpack() {
        Ok(x) => x,
        Err(e) => {
            let e = e.context("Missing required field in FrameProcessorWlanRxRequest.");
            error!("{}", e);
            return;
        }
    };

    let (packet_address, packet_size, packet_info) = match (
        payload.packet_address.with_name("packet_address"),
        payload.packet_size.with_name("packet_size"),
        payload.packet_info.with_name("packet_info"),
    )
        .try_unpack()
    {
        Ok(x) => x,
        Err(e) => {
            let e = e.context("Missing required field(s) in FrameProcessorWlanRxRequest.");
            error!("{}", e);
            wtrace::async_end_wlansoftmac_rx(async_id.into(), &e.to_string());
            return;
        }
    };

    let packet_ptr = packet_address as *const u8;
    if packet_ptr.is_null() {
        let e = format_err!("FrameProcessor.WlanRx request contained NULL packet_address");
        error!("{:?}", e);
        wtrace::async_end_wlansoftmac_rx(async_id.into(), &e.to_string());
        return;
    }

    // Safety: This call is safe because a `WlanRx` request is defined such that a slice
    // such as this one can be constructed from the `packet_address` and `packet_size` fields.
    let packet_bytes: Vec<u8> =
        unsafe { std::slice::from_raw_parts(packet_ptr, packet_size as usize) }.into();

    let _: Result<(), ()> = ctx
        .unbounded_send(DriverEvent::MacFrameRx {
            bytes: packet_bytes,
            rx_info: packet_info,
            async_id: async_id.into(),
        })
        .map_err(|e| {
            let e = format_err!("Failed to queue FrameProcessor.WlanRx request: {:?}", e);
            error!("{:?}", e);
            wtrace::async_end_wlansoftmac_rx(async_id.into(), &e.to_string());
        });
}

/// Queues an Ethernet frame into a `DriverEventSink` for processing.
///
/// The caller should either end the async
/// trace event corresponding to |async_id| if an error occurs or deferred ending the trace to a later call
/// into the C++ portion of wlansoftmac.
///
/// Assuming no errors occur, the Rust portion of wlansoftmac will eventually
/// rust_device_interface_t.queue_tx() with the same |async_id|. At that point, the C++ portion of
/// wlansoftmac will assume responsibility for ending the async trace event.
///
/// # Safety
///
/// Behavior is undefined unless `payload` points to a persisted
/// `fuchsia.wlan.softmac/FrameProcessor.EthernetTx` request of length `payload_len` that is properly
/// aligned.
#[no_mangle]
unsafe extern "C" fn ethernet_tx(
    ctx: &DriverEventSink,
    payload: *const u8,
    payload_len: usize,
) -> zx::zx_status_t {
    wtrace::duration!(c"ethernet_rx");

    // Safety: This call is safe because the caller promises `payload` points to a persisted
    // `fuchsia.wlan.softmac/FrameProcessor.EthernetTx` request of length `payload_len` that is properly
    // aligned.
    let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) };
    let payload = match fidl::unpersist::<fidl_softmac::FrameProcessorEthernetTxRequest>(payload) {
        Ok(payload) => payload,
        Err(e) => {
            error!("Unable to unpersist FrameProcessor.EthernetTx request: {}", e);
            return zx::Status::INTERNAL.into_raw();
        }
    };

    let async_id = match payload.async_id.with_name("async_id").try_unpack() {
        Ok(x) => x,
        Err(e) => {
            let e = e.context("Missing required field in FrameProcessorEthernetTxRequest.");
            error!("{}", e);
            return zx::Status::INVALID_ARGS.into_raw();
        }
    };

    let (packet_address, packet_size) = match (
        payload.packet_address.with_name("packet_address"),
        payload.packet_size.with_name("packet_size"),
    )
        .try_unpack()
    {
        Ok(x) => x,
        Err(e) => {
            let e = e.context("Missing required field(s) in FrameProcessorEthernetTxRequest.");
            error!("{}", e);
            return zx::Status::INVALID_ARGS.into_raw();
        }
    };

    let packet_ptr = packet_address as *const u8;
    if packet_ptr.is_null() {
        let e = format_err!("FrameProcessor.EthernetTx request contained NULL packet_address");
        error!("{:?}", e);
        return zx::Status::INVALID_ARGS.into_raw();
    }

    // Safety: This call is safe because a `EthernetTx` request is defined such that a slice
    // such as this one can be constructed from the `packet_address` and `packet_size` fields.
    let packet_bytes: Vec<u8> =
        unsafe { std::slice::from_raw_parts(packet_ptr, packet_size as usize) }.into();

    match ctx
        .unbounded_send(DriverEvent::EthFrameTx { bytes: packet_bytes, async_id: async_id.into() })
    {
        Err(e) => {
            let e = format_err!("Failed to queue FrameProcessor.EthernetTx request: {:?}", e);
            error!("{:?}", e);
            zx::Status::INTERNAL.into_raw()
        }
        Ok(()) => zx::Status::OK.into_raw(),
    }
}

const FRAME_PROCESSOR_OPS: CFrameProcessorOps = CFrameProcessorOps { wlan_rx, ethernet_tx };

struct FrameProcessor {
    sink: DriverEventSink,
    _pin: PhantomPinned,
}

impl FrameProcessor {
    /// Return a pinned `FrameProcessor`.
    ///
    /// Pinning the returned value is imperative to ensure future `to_c_binding()` calls will return
    /// pointers that are valid for the lifetime of the returned value.
    fn new(sink: DriverEventSink) -> Pin<Box<Self>> {
        Box::pin(Self { sink, _pin: PhantomPinned })
    }

    /// Returns a `CFrameProcessor` containing pointers to the static `FRAME_PROCESSOR_OPS`
    /// and `self.sink`.
    ///
    /// Note that those pointers are to a static value and a pinned value, i.e., the former pointer
    /// is always valid, and the latter pointer is valid as long as the pinned value was not
    /// dropped.
    ///
    /// # Safety
    ///
    /// This method unsafe because we cannot guarantee the `DriverEventSink` pointed to by `ctx`
    /// will have a lifetime that will exceed its use across an FFI boundary.
    ///
    /// By using this method, the caller promises the lifetime of `DriverEventSink` will exceed the
    /// `ctx` pointer used across the FFI boundary.
    unsafe fn to_c_binding(self: Pin<&mut Self>) -> CFrameProcessor {
        CFrameProcessor { ops: &FRAME_PROCESSOR_OPS, ctx: &self.sink }
    }
}

/// Type containing pointers to the static `FRAME_PROCESSOR_OPS` and a `DriverEventSink`.
///
/// The wlansoftmac driver copies the pointers from `CFrameProcessor` which means the code
/// constructing this type must ensure those pointers remain valid for their lifetime in
/// wlansoftmac.
#[repr(C)]
pub struct CFrameProcessor {
    ops: *const CFrameProcessorOps,
    ctx: *const DriverEventSink,
}

#[repr(C)]
pub struct CFrameSender {
    ctx: *mut c_void,
    /// Sends a WLAN MAC frame to the C++ portion of wlansoftmac.
    ///
    /// # Safety
    ///
    /// Behavior is undefined unless `payload` contains a persisted `FrameSender.WlanTx` request
    /// and `payload_len` is the length of the persisted byte array.
    wlan_tx: unsafe extern "C" fn(
        ctx: *mut c_void,
        payload: *const u8,
        payload_len: usize,
    ) -> zx::zx_status_t,
    /// Sends an Ethernet frame to the C++ portion of wlansoftmac.
    ///
    /// # Safety
    ///
    /// Behavior is undefined unless `payload` contains a persisted `FrameSender.EthernetRx` request
    /// and `payload_len` is the length of the persisted byte array.
    ethernet_rx: unsafe extern "C" fn(
        ctx: *mut c_void,
        payload: *const u8,
        payload_len: usize,
    ) -> zx::zx_status_t,
}

pub struct FrameSender {
    ctx: AtomicPtr<c_void>,
    /// Sends a WLAN MAC frame to the C++ portion of wlansoftmac.
    ///
    /// # Safety
    ///
    /// Behavior is undefined unless `payload` contains a persisted `FrameSender.WlanTx` request
    /// and `payload_len` is the length of the persisted byte array.
    wlan_tx: unsafe extern "C" fn(
        ctx: *mut c_void,
        payload: *const u8,
        payload_len: usize,
    ) -> zx::zx_status_t,
    /// Sends an Ethernet frame to the C++ portion of wlansoftmac.
    ///
    /// # Safety
    ///
    /// Behavior is undefined unless `payload` contains a persisted `FrameSender.EthernetRx` request
    /// and `payload_len` is the length of the persisted byte array.
    ethernet_rx: unsafe extern "C" fn(
        ctx: *mut c_void,
        payload: *const u8,
        payload_len: usize,
    ) -> zx::zx_status_t,
}

impl From<CFrameSender> for FrameSender {
    fn from(frame_sender: CFrameSender) -> Self {
        Self {
            ctx: AtomicPtr::new(frame_sender.ctx),
            wlan_tx: frame_sender.wlan_tx,
            ethernet_rx: frame_sender.ethernet_rx,
        }
    }
}

impl FrameSender {
    fn wlan_tx(
        &mut self,
        request: &fidl_softmac::FrameSenderWlanTxRequest,
    ) -> Result<(), zx::Status> {
        let payload = fidl::persist(request);
        match payload {
            Err(e) => {
                error!("Failed to persist FrameSender.WlanTxRequest: {}", e);
                Err(zx::Status::INTERNAL)
            }
            Ok(payload) => {
                // Safety: The `self.wlan_tx` call is safe because the payload is a persisted
                // `FrameSender.EthernetRx` request.
                zx::Status::from_raw(unsafe {
                    (self.wlan_tx)(*self.ctx.get_mut(), payload.as_slice().as_ptr(), payload.len())
                })
                .into()
            }
        }
    }

    fn ethernet_rx(
        &mut self,
        request: &fidl_softmac::FrameSenderEthernetRxRequest,
    ) -> Result<(), zx::Status> {
        let payload = fidl::persist(request);
        match payload {
            Err(e) => {
                error!("Failed to persist FrameSender.EthernetRxRequest: {}", e);
                Err(zx::Status::INTERNAL)
            }
            Ok(payload) => {
                let payload = payload.as_slice();
                // Safety: The `self.ethernet_rx` call is safe because the payload is a persisted
                // `FrameSender.EthernetRx` request.
                zx::Status::from_raw(unsafe {
                    (self.ethernet_rx)(*self.ctx.get_mut(), payload.as_ptr(), payload.len())
                })
                .into()
            }
        }
    }
}

/// Type that represents the FFI from the bridged wlansoftmac to wlansoftmac itself.
///
/// Each of the functions in this FFI are safe to call from any thread but not
/// safe to call concurrently, i.e., they can only be called one at a time.
///
/// # Safety
///
/// Rust does not support marking a type as unsafe, but initializing this type is
/// definitely unsafe and deserves documentation. This is because when the bridged
/// wlansoftmac uses this FFI, it cannot guarantee each of the functions is safe to
/// call from any thread.
///
/// By constructing a value of this type, the constructor promises each of the functions
/// is safe to call from any thread. And no undefined behavior will occur if the
/// caller only calls one of them at a time.
#[repr(C)]
pub struct CDeviceInterface {
    device: *mut c_void,
    /// Start operations on the underlying device and return the SME channel.
    start: extern "C" fn(
        device: *mut c_void,
        wlan_softmac_ifc_bridge_client_handle: zx::sys::zx_handle_t,
        frame_processor: *const CFrameProcessor,
        out_sme_channel: *mut zx::sys::zx_handle_t,
    ) -> i32,
    /// Reports the current status to the ethernet driver.
    set_ethernet_status: extern "C" fn(device: *mut c_void, status: u32) -> i32,
}

/// Type that represents the FFI from the bridged wlansoftmac to wlansoftmac itself.
///
/// Each of the functions in this FFI are safe to call from another thread but not
/// safe to call concurrently, i.e., they can only be called one at a time.
pub struct DeviceInterface {
    device: AtomicPtr<c_void>,
    /// Start operations on the underlying device and return the SME channel.
    ///
    /// # Lifetime
    ///
    /// The lifetime of `ifc` ends when the corresponding `Device` is destroyed.
    ///
    /// # Safety
    ///
    /// This function is unsafe because this function cannot guarantee `ifc` will remain valid
    /// for the lifetime of the bridged wlansoftmac driver.
    ///
    /// By calling this function, the caller promises `ifc` will remain valid for the lifetime
    /// of the bridged wlansoftmac driver. Otherwise, `ifc` might cause a `DriverEvent` to be
    /// written to a dropped `DriverEventSink`.
    start: unsafe extern "C" fn(
        device: *mut c_void,
        wlan_softmac_ifc_bridge_client_handle: zx::sys::zx_handle_t,
        frame_processor: *const CFrameProcessor,
        out_sme_channel: *mut zx::sys::zx_handle_t,
    ) -> i32,
    /// Reports the current status to the ethernet driver.
    set_ethernet_status: extern "C" fn(device: *mut c_void, status: u32) -> i32,
}

impl From<CDeviceInterface> for DeviceInterface {
    fn from(device_interface: CDeviceInterface) -> Self {
        Self {
            // Safety: This is safe because `device_interface.device` will never become
            // any other type than `*mut c_void`.
            device: AtomicPtr::new(device_interface.device),
            start: device_interface.start,
            set_ethernet_status: device_interface.set_ethernet_status,
        }
    }
}

pub mod test_utils {
    use {
        super::*,
        crate::{
            buffer::{CBufferProvider, FakeCBufferProvider},
            ddk_converter,
        },
        fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
        fidl_fuchsia_wlan_internal as fidl_internal, fidl_fuchsia_wlan_sme as fidl_sme,
        fuchsia_sync::Mutex,
        fuchsia_zircon::HandleBased,
        paste::paste,
        std::collections::VecDeque,
    };

    pub trait FromMlmeEvent {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self>
        where
            Self: std::marker::Sized;
    }

    impl FromMlmeEvent for fidl_mlme::AuthenticateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_authenticate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::AssociateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_associate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::ConnectConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_connect_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::StartConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_start_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::StopConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_stop_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::ScanResult {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_on_scan_result()
        }
    }

    impl FromMlmeEvent for fidl_mlme::ScanEnd {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_on_scan_end()
        }
    }

    impl FromMlmeEvent for fidl_mlme::EapolConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_eapol_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::EapolIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_eapol_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::DeauthenticateConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_deauthenticate_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::DeauthenticateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_deauthenticate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::DisassociateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_disassociate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::SetKeysConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_set_keys_conf()
        }
    }

    impl FromMlmeEvent for fidl_internal::SignalReportIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_signal_report()
        }
    }

    pub struct FakeDeviceConfig {
        mock_start_result: Option<Result<zx::Handle, zx::Status>>,
        mock_query_response: Option<Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status>>,
        mock_discovery_support: Option<Result<fidl_common::DiscoverySupport, zx::Status>>,
        mock_mac_sublayer_support: Option<Result<fidl_common::MacSublayerSupport, zx::Status>>,
        mock_security_support: Option<Result<fidl_common::SecuritySupport, zx::Status>>,
        mock_spectrum_management_support:
            Option<Result<fidl_common::SpectrumManagementSupport, zx::Status>>,
        pub start_passive_scan_fails: bool,
        pub start_active_scan_fails: bool,
        pub send_wlan_frame_fails: bool,
    }

    impl Default for FakeDeviceConfig {
        fn default() -> Self {
            Self {
                mock_start_result: None,
                mock_query_response: None,
                mock_discovery_support: None,
                mock_mac_sublayer_support: None,
                mock_security_support: None,
                mock_spectrum_management_support: None,
                start_passive_scan_fails: false,
                start_active_scan_fails: false,
                send_wlan_frame_fails: false,
            }
        }
    }

    /// Generates a public [<with_mock_ $mock_name>]() function to specify a mock value for corresponding
    /// DeviceOps method. When called, the generated function will overwrite whatever mocked value already
    /// exists, if any, including mocked fields.
    macro_rules! with_mock_func {
        ( $mock_name: ident, $mock_type: path ) => {
            paste! {
                pub fn [<with_mock_ $mock_name>](
                    mut self,
                    mock_value: Result<$mock_type, zx::Status>
                ) -> Self {
                    self.[<mock_ $mock_name>] = Some(mock_value);
                    self
                }
            }
        };
    }

    impl FakeDeviceConfig {
        with_mock_func!(start_result, zx::Handle);
        with_mock_func!(query_response, fidl_softmac::WlanSoftmacQueryResponse);
        with_mock_func!(discovery_support, fidl_common::DiscoverySupport);
        with_mock_func!(mac_sublayer_support, fidl_common::MacSublayerSupport);
        with_mock_func!(security_support, fidl_common::SecuritySupport);
        with_mock_func!(spectrum_management_support, fidl_common::SpectrumManagementSupport);

        pub fn with_mock_sta_addr(mut self, mock_field: [u8; 6]) -> Self {
            if let None = self.mock_query_response {
                let mut mock_value = Self::default_mock_query_response();
                mock_value.as_mut().unwrap().sta_addr = Some(mock_field);
                return self.with_mock_query_response(mock_value);
            }
            let mock_value = self
                .mock_query_response
                .as_mut()
                .unwrap()
                .as_mut()
                .expect("Cannot overwrite an Err value mock");
            mock_value.sta_addr = Some(mock_field);
            self
        }

        pub fn with_mock_mac_role(mut self, mock_field: fidl_common::WlanMacRole) -> Self {
            if let None = self.mock_query_response {
                let mut mock_value = Self::default_mock_query_response();
                mock_value.as_mut().unwrap().mac_role = Some(mock_field);
                return self.with_mock_query_response(mock_value);
            }
            let mock_value = self
                .mock_query_response
                .as_mut()
                .unwrap()
                .as_mut()
                .expect("Cannot overwrite an Err value mock");
            mock_value.mac_role = Some(mock_field);
            self
        }

        fn default_mock_query_response(
        ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
            Ok(fidl_softmac::WlanSoftmacQueryResponse {
                sta_addr: Some([7u8; 6]),
                mac_role: Some(fidl_common::WlanMacRole::Client),
                supported_phys: Some(vec![
                    fidl_common::WlanPhyType::Dsss,
                    fidl_common::WlanPhyType::Hr,
                    fidl_common::WlanPhyType::Ofdm,
                    fidl_common::WlanPhyType::Erp,
                    fidl_common::WlanPhyType::Ht,
                    fidl_common::WlanPhyType::Vht,
                ]),
                hardware_capability: Some(0),
                band_caps: Some(fake_band_caps()),
                ..Default::default()
            })
        }

        pub fn with_mock_probe_response_offload(
            mut self,
            mock_field: fidl_common::ProbeResponseOffloadExtension,
        ) -> Self {
            if let None = self.mock_discovery_support {
                let mut mock_value = Self::default_mock_discovery_support();
                mock_value.as_mut().unwrap().probe_response_offload = mock_field;
                return self.with_mock_discovery_support(mock_value);
            }
            let mock_value = self
                .mock_discovery_support
                .as_mut()
                .unwrap()
                .as_mut()
                .expect("Cannot overwrite an Err value mock");
            mock_value.probe_response_offload = mock_field;
            self
        }

        fn default_mock_discovery_support() -> Result<fidl_common::DiscoverySupport, zx::Status> {
            Ok(fidl_common::DiscoverySupport {
                scan_offload: fidl_common::ScanOffloadExtension {
                    supported: true,
                    scan_cancel_supported: false,
                },
                probe_response_offload: fidl_common::ProbeResponseOffloadExtension {
                    supported: false,
                },
            })
        }

        pub fn with_mock_mac_implementation_type(
            mut self,
            mock_field: fidl_common::MacImplementationType,
        ) -> Self {
            if let None = self.mock_mac_sublayer_support {
                let mut mock_value = Self::default_mock_mac_sublayer_support();
                mock_value.as_mut().unwrap().device.mac_implementation_type = mock_field;
                return self.with_mock_mac_sublayer_support(mock_value);
            }
            let mock_value = self
                .mock_mac_sublayer_support
                .as_mut()
                .unwrap()
                .as_mut()
                .expect("Cannot overwrite an Err value mock");
            mock_value.device.mac_implementation_type = mock_field;
            self
        }

        fn default_mock_mac_sublayer_support() -> Result<fidl_common::MacSublayerSupport, zx::Status>
        {
            Ok(fidl_common::MacSublayerSupport {
                rate_selection_offload: fidl_common::RateSelectionOffloadExtension {
                    supported: false,
                },
                data_plane: fidl_common::DataPlaneExtension {
                    data_plane_type: fidl_common::DataPlaneType::EthernetDevice,
                },
                device: fidl_common::DeviceExtension {
                    is_synthetic: true,
                    mac_implementation_type: fidl_common::MacImplementationType::Softmac,
                    tx_status_report_supported: true,
                },
            })
        }
    }

    /// Wrapper struct that can share mutable access to the internal
    /// FakeDeviceState.
    #[derive(Clone)]
    pub struct FakeDevice {
        state: Arc<Mutex<FakeDeviceState>>,
        mlme_event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
    }

    pub struct FakeDeviceState {
        pub config: FakeDeviceConfig,
        pub minstrel: Option<crate::MinstrelWrapper>,
        pub eth_queue: Vec<Vec<u8>>,
        pub wlan_queue: Vec<(Vec<u8>, u32)>,
        pub wlan_softmac_ifc_bridge_proxy: Option<fidl_softmac::WlanSoftmacIfcBridgeProxy>,
        pub mlme_event_stream: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
        pub mlme_request_sink: mpsc::UnboundedSender<wlan_sme::MlmeRequest>,
        pub mlme_request_stream: Option<mpsc::UnboundedReceiver<wlan_sme::MlmeRequest>>,
        pub usme_bootstrap_client_end:
            Option<fidl::endpoints::ClientEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub usme_bootstrap_server_end:
            Option<fidl::endpoints::ServerEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub wlan_channel: fidl_common::WlanChannel,
        pub keys: Vec<fidl_softmac::WlanKeyConfiguration>,
        pub next_scan_id: u64,
        pub captured_passive_scan_request:
            Option<fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest>,
        pub captured_active_scan_request: Option<fidl_softmac::WlanSoftmacStartActiveScanRequest>,

        pub join_bss_request: Option<fidl_common::JoinBssRequest>,
        pub beacon_config: Option<(Vec<u8>, usize, TimeUnit)>,
        pub link_status: LinkStatus,
        pub assocs: std::collections::HashMap<MacAddr, fidl_softmac::WlanAssociationConfig>,
        pub buffer_provider: CBufferProvider,
        pub install_key_results: VecDeque<Result<(), zx::Status>>,
        pub captured_update_wmm_parameters_request:
            Option<fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest>,
    }

    impl FakeDevice {
        // TODO(https://fxbug.dev/327499461): This function is async to ensure MLME functions will
        // run in an async context and not call `wlan_common::timer::Timer::now` without an
        // executor.
        pub async fn new() -> (FakeDevice, Arc<Mutex<FakeDeviceState>>) {
            Self::new_with_config(FakeDeviceConfig::default()).await
        }

        // TODO(https://fxbug.dev/327499461): This function is async to ensure MLME functions will
        // run in an async context and not call `wlan_common::timer::Timer::now` without an
        // executor.
        pub async fn new_with_config(
            config: FakeDeviceConfig,
        ) -> (FakeDevice, Arc<Mutex<FakeDeviceState>>) {
            // Create a channel for SME requests, to be surfaced by start().
            let (usme_bootstrap_client_end, usme_bootstrap_server_end) =
                fidl::endpoints::create_endpoints::<fidl_sme::UsmeBootstrapMarker>();
            let (mlme_event_sink, mlme_event_stream) = mpsc::unbounded();
            let (mlme_request_sink, mlme_request_stream) = mpsc::unbounded();
            let state = Arc::new(Mutex::new(FakeDeviceState {
                config,
                minstrel: None,
                eth_queue: vec![],
                wlan_queue: vec![],
                wlan_softmac_ifc_bridge_proxy: None,
                mlme_event_stream: Some(mlme_event_stream),
                mlme_request_sink,
                mlme_request_stream: Some(mlme_request_stream),
                usme_bootstrap_client_end: Some(usme_bootstrap_client_end),
                usme_bootstrap_server_end: Some(usme_bootstrap_server_end),
                wlan_channel: fidl_common::WlanChannel {
                    primary: 0,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                },
                next_scan_id: 0,
                captured_passive_scan_request: None,
                captured_active_scan_request: None,
                keys: vec![],
                join_bss_request: None,
                beacon_config: None,
                link_status: LinkStatus::DOWN,
                assocs: std::collections::HashMap::new(),
                buffer_provider: FakeCBufferProvider::new(),
                install_key_results: VecDeque::new(),
                captured_update_wmm_parameters_request: None,
            }));
            (FakeDevice { state: state.clone(), mlme_event_sink }, state)
        }

        pub fn state(&self) -> Arc<Mutex<FakeDeviceState>> {
            self.state.clone()
        }
    }

    impl FakeDeviceState {
        #[track_caller]
        pub fn next_mlme_msg<T: FromMlmeEvent>(&mut self) -> Result<T, Error> {
            self.mlme_event_stream
                .as_mut()
                .expect("no mlme event stream available")
                .try_next()
                .map_err(|e| anyhow::format_err!("Failed to read mlme event stream: {}", e))
                .and_then(|opt_next| opt_next.ok_or(anyhow::format_err!("No message available")))
                .and_then(|evt| {
                    T::from_event(evt).ok_or(anyhow::format_err!("Unexpected mlme event"))
                })
                .map_err(|e| e.into())
        }

        pub fn reset(&mut self) {
            self.eth_queue.clear();
        }
    }

    impl DeviceOps for FakeDevice {
        fn start(
            &mut self,
            _driver_event_sink: DriverEventSink,
            wlan_softmac_ifc_bridge_client_handle: zx::sys::zx_handle_t,
        ) -> Result<zx::Handle, zx::Status> {
            let mut state = self.state.lock();

            if let Some(mock_start_result) = state.config.mock_start_result.take() {
                return mock_start_result;
            }

            state.wlan_softmac_ifc_bridge_proxy =
                Some(fidl_softmac::WlanSoftmacIfcBridgeProxy::new(
                    fidl::AsyncChannel::from_channel(fidl::Channel::from(unsafe {
                        fidl::Handle::from_raw(wlan_softmac_ifc_bridge_client_handle)
                    })),
                ));
            let usme_bootstrap_server_end_handle =
                state.usme_bootstrap_server_end.take().unwrap().into_channel().into_handle();
            // TODO(https://fxbug.dev/42121991): Capture _ifc and provide a testing surface.
            Ok(usme_bootstrap_server_end_handle)
        }

        async fn wlan_softmac_query_response(
            &mut self,
        ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
            let state = self.state.lock();
            match state.config.mock_query_response.as_ref() {
                Some(query_response) => query_response.clone(),
                None => FakeDeviceConfig::default_mock_query_response(),
            }
        }

        async fn discovery_support(&mut self) -> Result<fidl_common::DiscoverySupport, zx::Status> {
            let state = self.state.lock();
            match state.config.mock_discovery_support.as_ref() {
                Some(discovery_support) => discovery_support.clone(),
                None => FakeDeviceConfig::default_mock_discovery_support(),
            }
        }

        async fn mac_sublayer_support(
            &mut self,
        ) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
            let state = self.state.lock();
            match state.config.mock_mac_sublayer_support.as_ref() {
                Some(mac_sublayer_support) => mac_sublayer_support.clone(),
                None => FakeDeviceConfig::default_mock_mac_sublayer_support(),
            }
        }

        async fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
            let state = self.state.lock();
            match state.config.mock_security_support.as_ref() {
                Some(security_support) => security_support.clone(),
                None => Ok(fidl_common::SecuritySupport {
                    mfp: fidl_common::MfpFeature { supported: false },
                    sae: fidl_common::SaeFeature {
                        driver_handler_supported: false,
                        sme_handler_supported: false,
                    },
                }),
            }
        }

        async fn spectrum_management_support(
            &mut self,
        ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
            let state = self.state.lock();
            match state.config.mock_spectrum_management_support.as_ref() {
                Some(spectrum_management_support) => spectrum_management_support.clone(),
                None => Ok(fidl_common::SpectrumManagementSupport {
                    dfs: fidl_common::DfsFeature { supported: true },
                }),
            }
        }

        fn deliver_eth_frame(&mut self, packet: &[u8]) -> Result<(), zx::Status> {
            self.state.lock().eth_queue.push(packet.to_vec());
            Ok(())
        }

        fn send_wlan_frame(
            &mut self,
            buffer: FinalizedBuffer,
            _tx_flags: fidl_softmac::WlanTxInfoFlags,
            _async_id: Option<TraceId>,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock();
            if state.config.send_wlan_frame_fails {
                return Err(zx::Status::IO);
            }
            state.wlan_queue.push((buffer.to_vec(), 0));
            Ok(())
        }

        fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
            self.state.lock().link_status = status;
            Ok(())
        }

        async fn set_channel(
            &mut self,
            wlan_channel: fidl_common::WlanChannel,
        ) -> Result<(), zx::Status> {
            self.state.lock().wlan_channel = wlan_channel;
            Ok(())
        }

        async fn start_passive_scan(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest,
        ) -> Result<fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
            let mut state = self.state.lock();
            if state.config.start_passive_scan_fails {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let scan_id = state.next_scan_id;
            state.next_scan_id += 1;
            state.captured_passive_scan_request.replace(request.clone());
            Ok(fidl_softmac::WlanSoftmacBaseStartPassiveScanResponse {
                scan_id: Some(scan_id),
                ..Default::default()
            })
        }

        async fn start_active_scan(
            &mut self,
            request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
        ) -> Result<fidl_softmac::WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
            let mut state = self.state.lock();
            if state.config.start_active_scan_fails {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let scan_id = state.next_scan_id;
            state.next_scan_id += 1;
            state.captured_active_scan_request.replace(request.clone());
            Ok(fidl_softmac::WlanSoftmacBaseStartActiveScanResponse {
                scan_id: Some(scan_id),
                ..Default::default()
            })
        }

        async fn cancel_scan(
            &mut self,
            _request: &fidl_softmac::WlanSoftmacBaseCancelScanRequest,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }

        async fn join_bss(
            &mut self,
            request: &fidl_common::JoinBssRequest,
        ) -> Result<(), zx::Status> {
            self.state.lock().join_bss_request.replace(request.clone());
            Ok(())
        }

        async fn enable_beaconing(
            &mut self,
            request: fidl_softmac::WlanSoftmacBaseEnableBeaconingRequest,
        ) -> Result<(), zx::Status> {
            match (request.packet_template, request.tim_ele_offset, request.beacon_interval) {
                (Some(packet_template), Some(tim_ele_offset), Some(beacon_interval)) => Ok({
                    self.state.lock().beacon_config = Some((
                        packet_template.mac_frame.clone(),
                        usize::try_from(tim_ele_offset).map_err(|_| zx::Status::INTERNAL)?,
                        TimeUnit(beacon_interval),
                    ));
                }),
                _ => Err(zx::Status::INVALID_ARGS),
            }
        }

        async fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
            self.state.lock().beacon_config = None;
            Ok(())
        }

        async fn install_key(
            &mut self,
            key_configuration: &fidl_softmac::WlanKeyConfiguration,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock();
            state.keys.push(key_configuration.clone());
            state.install_key_results.pop_front().unwrap_or(Ok(()))
        }

        async fn notify_association_complete(
            &mut self,
            cfg: fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock();
            if let Some(minstrel) = &state.minstrel {
                minstrel.lock().add_peer(&cfg)?
            }
            state.assocs.insert(cfg.bssid.unwrap().into(), cfg);
            Ok(())
        }

        async fn clear_association(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBaseClearAssociationRequest,
        ) -> Result<(), zx::Status> {
            let addr: MacAddr = request.peer_addr.unwrap().into();
            let mut state = self.state.lock();
            if let Some(minstrel) = &state.minstrel {
                minstrel.lock().remove_peer(&addr);
            }
            state.assocs.remove(&addr);
            state.join_bss_request = None;
            Ok(())
        }

        async fn update_wmm_parameters(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock();
            state.captured_update_wmm_parameters_request.replace(request.clone());
            Ok(())
        }

        fn take_mlme_event_stream(
            &mut self,
        ) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
            self.state.lock().mlme_event_stream.take()
        }

        fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
            self.mlme_event_sink.unbounded_send(event).map_err(|e| e.into())
        }

        fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper) {
            self.state.lock().minstrel.replace(minstrel);
        }

        fn minstrel(&mut self) -> Option<crate::MinstrelWrapper> {
            self.state.lock().minstrel.as_ref().map(Arc::clone)
        }
    }

    pub fn fake_band_caps() -> Vec<fidl_softmac::WlanSoftmacBandCapability> {
        vec![
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::TwoGhz),
                basic_rates: Some(vec![
                    0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c,
                ]),
                operating_channels: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                }),
                vht_supported: Some(false),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities { bytes: Default::default() }),
                ..Default::default()
            },
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x02, 0x04, 0x0b, 0x16, 0x30, 0x60, 0x7e, 0x7f]),
                operating_channels: Some(vec![36, 40, 44, 48, 149, 153, 157, 161]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                }),
                vht_supported: Some(true),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities {
                    bytes: [0x32, 0x50, 0x80, 0x0f, 0xfe, 0xff, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00],
                }),
                ..Default::default()
            },
        ]
    }

    pub fn fake_mlme_band_caps() -> Vec<fidl_mlme::BandCapability> {
        fake_band_caps()
            .into_iter()
            .map(ddk_converter::mlme_band_cap_from_softmac)
            .collect::<Result<_, _>>()
            .expect("Failed to convert softmac driver band capabilities.")
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{ddk_converter, WlanTxPacketExt as _},
        fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
        ieee80211::Ssid,
        wlan_common::assert_variant,
    };

    fn make_deauth_confirm_msg() -> fidl_mlme::DeauthenticateConfirm {
        fidl_mlme::DeauthenticateConfirm { peer_sta_address: [1; 6] }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn state_method_returns_correct_pointer() {
        let (fake_device, fake_device_state) = FakeDevice::new().await;
        assert_eq!(Arc::as_ptr(&fake_device.state()), Arc::as_ptr(&fake_device_state));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn fake_device_returns_expected_wlan_softmac_query_response() {
        let (mut fake_device, _) = FakeDevice::new().await;
        let query_response = fake_device.wlan_softmac_query_response().await.unwrap();
        assert_eq!(query_response.sta_addr, [7u8; 6].into());
        assert_eq!(query_response.mac_role, Some(fidl_common::WlanMacRole::Client));
        assert_eq!(
            query_response.supported_phys,
            Some(vec![
                fidl_common::WlanPhyType::Dsss,
                fidl_common::WlanPhyType::Hr,
                fidl_common::WlanPhyType::Ofdm,
                fidl_common::WlanPhyType::Erp,
                fidl_common::WlanPhyType::Ht,
                fidl_common::WlanPhyType::Vht,
            ]),
        );
        assert_eq!(query_response.hardware_capability, Some(0));

        let expected_band_caps = [
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::TwoGhz),
                basic_rates: Some(vec![
                    0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c,
                ]),
                operating_channels: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                }),
                vht_supported: Some(false),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities { bytes: Default::default() }),
                ..Default::default()
            },
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x02, 0x04, 0x0b, 0x16, 0x30, 0x60, 0x7e, 0x7f]),
                operating_channels: Some(vec![36, 40, 44, 48, 149, 153, 157, 161]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                }),
                vht_supported: Some(true),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities {
                    bytes: [0x32, 0x50, 0x80, 0x0f, 0xfe, 0xff, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00],
                }),
                ..Default::default()
            },
        ];
        let actual_band_caps = query_response.band_caps.as_ref().unwrap();
        for (actual_band_cap, expected_band_cap) in actual_band_caps.iter().zip(&expected_band_caps)
        {
            assert_eq!(actual_band_cap, expected_band_cap);
        }
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn fake_device_returns_expected_discovery_support() {
        let (mut fake_device, _) = FakeDevice::new().await;
        let discovery_support = fake_device.discovery_support().await.unwrap();
        assert_eq!(
            discovery_support,
            fidl_common::DiscoverySupport {
                scan_offload: fidl_common::ScanOffloadExtension {
                    supported: true,
                    scan_cancel_supported: false,
                },
                probe_response_offload: fidl_common::ProbeResponseOffloadExtension {
                    supported: false,
                },
            }
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn fake_device_returns_expected_mac_sublayer_support() {
        let (mut fake_device, _) = FakeDevice::new().await;
        let mac_sublayer_support = fake_device.mac_sublayer_support().await.unwrap();
        assert_eq!(
            mac_sublayer_support,
            fidl_common::MacSublayerSupport {
                rate_selection_offload: fidl_common::RateSelectionOffloadExtension {
                    supported: false,
                },
                data_plane: fidl_common::DataPlaneExtension {
                    data_plane_type: fidl_common::DataPlaneType::EthernetDevice,
                },
                device: fidl_common::DeviceExtension {
                    is_synthetic: true,
                    mac_implementation_type: fidl_common::MacImplementationType::Softmac,
                    tx_status_report_supported: true,
                },
            }
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn fake_device_returns_expected_security_support() {
        let (mut fake_device, _) = FakeDevice::new().await;
        let security_support = fake_device.security_support().await.unwrap();
        assert_eq!(
            security_support,
            fidl_common::SecuritySupport {
                mfp: fidl_common::MfpFeature { supported: false },
                sae: fidl_common::SaeFeature {
                    driver_handler_supported: false,
                    sme_handler_supported: false,
                },
            }
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn fake_device_returns_expected_spectrum_management_support() {
        let (mut fake_device, _) = FakeDevice::new().await;
        let spectrum_management_support = fake_device.spectrum_management_support().await.unwrap();
        assert_eq!(
            spectrum_management_support,
            fidl_common::SpectrumManagementSupport {
                dfs: fidl_common::DfsFeature { supported: true },
            }
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_can_dynamically_change_fake_device_state() {
        let (mut fake_device, fake_device_state) = FakeDevice::new_with_config(
            FakeDeviceConfig::default().with_mock_mac_role(fidl_common::WlanMacRole::Client),
        )
        .await;
        let query_response = fake_device.wlan_softmac_query_response().await.unwrap();
        assert_eq!(query_response.mac_role, Some(fidl_common::WlanMacRole::Client));

        fake_device_state.lock().config =
            FakeDeviceConfig::default().with_mock_mac_role(fidl_common::WlanMacRole::Ap);

        let query_response = fake_device.wlan_softmac_query_response().await.unwrap();
        assert_eq!(query_response.mac_role, Some(fidl_common::WlanMacRole::Ap));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn send_mlme_message() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device
            .send_mlme_event(fidl_mlme::MlmeEvent::DeauthenticateConf {
                resp: make_deauth_confirm_msg(),
            })
            .expect("error sending MLME message");

        // Read message from channel.
        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::DeauthenticateConfirm>()
            .expect("error reading message from channel");
        assert_eq!(msg, make_deauth_confirm_msg());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn send_mlme_message_peer_already_closed() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device_state.lock().mlme_event_stream.take();

        fake_device
            .send_mlme_event(fidl_mlme::MlmeEvent::DeauthenticateConf {
                resp: make_deauth_confirm_msg(),
            })
            .expect_err("Mlme event should fail");
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn fake_device_deliver_eth_frame() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        assert_eq!(fake_device_state.lock().eth_queue.len(), 0);
        let first_frame = [5; 32];
        let second_frame = [6; 32];
        assert_eq!(fake_device.deliver_eth_frame(&first_frame[..]), Ok(()));
        assert_eq!(fake_device.deliver_eth_frame(&second_frame[..]), Ok(()));
        assert_eq!(fake_device_state.lock().eth_queue.len(), 2);
        assert_eq!(&fake_device_state.lock().eth_queue[0], &first_frame);
        assert_eq!(&fake_device_state.lock().eth_queue[1], &second_frame);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn set_channel() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device
            .set_channel(fidl_common::WlanChannel {
                primary: 2,
                cbw: fidl_common::ChannelBandwidth::Cbw80P80,
                secondary80: 4,
            })
            .await
            .expect("set_channel failed?");
        // Check the internal state.
        assert_eq!(
            fake_device_state.lock().wlan_channel,
            fidl_common::WlanChannel {
                primary: 2,
                cbw: fidl_common::ChannelBandwidth::Cbw80P80,
                secondary80: 4
            }
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn install_key() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device
            .install_key(&fidl_softmac::WlanKeyConfiguration {
                protection: Some(fidl_softmac::WlanProtection::None),
                cipher_oui: Some([3, 4, 5]),
                cipher_type: Some(6),
                key_type: Some(fidl_common::WlanKeyType::Pairwise),
                peer_addr: Some([8; 6]),
                key_idx: Some(9),
                key: Some(vec![11; 32]),
                rsc: Some(12),
                ..Default::default()
            })
            .await
            .expect("error setting key");
        assert_eq!(fake_device_state.lock().keys.len(), 1);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_passive_scan() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;

        let result = fake_device
            .start_passive_scan(&fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest {
                channels: Some(vec![1u8, 2, 3]),
                min_channel_time: Some(zx::Duration::from_millis(0).into_nanos()),
                max_channel_time: Some(zx::Duration::from_millis(200).into_nanos()),
                min_home_time: Some(0),
                ..Default::default()
            })
            .await;
        assert!(result.is_ok());

        assert_eq!(
            fake_device_state.lock().captured_passive_scan_request,
            Some(fidl_softmac::WlanSoftmacBaseStartPassiveScanRequest {
                channels: Some(vec![1, 2, 3]),
                min_channel_time: Some(0),
                max_channel_time: Some(200_000_000),
                min_home_time: Some(0),
                ..Default::default()
            }),
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn start_active_scan() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;

        let result = fake_device
            .start_active_scan(&fidl_softmac::WlanSoftmacStartActiveScanRequest {
                channels: Some(vec![1u8, 2, 3]),
                ssids: Some(vec![
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("foo").unwrap().into(),
                    ),
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("bar").unwrap().into(),
                    ),
                ]),
                mac_header: Some(vec![
                    0x40u8, 0x00, // Frame Control
                    0x00, 0x00, // Duration
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
                    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
                    0x70, 0xdc, // Sequence Control
                ]),
                ies: Some(vec![
                    0x01u8, // Element ID for Supported Rates
                    0x08,   // Length
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // Supported Rates
                ]),
                min_channel_time: Some(zx::Duration::from_millis(0).into_nanos()),
                max_channel_time: Some(zx::Duration::from_millis(200).into_nanos()),
                min_home_time: Some(0),
                min_probes_per_channel: Some(1),
                max_probes_per_channel: Some(3),
                ..Default::default()
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(
            fake_device_state.lock().captured_active_scan_request,
            Some(fidl_softmac::WlanSoftmacStartActiveScanRequest {
                channels: Some(vec![1, 2, 3]),
                ssids: Some(vec![
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("foo").unwrap().into()
                    ),
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("bar").unwrap().into()
                    ),
                ]),
                mac_header: Some(vec![
                    0x40, 0x00, // Frame Control
                    0x00, 0x00, // Duration
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
                    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
                    0x70, 0xdc, // Sequence Control
                ]),
                ies: Some(vec![
                    0x01, // Element ID for Supported Rates
                    0x08, // Length
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 // Supported Rates
                ]),
                min_channel_time: Some(0),
                max_channel_time: Some(200_000_000),
                min_home_time: Some(0),
                min_probes_per_channel: Some(1),
                max_probes_per_channel: Some(3),
                ..Default::default()
            }),
            "No active scan argument available."
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn join_bss() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device
            .join_bss(&fidl_common::JoinBssRequest {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                bss_type: Some(fidl_common::BssType::Personal),
                remote: Some(true),
                beacon_period: Some(100),
                ..Default::default()
            })
            .await
            .expect("error configuring bss");
        assert!(fake_device_state.lock().join_bss_request.is_some());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn enable_disable_beaconing() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        let mut buffer =
            fake_device_state.lock().buffer_provider.get_buffer(4).expect("error getting buffer");
        buffer.copy_from_slice(&[1, 2, 3, 4][..]);
        let mac_frame = buffer.to_vec();

        fake_device
            .enable_beaconing(fidl_softmac::WlanSoftmacBaseEnableBeaconingRequest {
                packet_template: Some(fidl_softmac::WlanTxPacket::template(mac_frame)),
                tim_ele_offset: Some(1),
                beacon_interval: Some(2),
                ..Default::default()
            })
            .await
            .expect("error enabling beaconing");
        assert_variant!(
        fake_device_state.lock().beacon_config.as_ref(),
        Some((buffer, tim_ele_offset, beacon_interval)) => {
            assert_eq!(&buffer[..], &[1, 2, 3, 4][..]);
            assert_eq!(*tim_ele_offset, 1);
            assert_eq!(*beacon_interval, TimeUnit(2));
        });
        fake_device.disable_beaconing().await.expect("error disabling beaconing");
        assert_variant!(fake_device_state.lock().beacon_config.as_ref(), None);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn set_ethernet_status() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device.set_ethernet_up().expect("failed setting status");
        assert_eq!(fake_device_state.lock().link_status, LinkStatus::UP);

        fake_device.set_ethernet_down().expect("failed setting status");
        assert_eq!(fake_device_state.lock().link_status, LinkStatus::DOWN);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn notify_association_complete() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device
            .notify_association_complete(fidl_softmac::WlanAssociationConfig {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                aid: Some(1),
                listen_interval: Some(2),
                channel: Some(fidl_common::WlanChannel {
                    primary: 3,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                }),
                qos: Some(false),
                wmm_params: None,
                rates: None,
                capability_info: Some(0x0102),
                ht_cap: None,
                ht_op: None,
                vht_cap: None,
                vht_op: None,
                ..Default::default()
            })
            .await
            .expect("error configuring assoc");
        assert!(fake_device_state.lock().assocs.contains_key(&[1, 2, 3, 4, 5, 6].into()));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn clear_association() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;
        fake_device
            .join_bss(&fidl_common::JoinBssRequest {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                bss_type: Some(fidl_common::BssType::Personal),
                remote: Some(true),
                beacon_period: Some(100),
                ..Default::default()
            })
            .await
            .expect("error configuring bss");

        let assoc_cfg = fidl_softmac::WlanAssociationConfig {
            bssid: Some([1, 2, 3, 4, 5, 6]),
            aid: Some(1),
            channel: Some(fidl_common::WlanChannel {
                primary: 149,
                cbw: fidl_common::ChannelBandwidth::Cbw40,
                secondary80: 42,
            }),
            ..Default::default()
        };

        assert!(fake_device_state.lock().join_bss_request.is_some());
        fake_device.notify_association_complete(assoc_cfg).await.expect("error configuring assoc");
        assert_eq!(fake_device_state.lock().assocs.len(), 1);
        fake_device
            .clear_association(&fidl_softmac::WlanSoftmacBaseClearAssociationRequest {
                peer_addr: Some([1, 2, 3, 4, 5, 6]),
                ..Default::default()
            })
            .await
            .expect("error clearing assoc");
        assert_eq!(fake_device_state.lock().assocs.len(), 0);
        assert!(fake_device_state.lock().join_bss_request.is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn fake_device_captures_update_wmm_parameters_request() {
        let (mut fake_device, fake_device_state) = FakeDevice::new().await;

        let request = fidl_softmac::WlanSoftmacBaseUpdateWmmParametersRequest {
            ac: Some(fidl_ieee80211::WlanAccessCategory::Background),
            params: Some(fidl_common::WlanWmmParameters {
                apsd: true,
                ac_be_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 10,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
                ac_bk_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 11,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
                ac_vi_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 12,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
                ac_vo_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 13,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
            }),
            ..Default::default()
        };
        let result = fake_device.update_wmm_parameters(&request).await;
        assert!(result.is_ok());

        assert_eq!(fake_device_state.lock().captured_update_wmm_parameters_request, Some(request),);
    }
}
