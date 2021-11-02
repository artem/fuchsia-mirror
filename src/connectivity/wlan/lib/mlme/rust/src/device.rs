// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{buffer::OutBuf, key},
    banjo_fuchsia_hardware_wlan_info::*,
    banjo_fuchsia_hardware_wlan_mac::{
        self as banjo_wlan_mac, WlanHwScanConfig, WlanHwScanResult, WlanRxPacket, WlanTxPacket,
        WlanTxStatus, WlanmacInfo,
    },
    banjo_fuchsia_wlan_common as banjo_common,
    banjo_fuchsia_wlan_internal::BssConfig,
    fidl_fuchsia_wlan_mlme as fidl_mlme, fuchsia_zircon as zx,
    ieee80211::MacAddr,
    std::ffi::c_void,
    wlan_common::{mac::FrameControl, tx_vector, TimeUnit},
};

#[cfg(test)]
pub use test_utils::*;

#[derive(Debug, PartialEq)]
pub struct LinkStatus(u8);
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

#[derive(Debug, PartialEq, Eq)]
pub struct TxFlags(pub u32);
impl TxFlags {
    pub const NONE: Self = Self(0);

    pub const PROTECTED: Self = Self(1);
    pub const FAVOR_RELIABILITY: Self = Self(1 << 1);
    // TODO(fxbug.dev/29622): remove once MLME supports QoS tag.
    pub const QOS: Self = Self(1 << 2);
}

pub struct Device {
    raw_device: DeviceInterface,
    minstrel: Option<crate::MinstrelWrapper>,
    control_handle: fidl_mlme::MlmeControlHandle,
}

const REQUIRED_WLAN_HEADER_LEN: usize = 10;
const PEER_ADDR_OFFSET: usize = 4;

impl Device {
    pub fn new(
        raw_device: DeviceInterface,
        minstrel: Option<crate::MinstrelWrapper>,
        control_handle: fidl_mlme::MlmeControlHandle,
    ) -> Self {
        Self { raw_device, minstrel, control_handle }
    }
    pub fn mlme_control_handle(&self) -> &fidl_mlme::MlmeControlHandle {
        &self.control_handle
    }

    pub fn deliver_eth_frame(&self, slice: &[u8]) -> Result<(), zx::Status> {
        self.raw_device.deliver_eth_frame(slice)
    }

    pub fn send_wlan_frame(&self, buf: OutBuf, mut flags: TxFlags) -> Result<(), zx::Status> {
        if buf.as_slice().len() < REQUIRED_WLAN_HEADER_LEN {
            return Err(zx::Status::BUFFER_TOO_SMALL);
        }
        // Unwrap is safe since the byte slice is always the same size.
        let frame_control =
            zerocopy::LayoutVerified::<&[u8], FrameControl>::new(&buf.as_slice()[0..=1])
                .unwrap()
                .into_ref();
        if frame_control.protected() {
            flags.0 |= banjo_wlan_mac::WlanTxInfoFlags::PROTECTED.0 as u32;
        }
        let mut peer_addr = [0u8; 6];
        peer_addr.copy_from_slice(&buf.as_slice()[PEER_ADDR_OFFSET..PEER_ADDR_OFFSET + 6]);
        let tx_vector_idx = self
            .minstrel
            .as_ref()
            .and_then(|minstrel| {
                let mut banjo_flags = banjo_wlan_mac::WlanTxInfoFlags(0);
                if flags.0 & TxFlags::FAVOR_RELIABILITY.0 != 0 {
                    banjo_flags |= banjo_wlan_mac::WlanTxInfoFlags::FAVOR_RELIABILITY;
                }
                if flags.0 & TxFlags::PROTECTED.0 != 0 {
                    banjo_flags |= banjo_wlan_mac::WlanTxInfoFlags::PROTECTED;
                }
                if flags.0 & TxFlags::QOS.0 != 0 {
                    banjo_flags |= banjo_wlan_mac::WlanTxInfoFlags::QOS;
                }
                minstrel.lock().get_tx_vector_idx(frame_control, &peer_addr, banjo_flags)
            })
            .unwrap_or_else(|| {
                // We either don't have minstrel, or minstrel failed to generate a tx vector.
                // Use a reasonable default value instead.
                // Note: This section has no practical effect on ath10k. It is
                // only effective if the underlying device meets both criteria below:
                // 1. Does not support tx status report.
                // 2. Honors our instruction on tx_vector to use.
                // TODO(fxbug.dev/28893): Choose an optimal MCS for management frames
                // TODO(fxbug.dev/43456): Log stats about minstrel usage vs default tx vector.
                let mcs_idx = if frame_control.is_data() { 7 } else { 3 };
                tx_vector::TxVector::new(
                    WlanPhyType::ERP,
                    WlanGi::G_800NS,
                    banjo_common::ChannelBandwidth::CBW20,
                    mcs_idx,
                )
                .unwrap()
                .to_idx()
            });

        let tx_info = wlan_common::tx_vector::TxVector::from_idx(tx_vector_idx)
            .to_banjo_tx_info(flags.0, self.minstrel.is_some());
        self.raw_device.queue_tx(0, buf, tx_info)
    }

