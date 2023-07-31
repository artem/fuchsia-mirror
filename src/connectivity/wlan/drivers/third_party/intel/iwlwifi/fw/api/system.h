/* Use of this code is governed by a BSD-style license that can be found in the LICENSE file. */
/*
 * Copyright (C) 2012-2014, 2019-2021 Intel Corporation
 * Copyright (C) 2013-2015 Intel Mobile Communications GmbH
 * Copyright (C) 2016-2017 Intel Deutschland GmbH
 */
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_FW_API_SYSTEM_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_FW_API_SYSTEM_H_

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/compiler.h"

#define SOC_CONFIG_CMD_FLAGS_DISCRETE		BIT(0)
#define SOC_CONFIG_CMD_FLAGS_LOW_LATENCY	BIT(1)

#define SOC_FLAGS_LTR_APPLY_DELAY_MASK		0xc
#define SOC_FLAGS_LTR_APPLY_DELAY_NONE		0
#define SOC_FLAGS_LTR_APPLY_DELAY_200		1
#define SOC_FLAGS_LTR_APPLY_DELAY_2500		2
#define SOC_FLAGS_LTR_APPLY_DELAY_1820		3

/**
 * struct iwl_soc_configuration_cmd - Set device stabilization latency
 *
 * @flags: soc settings flags.  In VER_1, we can only set the DISCRETE
 *	flag, because the FW treats the whole value as an integer. In
 *	VER_2, we can set the bits independently.
 * @latency: time for SOC to ensure stable power & XTAL
 */
struct iwl_soc_configuration_cmd {
	__le32 flags;
	__le32 latency;
} __packed; /*
	     * SOC_CONFIGURATION_CMD_S_VER_1 (see description above)
	     * SOC_CONFIGURATION_CMD_S_VER_2
	     */

/**
 * struct iwl_system_features_control_cmd - system features control command
 * @features: bitmap of features to disable
 */
struct iwl_system_features_control_cmd {
	__le32 features[4];
} __packed; /* SYSTEM_FEATURES_CONTROL_CMD_API_S_VER_1 */

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_FW_API_SYSTEM_H_
