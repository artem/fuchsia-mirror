// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_LIB_MLME_FULLMAC_C_BINDING_BINDINGS_H_
#define SRC_CONNECTIVITY_WLAN_LIB_MLME_FULLMAC_C_BINDING_BINDINGS_H_

// Warning:
// This file was autogenerated by cbindgen.
// Do not modify this file manually.

#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct wlan_fullmac_mlme_handle_t wlan_fullmac_mlme_handle_t;

typedef struct {
  void (*on_scan_result)(void *ctx, const wlan_fullmac_scan_result_t *result);
  void (*on_scan_end)(void *ctx, const wlan_fullmac_scan_end_t *end);
  void (*connect_conf)(void *ctx, const wlan_fullmac_connect_confirm_t *resp);
  void (*roam_conf)(void *ctx, const wlan_fullmac_roam_confirm_t *resp);
  void (*auth_ind)(void *ctx, const wlan_fullmac_auth_ind_t *ind);
  void (*deauth_conf)(void *ctx, const wlan_fullmac_deauth_confirm_t *resp);
  void (*deauth_ind)(void *ctx, const wlan_fullmac_deauth_indication_t *ind);
  void (*assoc_ind)(void *ctx, const wlan_fullmac_assoc_ind_t *ind);
  void (*disassoc_conf)(void *ctx, const wlan_fullmac_disassoc_confirm_t *resp);
  void (*disassoc_ind)(void *ctx, const wlan_fullmac_disassoc_indication_t *ind);
  void (*start_conf)(void *ctx, const wlan_fullmac_start_confirm_t *resp);
  void (*stop_conf)(void *ctx, const wlan_fullmac_stop_confirm_t *resp);
  void (*eapol_conf)(void *ctx, const wlan_fullmac_eapol_confirm_t *resp);
  void (*on_channel_switch)(void *ctx, const wlan_fullmac_channel_switch_info_t *resp);
  void (*signal_report)(void *ctx, const wlan_fullmac_signal_report_indication_t *ind);
  void (*eapol_ind)(void *ctx, const wlan_fullmac_eapol_indication_t *ind);
  void (*on_pmk_available)(void *ctx, const wlan_fullmac_pmk_info_t *info);
  void (*sae_handshake_ind)(void *ctx, const wlan_fullmac_sae_handshake_ind_t *ind);
  void (*sae_frame_rx)(void *ctx, const wlan_fullmac_sae_frame_t *frame);
  void (*on_wmm_status_resp)(void *ctx, zx_status_t status,
                             const wlan_wmm_parameters_t *wmm_params);
} rust_wlan_fullmac_ifc_protocol_ops_copy_t;

/**
 * Hand-rolled Rust version of the banjo wlan_fullmac_ifc_protocol for communication from the
 * driver up.
 * Note that we copy the individual fns out of this struct into the equivalent generated struct
 * in C++. Thanks to cbindgen, this gives us a compile-time confirmation that our function
 * signatures are correct.
 */
typedef struct {
  const rust_wlan_fullmac_ifc_protocol_ops_copy_t *ops;
  void *ctx;
} rust_wlan_fullmac_ifc_protocol_copy_t;

/**
 * A `FullmacDeviceInterface` allows transmitting frames and MLME messages.
 */
typedef struct {
  void *device;
  /**
   * Start operations on the underlying device and return the SME channel.
   */
  zx_status_t (*start)(void *device, const rust_wlan_fullmac_ifc_protocol_copy_t *ifc,
                       zx_handle_t *out_sme_channel);
  wlan_fullmac_query_info_t (*query_device_info)(void *device);
  mac_sublayer_support_t (*query_mac_sublayer_support)(void *device);
  security_support_t (*query_security_support)(void *device);
  spectrum_management_support_t (*query_spectrum_management_support)(void *device);
  void (*start_scan)(void *device, wlan_fullmac_impl_start_scan_request_t *req);
  void (*connect)(void *device, wlan_fullmac_impl_connect_request_t *req);
  void (*reconnect_req)(void *device, wlan_fullmac_reconnect_req_t *req);
  void (*auth_resp)(void *device, wlan_fullmac_auth_resp_t *resp);
  void (*deauth_req)(void *device, wlan_fullmac_deauth_req_t *req);
  void (*assoc_resp)(void *device, wlan_fullmac_assoc_resp_t *resp);
  void (*disassoc_req)(void *device, wlan_fullmac_disassoc_req_t *req);
  void (*reset_req)(void *device, wlan_fullmac_reset_req_t *req);
  void (*start_req)(void *device, wlan_fullmac_start_req_t *req);
  void (*stop_req)(void *device, wlan_fullmac_stop_req_t *req);
  wlan_fullmac_set_keys_resp_t (*set_keys_req)(void *device, wlan_fullmac_set_keys_req_t *req);
  void (*del_keys_req)(void *device, wlan_fullmac_del_keys_req_t *req);
  void (*eapol_req)(void *device, wlan_fullmac_eapol_req_t *req);
  wlan_fullmac_iface_counter_stats_t (*get_iface_counter_stats)(void *device, int32_t *out_status);
  wlan_fullmac_iface_histogram_stats_t (*get_iface_histogram_stats)(void *device,
                                                                    int32_t *out_status);
  void (*sae_handshake_resp)(void *device, wlan_fullmac_sae_handshake_resp_t *resp);
  void (*sae_frame_tx)(void *device, wlan_fullmac_sae_frame_t *frame);
  void (*wmm_status_req)(void *device);
  void (*on_link_state_changed)(void *device, bool online);
} rust_fullmac_device_interface_t;

extern "C" wlan_fullmac_mlme_handle_t *start_fullmac_mlme(rust_fullmac_device_interface_t device);

extern "C" void stop_fullmac_mlme(wlan_fullmac_mlme_handle_t *mlme);

/**
 * FFI interface: Stop and delete a FullMAC MLME via the FullmacMlmeHandle. Takes ownership
 * and invalidates the passed FullmacMlmeHandle.
 *
 * # Safety
 *
 * This fn accepts a raw pointer that is held by the FFI caller as a handle to
 * the MLME. This API is fundamentally unsafe, and relies on the caller to
 * pass the correct pointer and make no further calls on it later.
 */
extern "C" void delete_fullmac_mlme(wlan_fullmac_mlme_handle_t *mlme);

#endif  // SRC_CONNECTIVITY_WLAN_LIB_MLME_FULLMAC_C_BINDING_BINDINGS_H_
