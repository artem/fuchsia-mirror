/******************************************************************************
 *
 * Copyright(c) 2015 Intel Deutschland GmbH
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 *
 *  * Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 *  * Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in
 *    the documentation and/or other materials provided with the
 *    distribution.
 *  * Neither the name Intel Corporation nor the names of its
 *    contributors may be used to endorse or promote products derived
 *    from this software without specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
 * "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
 * LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
 * A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
 * OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
 * SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
 * LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 *
 *****************************************************************************/
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_MVM_TOF_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_MVM_TOF_H_

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/fw/api/tof.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/kernel.h"

struct iwl_mvm_tof_data {
  struct iwl_tof_config_cmd tof_cfg;
  struct iwl_tof_range_req_cmd range_req;
  struct iwl_tof_range_req_ext_cmd range_req_ext;
#ifdef CPTCFG_IWLWIFI_DEBUGFS
  struct iwl_tof_responder_config_cmd responder_cfg;
#endif
  struct iwl_tof_range_rsp_ntfy range_resp;
  uint8_t last_abort_id;
  uint16_t active_range_request;
};

void iwl_mvm_tof_init(struct iwl_mvm* mvm);
void iwl_mvm_tof_clean(struct iwl_mvm* mvm);
int iwl_mvm_tof_config_cmd(struct iwl_mvm* mvm);
int iwl_mvm_tof_range_abort_cmd(struct iwl_mvm* mvm, uint8_t id);
int iwl_mvm_tof_range_request_cmd(struct iwl_mvm* mvm, struct ieee80211_vif* vif);
void iwl_mvm_tof_resp_handler(struct iwl_mvm* mvm, struct iwl_rx_cmd_buffer* rxb);
int iwl_mvm_tof_range_request_ext_cmd(struct iwl_mvm* mvm, struct ieee80211_vif* vif);
#ifdef CPTCFG_IWLWIFI_DEBUGFS
int iwl_mvm_tof_responder_cmd(struct iwl_mvm* mvm, struct ieee80211_vif* vif);
#endif
#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_MVM_TOF_H_
