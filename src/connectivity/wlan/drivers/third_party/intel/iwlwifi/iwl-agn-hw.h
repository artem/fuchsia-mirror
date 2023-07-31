/******************************************************************************
 *
 * Copyright(c) 2005 - 2014 Intel Corporation. All rights reserved.
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
/*
 * Please use this file (iwl-agn-hw.h) only for hardware-related definitions.
 */

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_IWL_AGN_HW_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_IWL_AGN_HW_H_

#define IWLAGN_RTC_INST_LOWER_BOUND (0x000000)
#define IWLAGN_RTC_INST_UPPER_BOUND (0x020000)

#define IWLAGN_RTC_DATA_LOWER_BOUND (0x800000)
#define IWLAGN_RTC_DATA_UPPER_BOUND (0x80C000)

#define IWLAGN_RTC_INST_SIZE (IWLAGN_RTC_INST_UPPER_BOUND - IWLAGN_RTC_INST_LOWER_BOUND)
#define IWLAGN_RTC_DATA_SIZE (IWLAGN_RTC_DATA_UPPER_BOUND - IWLAGN_RTC_DATA_LOWER_BOUND)

#define IWL60_RTC_INST_LOWER_BOUND (0x000000)
#define IWL60_RTC_INST_UPPER_BOUND (0x040000)
#define IWL60_RTC_DATA_LOWER_BOUND (0x800000)
#define IWL60_RTC_DATA_UPPER_BOUND (0x814000)
#define IWL60_RTC_INST_SIZE (IWL60_RTC_INST_UPPER_BOUND - IWL60_RTC_INST_LOWER_BOUND)
#define IWL60_RTC_DATA_SIZE (IWL60_RTC_DATA_UPPER_BOUND - IWL60_RTC_DATA_LOWER_BOUND)

/* RSSI to dBm */
#define IWLAGN_RSSI_OFFSET 44

#define IWLAGN_DEFAULT_TX_RETRY 15
#define IWLAGN_MGMT_DFAULT_RETRY_LIMIT 3
#define IWLAGN_RTS_DFAULT_RETRY_LIMIT 60
#define IWLAGN_BAR_DFAULT_RETRY_LIMIT 60
#define IWLAGN_LOW_RETRY_LIMIT 7

/* Limit range of txpower output target to be between these values */
#define IWLAGN_TX_POWER_TARGET_POWER_MIN (0)  /* 0 dBm: 1 milliwatt */
#define IWLAGN_TX_POWER_TARGET_POWER_MAX (16) /* 16 dBm */

/* EEPROM */
#define IWLAGN_EEPROM_IMG_SIZE 2048

/* high blocks contain PAPD data */
#define OTP_HIGH_IMAGE_SIZE_6x00 (6 * 512 * sizeof(uint16_t)) /* 6 KB */
#define OTP_HIGH_IMAGE_SIZE_1000 (0x200 * sizeof(uint16_t))   /* 1024 bytes */
#define OTP_MAX_LL_ITEMS_1000 (3)                             /* OTP blocks for 1000 */
#define OTP_MAX_LL_ITEMS_6x00 (4)                             /* OTP blocks for 6x00 */
#define OTP_MAX_LL_ITEMS_6x50 (7)                             /* OTP blocks for 6x50 */
#define OTP_MAX_LL_ITEMS_2x00 (4)                             /* OTP blocks for 2x00 */

#define IWLAGN_NUM_QUEUES 20

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_IWL_AGN_HW_H_