    pub fn set_eth_status(&self, status: u32) {
        self.raw_device.set_eth_status(status);
    }

    pub fn set_channel(&self, channel: banjo_common::WlanChannel) -> Result<(), zx::Status> {
        self.raw_device.set_channel(channel)
    }

    pub fn set_key(&self, key: key::KeyConfig) -> Result<(), zx::Status> {
        self.raw_device.set_key(key)
    }

    pub fn start_hw_scan(&self, config: &WlanHwScanConfig) -> Result<(), zx::Status> {
        self.raw_device.start_hw_scan(config)
    }

    pub fn channel(&self) -> banjo_common::WlanChannel {
        self.raw_device.channel()
    }

    pub fn wlanmac_info(&self) -> WlanmacInfo {
        self.raw_device.wlanmac_info()
    }

    pub fn configure_bss(&self, cfg: BssConfig) -> Result<(), zx::Status> {
        self.raw_device.configure_bss(cfg)
    }

    pub fn enable_beaconing(
        &self,
        buf: OutBuf,
        tim_ele_offset: usize,
        beacon_interval: TimeUnit,
    ) -> Result<(), zx::Status> {
        self.raw_device.enable_beaconing(buf, tim_ele_offset, beacon_interval)
    }

    pub fn disable_beaconing(&self) -> Result<(), zx::Status> {
        self.raw_device.disable_beaconing()
    }

    pub fn configure_beacon(&self, buf: OutBuf) -> Result<(), zx::Status> {
        self.raw_device.configure_beacon(buf)
    }

    pub fn set_eth_link(&self, status: LinkStatus) -> Result<(), zx::Status> {
        self.raw_device.set_eth_link(status)
    }

    pub fn set_eth_link_up(&self) -> Result<(), zx::Status> {
        self.raw_device.set_eth_link(LinkStatus::UP)
    }

    pub fn set_eth_link_down(&self) -> Result<(), zx::Status> {
        self.raw_device.set_eth_link(LinkStatus::DOWN)
    }

    pub fn configure_assoc(&self, assoc_ctx: WlanAssocCtx) -> Result<(), zx::Status> {
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().add_peer(&assoc_ctx);
        }
        self.raw_device.configure_assoc(assoc_ctx)
    }

    pub fn clear_assoc(&self, addr: &MacAddr) -> Result<(), zx::Status> {
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().remove_peer(addr);
        }
        self.raw_device.clear_assoc(addr)
    }
}

/// Hand-rolled Rust version of the banjo wlanmac_ifc_protocol for communication from the driver up.
/// Note that we copy the individual fns out of this struct into the equivalent generated struct
/// in C++. Thanks to cbindgen, this gives us a compile-time confirmation that our function
/// signatures are correct.
#[repr(C)]
pub struct WlanmacIfcProtocol<'a> {
    ops: *const WlanmacIfcProtocolOps,
    ctx: &'a mut crate::DriverEventSink,
}

#[repr(C)]
pub struct WlanmacIfcProtocolOps {
    status: extern "C" fn(ctx: &mut crate::DriverEventSink, status: u32),
    recv: extern "C" fn(ctx: &mut crate::DriverEventSink, packet: *const WlanRxPacket),
    complete_tx: extern "C" fn(
        ctx: &'static mut crate::DriverEventSink,
        packet: *const WlanTxPacket,
        status: i32,
    ),
    indication: extern "C" fn(ctx: &mut crate::DriverEventSink, ind: u32),
    report_tx_status:
        extern "C" fn(ctx: &mut crate::DriverEventSink, tx_status: *const WlanTxStatus),
    hw_scan_complete:
        extern "C" fn(ctx: &mut crate::DriverEventSink, result: *const WlanHwScanResult),
}

#[no_mangle]
extern "C" fn handle_status(ctx: &mut crate::DriverEventSink, status: u32) {
    let _ = ctx.0.unbounded_send(crate::DriverEvent::Status { status });
}
#[no_mangle]
extern "C" fn handle_recv(ctx: &mut crate::DriverEventSink, packet: *const WlanRxPacket) {
    // TODO(fxbug.dev/29063): C++ uses a buffer allocator for this, determine if we need one.
    let bytes =
        unsafe { std::slice::from_raw_parts((*packet).mac_frame_buffer, (*packet).mac_frame_size) }
            .into();
    let rx_info = unsafe { (*packet).info };
    let _ = ctx.0.unbounded_send(crate::DriverEvent::MacFrameRx { bytes, rx_info });
}
#[no_mangle]
extern "C" fn handle_complete_tx(
    _ctx: &mut crate::DriverEventSink,
    _packet: *const WlanTxPacket,
    _status: i32,
) {
    // TODO(fxbug.dev/85924): Implement this to support asynchronous packet delivery.
}
#[no_mangle]
extern "C" fn handle_indication(ctx: &mut crate::DriverEventSink, ind: u32) {
    // TODO(fxbug.dev/86141): Determine if we should support this.
    let _ = ctx.0.unbounded_send(crate::DriverEvent::HwIndication {
        ind: banjo_wlan_mac::WlanIndication(ind as u8),
    });
}
#[no_mangle]
extern "C" fn handle_report_tx_status(
    ctx: &mut crate::DriverEventSink,
    tx_status: *const WlanTxStatus,
) {
    if tx_status.is_null() {
        return;
    }
    let tx_status = unsafe { *tx_status };
    let _ = ctx.0.unbounded_send(crate::DriverEvent::TxStatusReport { tx_status });
}
#[no_mangle]
extern "C" fn handle_hw_scan_complete(
    ctx: &mut crate::DriverEventSink,
    result: *const WlanHwScanResult,
) {
    if result.is_null() {
        return;
    }
    let result = unsafe { *result };
    let ind = match result.code {
        banjo_wlan_mac::WlanHwScan::SUCCESS => banjo_wlan_mac::WlanIndication::HW_SCAN_COMPLETE,
        banjo_wlan_mac::WlanHwScan::ABORTED => banjo_wlan_mac::WlanIndication::HW_SCAN_ABORTED,
        _ => return,
    };
    let _ = ctx.0.unbounded_send(crate::DriverEvent::HwIndication { ind });
}

