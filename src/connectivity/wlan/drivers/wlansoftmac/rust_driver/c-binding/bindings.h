// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_RUST_DRIVER_C_BINDING_BINDINGS_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_RUST_DRIVER_C_BINDING_BINDINGS_H_

// Warning:
// This file was autogenerated by cbindgen.
// Do not modify this file manually.

#include <fuchsia/wlan/softmac/c/banjo.h>
#include <lib/trace/event.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <zircon/types.h>

typedef struct wlansoftmac_handle_t wlansoftmac_handle_t;

typedef struct {
  void (*recv)(const void *ctx, const wlan_rx_packet_t *packet, trace_async_id_t async_id);
} rust_wlan_softmac_ifc_protocol_ops_copy_t;

/**
 * Type containing pointers to the static `PROTOCOL_OPS` and a `DriverEventSink`.
 *
 * The wlansoftmac driver copies the pointers from `CWlanSoftmacIfcProtocol` which means the code
 * constructing this type must ensure those pointers remain valid for their lifetime in
 * wlansoftmac.
 *
 * Additionally, by assigning functions directly from `PROTOCOL_OPS` to the corresponding Banjo
 * generated types in wlansoftmac, wlansoftmac ensure the function signatures are correct at
 * compile-time.
 */
typedef struct {
  const rust_wlan_softmac_ifc_protocol_ops_copy_t *ops;
  const void *ctx;
} rust_wlan_softmac_ifc_protocol_copy_t;

/**
 * Type that represents the FFI from the bridged wlansoftmac to wlansoftmac itself.
 *
 * Each of the functions in this FFI are safe to call from any thread but not
 * safe to call concurrently, i.e., they can only be called one at a time.
 *
 * # Safety
 *
 * Rust does not support marking a type as unsafe, but initializing this type is
 * definitely unsafe and deserves documentation. This is because when the bridged
 * wlansoftmac uses this FFI, it cannot guarantee each of the functions is safe to
 * call from any thread.
 *
 * By constructing a value of this type, the constructor promises each of the functions
 * is safe to call from any thread. And no undefined behavior will occur if the
 * caller only calls one of them at a time.
 */
typedef struct {
  const void *device;
  /**
   * Start operations on the underlying device and return the SME channel.
   */
  int32_t (*start)(const void *device, const rust_wlan_softmac_ifc_protocol_copy_t *ifc,
                   zx_handle_t wlan_softmac_ifc_bridge_client_handle, zx_handle_t *out_sme_channel);
  /**
   * Request to deliver an Ethernet II frame to Fuchsia's Netstack.
   */
  int32_t (*deliver_eth_frame)(const void *device, const uint8_t *data, uintptr_t len);
  /**
   * Deliver a WLAN frame directly through the firmware.
   *
   * The `buffer` and `written` arguments must be from a call to `FinalizedBuffer::release`. The
   * C++ portion of wlansoftmac will reconstruct an instance of the `FinalizedBuffer` class
   * defined in buffer_allocator.h.
   */
  int32_t (*queue_tx)(const void *device, uint32_t options, void *buffer, uintptr_t written,
                      wlan_tx_info_t tx_info, trace_async_id_t async_id);
  /**
   * Reports the current status to the ethernet driver.
   */
  int32_t (*set_ethernet_status)(const void *device, uint32_t status);
} rust_device_interface_t;

/**
 * Type that wraps a pointer to a buffer allocated in the C++ portion of wlansoftmac.
 */
typedef struct {
  /**
   * Returns the buffer's ownership and free it.
   *
   * # Safety
   *
   * The `free` function is unsafe because the function cannot guarantee the pointer it's
   * called with is the `raw` field in this struct.
   *
   * By calling `free`, the caller promises the pointer it's called with is the `raw` field
   * in this struct.
   */
  void (*free)(void *raw);
  /**
   * Pointer to the buffer allocated in the C++ portion of wlansoftmac and owned by the Rust
   * portion of wlansoftmac.
   */
  void *raw;
  /**
   * Pointer to the start of bytes written in the buffer.
   */
  uint8_t *data;
  /**
   * Capacity of the buffer, starting at `data`.
   */
  uintptr_t capacity;
} wlansoftmac_buffer_t;

typedef struct {
  /**
   * Allocate and take ownership of a buffer allocated by the C++ portion of wlansoftmac
   * with at least `min_capacity` bytes of capacity.
   *
   * The returned `CBuffer` contains a pointer whose pointee the caller now owns, unless that
   * pointer is the null pointer. If the pointer is non-null, then the `Drop` implementation of
   * `CBuffer` ensures its `free` will be called if its dropped. If the pointer is null, the
   * allocation failed, and the caller must discard the `CBuffer` without calling its `Drop`
   * implementation.
   *
   * # Safety
   *
   * This function is unsafe because the returned `CBuffer` could contain null pointers,
   * indicating the allocation failed.
   *
   * By calling this function, the caller promises to call `mem::forget` on the returned
   * `CBuffer` if either the `raw` or `data` fields in the `CBuffer` are null.
   */
  wlansoftmac_buffer_t (*get_buffer)(uintptr_t min_capacity);
} wlansoftmac_buffer_provider_ops_t;

