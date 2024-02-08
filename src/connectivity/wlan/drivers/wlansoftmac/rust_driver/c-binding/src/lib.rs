// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! C bindings for wlansoftmac-rust crate.

// Explicitly declare usage for cbindgen.

use {
    diagnostics_log::PublishOptions,
    fidl_fuchsia_wlan_softmac as fidl_softmac,
    fuchsia_async::LocalExecutor,
    fuchsia_trace as trace, fuchsia_zircon as zx,
    std::{ffi::c_void, sync::Once},
    trace::Id as TraceId,
    tracing::error,
    wlan_mlme::{
        buffer::BufferProvider,
        device::{completers::StopCompleter, Device, DeviceInterface},
    },
    wlan_span::CSpan,
    wlan_trace as wtrace,
    wlansoftmac_rust::WlanSoftmacHandle,
};

static LOGGER_ONCE: Once = Once::new();

/// Cast a *mut c_void to a usize. This is normally done to workaround the Send trait which *mut c_void does
/// not implement
///
/// # Safety
///
/// The caller must only cast the usize back into a *mut c_void in the same address space.  Otherwise, the
/// *mut c_void may not be valid.
unsafe fn ptr_as_usize(ptr: *mut c_void) -> usize {
    ptr as usize
}

/// Cast a usize to a *mut c_void. This is normally done to workaround the Send trait which *mut c_void does
/// not implement
///
/// # Safety
///
/// The caller must only cast a usize into a *mut c_void if its known the *mut c_void will be used in the
/// same address space where it originated.  Otherwise, the *mut c_void may not be valid.
unsafe fn usize_as_ptr(u: usize) -> *mut c_void {
    u as *mut c_void
}

/// Start and run a bridged wlansoftmac driver hosting an MLME server and an SME server. The driver is
/// "bridged" in sense that it requires a bridge to a Fuchsia driver to communicate with other Fuchsia
/// drivers over the FDF transport. When initialization of the bridged driver completes, run_init_completer
/// will be called.
///
/// # Safety
///
/// The caller of this function should provide raw pointers that will be valid in the address space
/// where the Rust portion of wlansoftmac will run.
#[no_mangle]
pub unsafe extern "C" fn start_and_run_bridged_wlansoftmac(
    init_completer: *mut c_void,
    run_init_completer: extern "C" fn(
        init_completer: *mut c_void,
        status: zx::zx_status_t,
        wlan_softmac_handle: *mut WlanSoftmacHandle,
    ),
    device: DeviceInterface,
    buf_provider: BufferProvider,
    wlan_softmac_bridge_client_handle: zx::sys::zx_handle_t,
) -> zx::sys::zx_status_t {
    // The Fuchsia syslog must not be initialized from Rust more than once per process. In the case of MLME,
    // that means we can only call it once for both the client and ap modules. Ensure this by using a shared
    // `Once::call_once()`.
    LOGGER_ONCE.call_once(|| {
        // Initialize logging with a tag that can be used to filter for forwarding to console
        diagnostics_log::initialize_sync(
            PublishOptions::default()
                .tags(&["wlan"])
                .enable_metatag(diagnostics_log::Metatag::Target),
        );
    });

    let wlan_softmac_bridge_proxy = {
        let handle = unsafe { fidl::Handle::from_raw(wlan_softmac_bridge_client_handle) };
        let channel = fidl::Channel::from(handle);
        fidl_softmac::WlanSoftmacBridgeSynchronousProxy::new(channel)
    };
    let device = Device::new(device, wlan_softmac_bridge_proxy);

    // SAFETY: Cast *mut c_void to usize so the constructed lambda will be Send. This is safe since
    // InitCompleter will not move to a thread in a different address space.
    let mut executor = LocalExecutor::new();
    executor.run_singlethreaded(async move {
        let init_completer = unsafe { ptr_as_usize(init_completer) };
        zx::Status::from(
            wlansoftmac_rust::start_and_serve(
                move |result: Result<WlanSoftmacHandle, zx::Status>| match result {
                    Ok(handle) => {
                        run_init_completer(
                            unsafe { usize_as_ptr(init_completer) },
                            zx::Status::OK.into_raw(),
                            Box::into_raw(Box::new(handle)),
                        );
                    }
                    Err(status) => {
                        run_init_completer(
                            unsafe { usize_as_ptr(init_completer) },
                            status.into_raw(),
                            std::ptr::null_mut(),
                        );
                    }
                },
                device,
                buf_provider,
            )
            .await,
        )
        .into_raw()
    })
}

/// FFI interface: Stop a WlanSoftmac via the WlanSoftmacHandle. Takes ownership and invalidates
/// the passed WlanSoftmacHandle.
///
/// # Safety
///
/// This function casts a raw pointer to a WlanSoftmacHandle. This API is fundamentally
/// unsafe, and relies on the caller passing ownership of the correct pointer.
#[no_mangle]
pub unsafe extern "C" fn stop_bridged_wlansoftmac(
    completer: *mut c_void,
    run_completer: extern "C" fn(completer: *mut c_void),
    softmac: *mut WlanSoftmacHandle,
) {
    if softmac.is_null() {
        error!("Call to stop_bridged_wlansoftmac() with NULL pointer!");
        return;
    }
    let softmac = Box::from_raw(softmac);
    // SAFETY: Cast *mut c_void to usize so the constructed lambda will be Send. This is safe since
    // StopCompleter will not move to a thread in a different address space.
    let completer = unsafe { ptr_as_usize(completer) };
    softmac.stop(StopCompleter::new(Box::new(move || {
        run_completer(unsafe { usize_as_ptr(completer) })
    })));
}

/// FFI interface: Queue an ethernet frame to be sent over the air. The caller should either end the async
/// trace event corresponding to |async_id| if an error occurs or deferred ending the trace to a later call
/// into the C++ portion of wlansoftmac.
///
/// Assuming no errors occur, the Rust portion of wlansoftmac will eventually
/// rust_device_interface_t.queue_tx() with the same |async_id|. At that point, the C++ portion of
/// wlansoftmac will assume responsibility for ending the async trace event.
#[no_mangle]
pub extern "C" fn sta_queue_eth_frame_tx(
    softmac: &mut WlanSoftmacHandle,
    frame: CSpan<'_>,
    async_id: TraceId,
) -> zx::zx_status_t {
    zx::Status::from(softmac.queue_eth_frame_tx(frame.into(), async_id).map_err(|s| {
        wtrace::async_end_wlansoftmac_tx(async_id, s);
        s
    }))
    .into_raw()
}