const PROTOCOL_OPS: WlanmacIfcProtocolOps = WlanmacIfcProtocolOps {
    status: handle_status,
    recv: handle_recv,
    complete_tx: handle_complete_tx,
    indication: handle_indication,
    report_tx_status: handle_report_tx_status,
    hw_scan_complete: handle_hw_scan_complete,
};

impl<'a> WlanmacIfcProtocol<'a> {
    pub(crate) fn new(sink: &'a mut crate::DriverEventSink) -> Self {
        // Const reference has 'static lifetime, so it's safe to pass down to the driver.
        let ops = &PROTOCOL_OPS;
        Self { ops, ctx: sink }
    }
}

// Our device is used inside a separate worker thread, so we force Rust to allow this.
unsafe impl Send for DeviceInterface {}

/// A `Device` allows transmitting frames and MLME messages.
#[repr(C)]
pub struct DeviceInterface {
    device: *mut c_void,
    /// Start operations on the underlying device and return the SME channel.
    start: extern "C" fn(
        device: *mut c_void,
        ifc: *const WlanmacIfcProtocol<'_>,
        out_sme_channel: *mut zx::sys::zx_handle_t,
    ) -> i32,
    /// Request to deliver an Ethernet II frame to Fuchsia's Netstack.
    deliver_eth_frame: extern "C" fn(device: *mut c_void, data: *const u8, len: usize) -> i32,
    /// Deliver a WLAN frame directly through the firmware.
    queue_tx: extern "C" fn(
        device: *mut c_void,
        options: u32,
        buf: OutBuf,
        tx_info: banjo_wlan_mac::WlanTxInfo,
    ) -> i32,
    /// Reports the current status to the ethernet driver.
    set_eth_status: extern "C" fn(device: *mut c_void, status: u32),
    /// Returns the currently set WLAN channel.
    get_wlan_channel: extern "C" fn(device: *mut c_void) -> banjo_common::WlanChannel,
    /// Request the PHY to change its channel. If successful, get_wlan_channel will return the
    /// chosen channel.
    set_wlan_channel: extern "C" fn(device: *mut c_void, channel: banjo_common::WlanChannel) -> i32,
    /// Set a key on the device.
    /// |key| is mutable because the underlying API does not take a const wlan_key_config_t.
    set_key: extern "C" fn(device: *mut c_void, key: *mut key::KeyConfig) -> i32,
    /// Make scan request to the driver
    start_hw_scan: extern "C" fn(device: *mut c_void, config: *const WlanHwScanConfig) -> i32,
    /// Get information and capabilities of this WLAN interface
    get_wlanmac_info: extern "C" fn(device: *mut c_void) -> WlanmacInfo,
    /// Configure the device's BSS.
    /// |cfg| is mutable because the underlying API does not take a const bss_config_t.
    configure_bss: extern "C" fn(device: *mut c_void, cfg: *mut BssConfig) -> i32,
    /// Enable hardware offload of beaconing on the device.
    enable_beaconing: extern "C" fn(
        device: *mut c_void,
        buf: OutBuf,
        tim_ele_offset: usize,
        beacon_interval: u16,
    ) -> i32,
    /// Disable beaconing on the device.
    disable_beaconing: extern "C" fn(device: *mut c_void) -> i32,
    /// Reconfigure the enabled beacon on the device.
    configure_beacon: extern "C" fn(device: *mut c_void, buf: OutBuf) -> i32,
    /// Sets the link status to be UP or DOWN.
    set_link_status: extern "C" fn(device: *mut c_void, status: u8) -> i32,
    /// Configure the association context.
    /// |assoc_ctx| is mutable because the underlying API does not take a const wlan_assoc_ctx_t.
    configure_assoc: extern "C" fn(device: *mut c_void, assoc_ctx: *mut WlanAssocCtx) -> i32,
    /// Clear the association context.
    clear_assoc: extern "C" fn(device: *mut c_void, addr: &[u8; 6]) -> i32,
}