/**
 * A convenient C-wrapper for read-only memory that is neither owned or managed by Rust
 */
typedef struct {
  const uint8_t *data;
  uintptr_t size;
} wlan_span_t;

/**
 * Start and run a bridged wlansoftmac driver hosting an MLME server and an SME server.
 *
 * The driver is "bridged" in the sense that it requires a bridge to a Fuchsia driver to
 * communicate with other Fuchsia drivers over the FDF transport. When initialization of the
 * bridged driver completes, `run_init_completer` will be called.
 *
 * # Safety
 *
 * There are two layers of safety documentation for this function. The first layer is for this
 * function itself, and the second is for the `run_init_completer` function.
 *
 * ## For this function itself
 *
 * This function is unsafe for the following reasons:
 *
 *   - This function cannot guarantee `run_init_completer` is thread-safe, i.e., that it's safe to
 *     to call at any time from any thread.
 *   - This function cannot guarantee `init_completer` points to a valid object when
 *     `run_init_completer` is called.
 *   - This function cannot guarantee `wlan_softmac_bridge_client_handle` is a valid handle.
 *
 * By calling this function, the caller promises the following:
 *
 *   - The `run_init_completer` function is thread-safe.
 *   - The `init_completer` pointer will point to a valid object at least until
 *     `run_init_completer` is called.
 *   - The `wlan_softmac_bridge_client_handle` is a valid handle.
 *
 * ## For `run_init_completer`
 *
 * The `run_init_completer` function is unsafe because it cannot guarantee the `init_completer`
 * argument will be the same `init_completer` passed to `start_and_run_bridged_wlansoftmac`, and
 * cannot guarantee it will be called exactly once.
 *
 * The caller of `run_init_completer` must promise to pass the same `init_completer` from
 * `start_and_run_bridged_wlansoftmac` to `run_init_completer` and call `run_init_completer`
 * exactly once.
 */
extern "C" zx_status_t start_and_run_bridged_wlansoftmac(
    void *init_completer,
    void (*run_init_completer)(void *init_completer, zx_status_t status,
                               wlansoftmac_handle_t *wlan_softmac_handle),
    rust_device_interface_t device, wlansoftmac_buffer_provider_ops_t buffer_provider,
    zx_handle_t wlan_softmac_bridge_client_handle);

/**
 * Stop the bridged wlansoftmac driver associated with `softmac`.
 *
 * This function takes ownership of the `WlanSoftmacHandle` that `softmac` points to and destroys
 * it. When the bridged driver stops, `run_stop_completer` will be called.
 *
 * # Safety
 *
 * There are two layers of safety documentation for this function. The first layer is for this
 * function itself, and the second is for the `run_stop_completer` function.
 *
 * ## For this function itself
 *
 * This function is unsafe for the following reasons:
 *
 *   - This function cannot guarantee `run_stop_completer` is thread-safe, i.e., that it's safe to
 *     to call at any time from any thread.
 *   - This function cannot guarantee `stop_completer` points to a valid object when
 *     `run_stop_completer` is called.
 *   - This function cannot guarantee `softmac` is a valid pointer, and the only pointer, to a
 *     `WlanSoftmacHandle`.
 *
 * By calling this function, the caller promises the following:
 *
 *   - The `run_stop_completer` function is thread-safe.
 *   - The `stop_completer` pointer will point to a valid object at least until
 *     `run_stop_completer` is called.
 *   - The `softmac` pointer is the same pointer received from `run_init_completer` (called as
 *     a consequence of the startup initiated by calling `start_and_run_bridged_wlansoftmac`.
 *
 * ## For `run_stop_completer`
 *
 * The `run_stop_completer` function is unsafe because it cannot guarantee the `stop_completer`
 * argument will be the same `stop_completer` passed to `stop_bridged_wlansoftmac`, and cannot
 * guarantee it will be called exactly once.
 *
 * The caller of `run_stop_completer` must promise to pass the same `stop_completer` from
 * `stop_bridged_wlansoftmac` to `run_stop_completer` and call `run_stop_completer` exactly once.
 */
extern "C" void stop_bridged_wlansoftmac(void *stop_completer,
                                         void (*run_stop_completer)(void *stop_completer),
                                         wlansoftmac_handle_t *softmac);

/**
 * FFI interface: Queue an ethernet frame to be sent over the air. The caller should either end the
 * async trace event corresponding to |async_id| if an error occurs or deferred ending the trace to
 * a later call into the C++ portion of wlansoftmac.
 *
 * Assuming no errors occur, the Rust portion of wlansoftmac will eventually
 * rust_device_interface_t.queue_tx() with the same |async_id|. At that point, the C++ portion of
 * wlansoftmac will assume responsibility for ending the async trace event.
 */
extern "C" zx_status_t sta_queue_eth_frame_tx(wlansoftmac_handle_t *softmac, wlan_span_t frame,
                                              trace_async_id_t async_id);

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_RUST_DRIVER_C_BINDING_BINDINGS_H_
