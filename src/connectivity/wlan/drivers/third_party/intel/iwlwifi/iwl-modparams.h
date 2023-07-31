/******************************************************************************
 *
 * Copyright(c) 2005 - 2014 Intel Corporation. All rights reserved.
 * Copyright(c) 2018 Intel Corporation
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
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_IWL_MODPARAMS_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_IWL_MODPARAMS_H_

#include <stdbool.h>
#include <stdint.h>

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/compiler.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/kernel.h"

extern struct iwl_mod_params iwlwifi_mod_params;

enum iwl_power_level {
  IWL_POWER_INDEX_1,
  IWL_POWER_INDEX_2,
  IWL_POWER_INDEX_3,
  IWL_POWER_INDEX_4,
  IWL_POWER_INDEX_5,
  IWL_POWER_NUM
};

enum iwl_disable_11n {
  IWL_DISABLE_HT_ALL = BIT(0),
  IWL_DISABLE_HT_TXAGG = BIT(1),
  IWL_DISABLE_HT_RXAGG = BIT(2),
  IWL_ENABLE_HT_TXAGG = BIT(3),
};

enum iwl_amsdu_size {
  IWL_AMSDU_DEF = 0,
  IWL_AMSDU_4K = 1,
  IWL_AMSDU_8K = 2,
  IWL_AMSDU_12K = 3,
  /* Add 2K at the end to avoid breaking current API */
  IWL_AMSDU_2K = 4,
};

enum iwl_uapsd_disable {
  IWL_DISABLE_UAPSD_BSS = BIT(0),
  IWL_DISABLE_UAPSD_P2P_CLIENT = BIT(1),
};

/**
 * struct iwl_mod_params
 *
 * Holds the module parameters
 *
 * @swcrypto: using hardware encryption, default = 0
 * @disable_11n: disable 11n capabilities, default = 0,
 *  use IWL_[DIS,EN]ABLE_HT_* constants
 * @amsdu_size: See &enum iwl_amsdu_size.
 * @fw_restart: restart firmware, default = 1
 * @bt_coex_active: enable bt coex, default = true
 * @led_mode: system default, default = 0
 * @power_save: enable power save, default = false
 * @power_level: power level, default = 1
 * @debug_level: levels are IWL_DL_*
 * @antenna_coupling: antenna coupling in dB, default = 0
 * @nvm_file: specifies a external NVM file
 * @uapsd_disable: disable U-APSD, see &enum iwl_uapsd_disable, default =
 *  IWL_DISABLE_UAPSD_BSS | IWL_DISABLE_UAPSD_P2P_CLIENT
 * @xvt_default_mode: xVT is the default operation mode, default = false
 * @d0i3_disable: disable d0i3, default = 1,
 * @d0i3_timeout: time to wait after no refs are taken before
 *  entering D0i3 (in msecs)
 * @lar_disable: disable LAR (regulatory), default = 0
 * @fw_monitor: allow to use firmware monitor
 * @disable_11ac: disable VHT capabilities, default = false.
 * @disable_msix: disable MSI-X and fall back to MSI on PCIe, default = false.
 * @remove_when_gone: remove an inaccessible device from the PCIe bus.
 * @enable_ini: enable new FW debug infratructure (INI TLVs)
 */
struct iwl_mod_params {
  int swcrypto;
  unsigned int disable_11n;
  int amsdu_size;
  bool fw_restart;
  bool bt_coex_active;
  int led_mode;
  bool power_save;
  int power_level;
#ifdef CPTCFG_IWLWIFI_DEBUG
  uint32_t debug_level;
#endif
  int antenna_coupling;
#if IS_ENABLED(CPTCFG_IWLXVT)
  bool xvt_default_mode;
#endif
#if IS_ENABLED(CPTCFG_IWLTEST)
  bool trans_test;
#endif
  char* nvm_file;
  uint32_t uapsd_disable;
  bool d0i3_disable;
  unsigned int d0i3_timeout;
  bool lar_disable;
  bool fw_monitor;
  bool disable_11ac;
  /**
   * @disable_11ax: disable HE capabilities, default = false
   */
  bool disable_11ax;
  bool disable_msix;
  bool remove_when_gone;
  bool enable_ini;
};

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_IWL_MODPARAMS_H_