impl DeviceInterface {
    pub fn start(&self, ifc: *const WlanmacIfcProtocol<'_>) -> Result<zx::Handle, zx::Status> {
        let mut out_channel = 0;
        let status = (self.start)(self.device, ifc, &mut out_channel as *mut u32);
        // Unsafe block required because we cannot pass a Rust handle over FFI. An invalid
        // handle violates the banjo API, and may be detected by the caller of this fn.
        zx::ok(status).map(|_| unsafe { zx::Handle::from_raw(out_channel) })
    }
    fn deliver_eth_frame(&self, slice: &[u8]) -> Result<(), zx::Status> {
        let status = (self.deliver_eth_frame)(self.device, slice.as_ptr(), slice.len());
        zx::ok(status)
    }

    fn queue_tx(
        &self,
        options: u32,
        buf: OutBuf,
        tx_info: banjo_wlan_mac::WlanTxInfo,
    ) -> Result<(), zx::Status> {
        let status = (self.queue_tx)(self.device, options, buf, tx_info);
        zx::ok(status)
    }

    fn set_eth_status(&self, status: u32) {
        (self.set_eth_status)(self.device, status);
    }

    fn set_channel(&self, channel: banjo_common::WlanChannel) -> Result<(), zx::Status> {
        let status = (self.set_wlan_channel)(self.device, channel);
        zx::ok(status)
    }

    fn set_key(&self, mut key: key::KeyConfig) -> Result<(), zx::Status> {
        let status = (self.set_key)(self.device, &mut key as *mut key::KeyConfig);
        zx::ok(status)
    }

    fn start_hw_scan(&self, config: &WlanHwScanConfig) -> Result<(), zx::Status> {
        let status = (self.start_hw_scan)(self.device, config as *const WlanHwScanConfig);
        zx::ok(status)
    }

    fn channel(&self) -> banjo_common::WlanChannel {
        (self.get_wlan_channel)(self.device)
    }

    pub fn wlanmac_info(&self) -> WlanmacInfo {
        (self.get_wlanmac_info)(self.device)
    }

    fn configure_bss(&self, mut cfg: BssConfig) -> Result<(), zx::Status> {
        let status = (self.configure_bss)(self.device, &mut cfg as *mut BssConfig);
        zx::ok(status)
    }

    fn enable_beaconing(
        &self,
        buf: OutBuf,
        tim_ele_offset: usize,
        beacon_interval: TimeUnit,
    ) -> Result<(), zx::Status> {
        let status =
            (self.enable_beaconing)(self.device, buf, tim_ele_offset, beacon_interval.into());
        zx::ok(status)
    }

    fn disable_beaconing(&self) -> Result<(), zx::Status> {
        let status = (self.disable_beaconing)(self.device);
        zx::ok(status)
    }

    fn configure_beacon(&self, buf: OutBuf) -> Result<(), zx::Status> {
        let status = (self.configure_beacon)(self.device, buf);
        zx::ok(status)
    }

    fn set_eth_link(&self, status: LinkStatus) -> Result<(), zx::Status> {
        let status = (self.set_link_status)(self.device, status.0);
        zx::ok(status)
    }

    fn configure_assoc(&self, mut assoc_ctx: WlanAssocCtx) -> Result<(), zx::Status> {
        let status = (self.configure_assoc)(self.device, &mut assoc_ctx as *mut WlanAssocCtx);
        zx::ok(status)
    }

    fn clear_assoc(&self, addr: &MacAddr) -> Result<(), zx::Status> {
        let status = (self.clear_assoc)(self.device, addr);
        zx::ok(status)
    }
}

#[cfg(test)]
macro_rules! arr {
    ($slice:expr, $size:expr) => {{
        assert!($slice.len() < $size);
        let mut a = [0; $size];
        a[..$slice.len()].clone_from_slice(&$slice);
        a
    }};
}

#[cfg(test)]
mod test_utils {
    use {
        super::*,
        crate::{
            buffer::{BufferProvider, FakeBufferProvider},
            error::Error,
            test_utils::fake_control_handle,
        },
        banjo_ddk_hw_wlan_ieee80211::*,
        banjo_ddk_hw_wlan_wlaninfo::*,
        fuchsia_async as fasync,
    };

    pub struct FakeDevice {
        pub eth_queue: Vec<Vec<u8>>,
        pub wlan_queue: Vec<(Vec<u8>, u32)>,
        pub sme_sap: (fidl_mlme::MlmeControlHandle, zx::Channel),
        pub wlan_channel: banjo_common::WlanChannel,
        pub keys: Vec<key::KeyConfig>,
        pub hw_scan_req: Option<WlanHwScanConfig>,
        pub info: WlanmacInfo,
        pub bss_cfg: Option<BssConfig>,
        pub bcn_cfg: Option<(Vec<u8>, usize, TimeUnit)>,
        pub link_status: LinkStatus,
        pub assocs: std::collections::HashMap<MacAddr, WlanAssocCtx>,
        pub buffer_provider: BufferProvider,
    }

    impl FakeDevice {
        pub fn new(executor: &fasync::TestExecutor) -> Self {
            let sme_sap = fake_control_handle(&executor);
            Self {
                eth_queue: vec![],
                wlan_queue: vec![],
                sme_sap,
                wlan_channel: banjo_common::WlanChannel {
                    primary: 0,
                    cbw: banjo_common::ChannelBandwidth::CBW20,
                    secondary80: 0,
                },
                hw_scan_req: None,
                info: fake_wlanmac_info(),
                keys: vec![],
                bss_cfg: None,
                bcn_cfg: None,
                link_status: LinkStatus::DOWN,
                assocs: std::collections::HashMap::new(),
                buffer_provider: FakeBufferProvider::new(),
            }
        }

        pub extern "C" fn start(
            _device: *mut c_void,
            _ifc: *const WlanmacIfcProtocol<'_>,
            _out_sme_channel: *mut zx::sys::zx_handle_t,
        ) -> i32 {
            // TODO(fxbug.dev/45464): Implement when AP tests are ported to Rust.
            zx::sys::ZX_OK
        }

        pub extern "C" fn deliver_eth_frame(
            device: *mut c_void,
            data: *const u8,
            len: usize,
        ) -> i32 {
            assert!(!device.is_null());
            assert!(!data.is_null());
            assert_ne!(len, 0);
            // safe here because slice will not outlive data
            let slice = unsafe { std::slice::from_raw_parts(data, len) };
            // safe here because device_ptr alwyas points to self
            unsafe {
                (*(device as *mut Self)).eth_queue.push(slice.to_vec());
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn queue_tx(
            device: *mut c_void,
            _options: u32,
            buf: OutBuf,
            _tx_info: banjo_wlan_mac::WlanTxInfo,
        ) -> i32 {
            assert!(!device.is_null());
            unsafe {
                (*(device as *mut Self)).wlan_queue.push((buf.as_slice().to_vec(), 0));
            }
            buf.free();
            zx::sys::ZX_OK
        }

        pub extern "C" fn set_eth_status(_device: *mut c_void, _status: u32) {}

        pub extern "C" fn set_link_status(device: *mut c_void, status: u8) -> i32 {
            assert!(!device.is_null());
            // safe here because device_ptr always points to Self
            unsafe {
                (*(device as *mut Self)).link_status = LinkStatus(status);
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn queue_tx_with_failure(
            _: *mut c_void,
            _: u32,
            buf: OutBuf,
            _: banjo_wlan_mac::WlanTxInfo,
        ) -> i32 {
            buf.free();
            zx::sys::ZX_ERR_IO
        }

        pub extern "C" fn get_wlan_channel(device: *mut c_void) -> banjo_common::WlanChannel {
            unsafe { (*(device as *const Self)).wlan_channel }
        }

        pub extern "C" fn set_wlan_channel(
            device: *mut c_void,
            wlan_channel: banjo_common::WlanChannel,
        ) -> i32 {
            unsafe {
                (*(device as *mut Self)).wlan_channel = wlan_channel;
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn set_key(device: *mut c_void, key: *mut key::KeyConfig) -> i32 {
            unsafe {
                (*(device as *mut Self)).keys.push((*key).clone());
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn start_hw_scan(
            device: *mut c_void,
            config: *const WlanHwScanConfig,
        ) -> i32 {
            unsafe {
                (*(device as *mut Self)).hw_scan_req = Some((*config).clone());
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn start_hw_scan_fails(
            _device: *mut c_void,
            _config: *const WlanHwScanConfig,
        ) -> i32 {
            zx::sys::ZX_ERR_NOT_SUPPORTED
        }

        pub extern "C" fn get_wlanmac_info(device: *mut c_void) -> WlanmacInfo {
            unsafe { (*(device as *const Self)).info }
        }

        pub extern "C" fn configure_bss(device: *mut c_void, cfg: *mut BssConfig) -> i32 {
            unsafe {
                (*(device as *mut Self)).bss_cfg.replace((*cfg).clone());
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn enable_beaconing(
            device: *mut c_void,
            buf: OutBuf,
            tim_ele_offset: usize,
            beacon_interval: u16,
        ) -> i32 {
            unsafe {
                (*(device as *mut Self)).bcn_cfg =
                    Some((buf.as_slice().to_vec(), tim_ele_offset, TimeUnit(beacon_interval)));
                buf.free();
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn disable_beaconing(device: *mut c_void) -> i32 {
            unsafe {
                (*(device as *mut Self)).bcn_cfg = None;
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn configure_beacon(device: *mut c_void, buf: OutBuf) -> i32 {
            unsafe {
                if let Some((_, tim_ele_offset, beacon_interval)) = (*(device as *mut Self)).bcn_cfg
                {
                    (*(device as *mut Self)).bcn_cfg =
                        Some((buf.as_slice().to_vec(), tim_ele_offset, beacon_interval));
                    buf.free();
                    zx::sys::ZX_OK
                } else {
                    zx::sys::ZX_ERR_BAD_STATE
                }
            }
        }

        pub extern "C" fn configure_assoc(device: *mut c_void, cfg: *mut WlanAssocCtx) -> i32 {
            unsafe {
                (*(device as *mut Self)).assocs.insert((*cfg).bssid, (*cfg).clone());
            }
            zx::sys::ZX_OK
        }

        pub extern "C" fn clear_assoc(device: *mut c_void, addr: &MacAddr) -> i32 {
            unsafe {
                (*(device as *mut Self)).assocs.remove(addr);
            }
            zx::sys::ZX_OK
        }

        pub fn next_mlme_msg<T: fidl::encoding::Decodable>(&mut self) -> Result<T, Error> {
            use fidl::encoding::{decode_transaction_header, Decodable, Decoder};

            let mut buf = zx::MessageBuf::new();
            let () =
                self.sme_sap.1.read(&mut buf).map_err(|status| {
                    Error::Status(format!("error reading MLME message"), status)
                })?;

            let (header, tail): (_, &[u8]) = decode_transaction_header(buf.bytes())?;
            let mut msg = Decodable::new_empty();
            Decoder::decode_into(&header, tail, &mut [], &mut msg)
                .expect("error decoding MLME message");
            Ok(msg)
        }

        pub fn reset(&mut self) {
            self.eth_queue.clear();
        }

        pub fn as_device(&mut self) -> Device {
            let raw_device = DeviceInterface {
                device: self as *mut Self as *mut c_void,
                start: Self::start,
                deliver_eth_frame: Self::deliver_eth_frame,
                queue_tx: Self::queue_tx,
                set_eth_status: Self::set_eth_status,
                get_wlan_channel: Self::get_wlan_channel,
                set_wlan_channel: Self::set_wlan_channel,
                set_key: Self::set_key,
                start_hw_scan: Self::start_hw_scan,
                get_wlanmac_info: Self::get_wlanmac_info,
                configure_bss: Self::configure_bss,
                enable_beaconing: Self::enable_beaconing,
                disable_beaconing: Self::disable_beaconing,
                configure_beacon: Self::configure_beacon,
                set_link_status: Self::set_link_status,
                configure_assoc: Self::configure_assoc,
                clear_assoc: Self::clear_assoc,
            };
            Device { raw_device, minstrel: None, control_handle: self.sme_sap.0.clone() }
        }

        pub fn as_device_fail_wlan_tx(&mut self) -> Device {
            let mut dev = self.as_device();
            dev.raw_device.queue_tx = Self::queue_tx_with_failure;
            dev
        }

        pub fn as_device_fail_start_hw_scan(&mut self) -> Device {
            let mut dev = self.as_device();
            dev.raw_device.start_hw_scan = Self::start_hw_scan_fails;
            dev
        }
    }

    pub fn fake_wlanmac_info() -> WlanmacInfo {
        let bands_count = 2;
        let mut bands = [default_band_info(); WLAN_INFO_MAX_BANDS as usize];
        bands[0] = WlanInfoBandInfo {
            band: WlanInfoBand::TWO_GHZ,
            rates: arr!([12, 24, 48, 54, 96, 108], WLAN_INFO_BAND_INFO_MAX_RATES as usize),
            supported_channels: WlanInfoChannelList {
                base_freq: 2407,
                channels: arr!(
                    [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
                    WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize
                ),
            },
            ht_supported: true,
            ht_caps: ht_cap(),
            vht_supported: false,
            vht_caps: Ieee80211VhtCapabilities {
                vht_capability_info: 0,
                supported_vht_mcs_and_nss_set: 0,
            },
        };
        bands[1] = WlanInfoBandInfo {
            band: WlanInfoBand::FIVE_GHZ,
            rates: arr!([12, 24, 48, 54, 96, 108], WLAN_INFO_BAND_INFO_MAX_RATES as usize),
            supported_channels: WlanInfoChannelList {
                base_freq: 5000,
                channels: arr!(
                    [36, 40, 44, 48, 149, 153, 157, 161],
                    WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize
                ),
            },
            ht_supported: true,
            ht_caps: ht_cap(),
            vht_supported: false,
            vht_caps: Ieee80211VhtCapabilities {
                vht_capability_info: 0x0f805032,
                supported_vht_mcs_and_nss_set: 0x0000fffe0000fffe,
            },
        };

        WlanmacInfo {
            sta_addr: [7u8; 6],
            mac_role: WlanInfoMacRole::CLIENT,
            supported_phys: WlanInfoPhyType::ERP | WlanInfoPhyType::HT | WlanInfoPhyType::VHT,
            driver_features: WlanInfoDriverFeature(0),
            caps: WlanInfoHardwareCapability(0),
            bands,
            bands_count,
        }
    }

    fn ht_cap() -> Ieee80211HtCapabilities {
        Ieee80211HtCapabilities {
            ht_capability_info: 0x0063,
            ampdu_params: 0x17,
            supported_mcs_set: Ieee80211HtCapabilitiesSupportedMcsSet {
                bytes: [
                    // Rx MCS bitmask, Supported MCS values: 0-7
                    0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    // Tx parameters
                    0x01, 0x00, 0x00, 0x00,
                ],
            },
            ht_ext_capabilities: 0,
            tx_beamforming_capabilities: 0,
            asel_capabilities: 0,
        }
    }

    /// Placeholder for default initialization of WlanInfoBandInfo, used for case where we don't
    /// care about the exact information, but the type demands it.
    pub const fn default_band_info() -> WlanInfoBandInfo {
        WlanInfoBandInfo {
            band: WlanInfoBand(0),
            ht_supported: false,
            ht_caps: Ieee80211HtCapabilities {
                ht_capability_info: 0,
                ampdu_params: 0,
                supported_mcs_set: Ieee80211HtCapabilitiesSupportedMcsSet { bytes: [0; 16] },
                ht_ext_capabilities: 0,
                tx_beamforming_capabilities: 0,
                asel_capabilities: 0,
            },
            vht_supported: false,
            vht_caps: Ieee80211VhtCapabilities {
                vht_capability_info: 0,
                supported_vht_mcs_and_nss_set: 0,
            },
            rates: [0; WLAN_INFO_BAND_INFO_MAX_RATES as usize],
            supported_channels: WlanInfoChannelList {
                base_freq: 0,
                channels: [0; WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::ddk_converter, banjo_ddk_hw_wlan_ieee80211::*,
        banjo_ddk_hw_wlan_wlaninfo::*, banjo_fuchsia_hardware_wlan_mac::WlanHwScanType,
        banjo_fuchsia_wlan_ieee80211 as banjo_ieee80211,
        banjo_fuchsia_wlan_internal as banjo_internal,
        fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fuchsia_async as fasync,
        wlan_common::assert_variant,
    };

    fn make_auth_confirm_msg() -> fidl_mlme::AuthenticateConfirm {
        fidl_mlme::AuthenticateConfirm {
            peer_sta_address: [1; 6],
            auth_type: fidl_mlme::AuthenticationTypes::SharedKey,
            result_code: fidl_ieee80211::StatusCode::RejectedSequenceTimeout,
        }
    }

    #[test]
    fn send_mlme_message() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();
        dev.mlme_control_handle()
            .send_authenticate_conf(&mut make_auth_confirm_msg())
            .expect("error sending MLME message");

        // Read message from channel.
        let msg = fake_device
            .next_mlme_msg::<fidl_mlme::AuthenticateConfirm>()
            .expect("error reading message from channel");
        assert_eq!(msg, make_auth_confirm_msg());
    }

    #[test]
    fn send_mlme_message_peer_already_closed() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();

        drop(fake_device.sme_sap.1);

        let result = dev.mlme_control_handle().send_authenticate_conf(&mut make_auth_confirm_msg());
        assert!(result.unwrap_err().is_closed());
    }

    #[test]
    fn fake_device_deliver_eth_frame() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();
        assert_eq!(fake_device.eth_queue.len(), 0);
        let first_frame = [5; 32];
        let second_frame = [6; 32];
        assert_eq!(dev.deliver_eth_frame(&first_frame[..]), Ok(()));
        assert_eq!(dev.deliver_eth_frame(&second_frame[..]), Ok(()));
        assert_eq!(fake_device.eth_queue.len(), 2);
        assert_eq!(&fake_device.eth_queue[0], &first_frame);
        assert_eq!(&fake_device.eth_queue[1], &second_frame);
    }

    #[test]
    fn get_set_channel() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();
        dev.set_channel(banjo_common::WlanChannel {
            primary: 2,
            cbw: banjo_common::ChannelBandwidth::CBW80P80,
            secondary80: 4,
        })
        .expect("set_channel failed?");
        // Check the internal state.
        assert_eq!(
            fake_device.wlan_channel,
            banjo_common::WlanChannel {
                primary: 2,
                cbw: banjo_common::ChannelBandwidth::CBW80P80,
                secondary80: 4
            }
        );
        // Check the external view of the internal state.
        assert_eq!(
            dev.channel(),
            banjo_common::WlanChannel {
                primary: 2,
                cbw: banjo_common::ChannelBandwidth::CBW80P80,
                secondary80: 4
            }
        );
    }

    #[test]
    fn set_key() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();
        dev.set_key(key::KeyConfig {
            bssid: 1,
            protection: key::Protection::NONE,
            cipher_oui: [3, 4, 5],
            cipher_type: 6,
            key_type: key::KeyType::PAIRWISE,
            peer_addr: [8; 6],
            key_idx: 9,
            key_len: 10,
            key: [11; 32],
            rsc: 12,
        })
        .expect("error setting key");
        assert_eq!(fake_device.keys.len(), 1);
    }

    #[test]
    fn start_hw_scan() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();

        let result = dev.start_hw_scan(&WlanHwScanConfig {
            scan_type: WlanHwScanType::PASSIVE,
            num_channels: 3,
            channels: arr!([1, 2, 3], WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize),
            ssid: banjo_ieee80211::CSsid {
                len: 3,
                data: arr!([65; 3], fidl_ieee80211::MAX_SSID_BYTE_LEN as usize),
            },
        });
        assert!(result.is_ok());
        assert_variant!(fake_device.hw_scan_req, Some(config) => {
            assert_eq!(config.scan_type, WlanHwScanType::PASSIVE);
            assert_eq!(config.num_channels, 3);
            assert_eq!(&config.channels[..], &arr!([1, 2, 3], WLAN_INFO_CHANNEL_LIST_MAX_CHANNELS as usize)[..]);
            assert_eq!(config.ssid, banjo_ieee80211::CSsid { len: 3, data: arr!([65; 3], fidl_ieee80211::MAX_SSID_BYTE_LEN as usize) });
        }, "expected HW scan config");
    }

    #[test]
    fn get_wlanmac_info() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();
        let info = dev.wlanmac_info();
        assert_eq!(info.sta_addr, [7u8; 6]);
        assert_eq!(info.mac_role, WlanInfoMacRole::CLIENT);
        assert_eq!(info.driver_features, WlanInfoDriverFeature(0));
        assert_eq!(info.caps, WlanInfoHardwareCapability(0));
        assert_eq!(info.bands_count, 2);
    }

    #[test]
    fn configure_bss() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();
        dev.configure_bss(BssConfig {
            bssid: [1, 2, 3, 4, 5, 6],
            bss_type: banjo_internal::BssType::PERSONAL,
            remote: true,
        })
        .expect("error configuring bss");
        assert!(fake_device.bss_cfg.is_some());
    }

    #[test]
    fn enable_disable_beaconing() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();

        let mut in_buf = fake_device.buffer_provider.get_buffer(4).expect("error getting buffer");
        in_buf.as_mut_slice().copy_from_slice(&[1, 2, 3, 4][..]);

        dev.enable_beaconing(OutBuf::from(in_buf, 4), 1, TimeUnit(2))
            .expect("error enabling beaconing");
        assert_variant!(
        fake_device.bcn_cfg.as_ref(),
        Some((buf, tim_ele_offset, beacon_interval)) => {
            assert_eq!(&buf[..], &[1, 2, 3, 4][..]);
            assert_eq!(*tim_ele_offset, 1);
            assert_eq!(*beacon_interval, TimeUnit(2));
        });
        dev.disable_beaconing().expect("error disabling beaconing");
        assert_variant!(fake_device.bcn_cfg.as_ref(), None);
    }

    #[test]
    fn configure_beacon() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();

        {
            let mut in_buf =
                fake_device.buffer_provider.get_buffer(4).expect("error getting buffer");
            in_buf.as_mut_slice().copy_from_slice(&[1, 2, 3, 4][..]);
            dev.enable_beaconing(OutBuf::from(in_buf, 4), 1, TimeUnit(2))
                .expect("error enabling beaconing");
            assert_variant!(fake_device.bcn_cfg.as_ref(), Some((buf, _, _)) => {
                assert_eq!(&buf[..], &[1, 2, 3, 4][..]);
            });
        }

        {
            let mut in_buf =
                fake_device.buffer_provider.get_buffer(4).expect("error getting buffer");
            in_buf.as_mut_slice().copy_from_slice(&[1, 2, 3, 5][..]);
            dev.configure_beacon(OutBuf::from(in_buf, 4)).expect("error enabling beaconing");
            assert_variant!(fake_device.bcn_cfg.as_ref(), Some((buf, _, _)) => {
                assert_eq!(&buf[..], &[1, 2, 3, 5][..]);
            });
        }
    }

    #[test]
    fn set_link_status() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();

        dev.set_eth_link_up().expect("failed setting status");
        assert_eq!(fake_device.link_status, LinkStatus::UP);

        dev.set_eth_link_down().expect("failed setting status");
        assert_eq!(fake_device.link_status, LinkStatus::DOWN);
    }

    #[test]
    fn configure_assoc() {
        let exec = fasync::TestExecutor::new().expect("failed to create an executor");
        let mut fake_device = FakeDevice::new(&exec);
        let dev = fake_device.as_device();
        dev.configure_assoc(WlanAssocCtx {
            bssid: [1, 2, 3, 4, 5, 6],
            aid: 1,
            listen_interval: 2,
            phy: WlanPhyType::ERP,
            channel: banjo_common::WlanChannel {
                primary: 3,
                cbw: banjo_common::ChannelBandwidth::CBW20,
                secondary80: 0,
            },
            qos: false,
            wmm_params: ddk_converter::blank_wmm_params(),

            rates_cnt: 4,
            rates: [0; WLAN_MAC_MAX_RATES as usize],
            capability_info: 0x0102,

            has_ht_cap: false,
            // Safe: This is not read by the driver.
            ht_cap: unsafe { std::mem::zeroed::<Ieee80211HtCapabilities>() },
            has_ht_op: false,
            // Safe: This is not read by the driver.
            ht_op: unsafe { std::mem::zeroed::<WlanHtOp>() },

            has_vht_cap: false,
            // Safe: This is not read by the driver.
            vht_cap: unsafe { std::mem::zeroed::<Ieee80211VhtCapabilities>() },
            has_vht_op: false,
            // Safe: This is not read by the driver.
            vht_op: unsafe { std::mem::zeroed::<WlanVhtOp>() },
        })
        .expect("error configuring assoc");
        assert!(fake_device.assocs.contains_key(&[1, 2, 3, 4, 5, 6]));
    }
}
