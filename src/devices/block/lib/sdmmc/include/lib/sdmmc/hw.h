// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_LIB_SDMMC_INCLUDE_LIB_SDMMC_HW_H_
#define SRC_DEVICES_BLOCK_LIB_SDMMC_INCLUDE_LIB_SDMMC_HW_H_

#include <stdint.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

//
// Common SD/MMC defines
//
#define SDHC_BLOCK_SIZE 512

#define SDMMC_RESP_LEN_EMPTY (1 << 0)
#define SDMMC_RESP_LEN_136 (1 << 1)
#define SDMMC_RESP_LEN_48 (1 << 2)
#define SDMMC_RESP_LEN_48B (1 << 3)
#define SDMMC_RESP_CRC_CHECK (1 << 4)
#define SDMMC_RESP_CMD_IDX_CHECK (1 << 5)
#define SDMMC_RESP_DATA_PRESENT (1 << 6)

#define SDMMC_CMD_TYPE_NORMAL (1 << 7)
#define SDMMC_CMD_TYPE_SUSPEND (1 << 8)
#define SDMMC_CMD_TYPE_RESUME (1 << 9)
#define SDMMC_CMD_TYPE_ABORT (1 << 10)

#define SDMMC_CMD_DMA_EN (1 << 11)
#define SDMMC_CMD_BLKCNT_EN (1 << 12)
#define SDMMC_CMD_AUTO12 (1 << 13)
#define SDMMC_CMD_AUTO23 (1 << 14)
#define SDMMC_CMD_READ (1 << 15)
#define SDMMC_CMD_MULTI_BLK (1 << 16)

#define SDMMC_RESP_NONE (0x0)
#define SDMMC_RESP_R1 (SDMMC_RESP_LEN_48 | SDMMC_RESP_CMD_IDX_CHECK | SDMMC_RESP_CRC_CHECK)
#define SDMMC_RESP_R1b (SDMMC_RESP_LEN_48B | SDMMC_RESP_CMD_IDX_CHECK | SDMMC_RESP_CRC_CHECK)
#define SDMMC_RESP_R2 (SDMMC_RESP_LEN_136 | SDMMC_RESP_CRC_CHECK)
#define SDMMC_RESP_R3 (SDMMC_RESP_LEN_48)
#define SDMMC_RESP_R4 (SDMMC_RESP_LEN_48)
#define SDMMC_RESP_R5 (SDMMC_RESP_LEN_48 | SDMMC_RESP_CMD_IDX_CHECK | SDMMC_RESP_CRC_CHECK)
#define SDMMC_RESP_R5b (SDMMC_RESP_LEN_48B | SDMMC_RESP_CMD_IDX_CHECK | SDMMC_RESP_CRC_CHECK)
#define SDMMC_RESP_R6 (SDMMC_RESP_LEN_48 | SDMMC_RESP_CMD_IDX_CHECK | SDMMC_RESP_CRC_CHECK)
#define SDMMC_RESP_R7 (SDMMC_RESP_LEN_48 | SDMMC_RESP_CMD_IDX_CHECK | SDMMC_RESP_CRC_CHECK)

// Common SD/MMC commands
#define SDMMC_GO_IDLE_STATE_FLAGS SDMMC_RESP_NONE
#define SDMMC_ALL_SEND_CID_FLAGS SDMMC_RESP_R2
#define SDMMC_SEND_CSD_FLAGS SDMMC_RESP_R2
#define SDMMC_STOP_TRANSMISSION_FLAGS SDMMC_RESP_R1b | SDMMC_CMD_TYPE_ABORT
#define SDMMC_SEND_STATUS_FLAGS SDMMC_RESP_R1
#define SDMMC_READ_BLOCK_FLAGS SDMMC_RESP_R1 | SDMMC_RESP_DATA_PRESENT | SDMMC_CMD_READ
#define SDMMC_READ_MULTIPLE_BLOCK_FLAGS                                            \
  SDMMC_RESP_R1 | SDMMC_RESP_DATA_PRESENT | SDMMC_CMD_READ | SDMMC_CMD_MULTI_BLK | \
      SDMMC_CMD_BLKCNT_EN
#define SDMMC_SET_BLOCK_COUNT_FLAGS SDMMC_RESP_R1
#define SDMMC_WRITE_BLOCK_FLAGS SDMMC_RESP_R1 | SDMMC_RESP_DATA_PRESENT
#define SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS \
  SDMMC_RESP_R1 | SDMMC_RESP_DATA_PRESENT | SDMMC_CMD_MULTI_BLK | SDMMC_CMD_BLKCNT_EN
#define SDMMC_ERASE_FLAGS SDMMC_RESP_R1b
#define SDMMC_LOCK_UNLOCK_FLAGS SDMMC_RESP_R1
#define SDMMC_APP_CMD_FLAGS SDMMC_RESP_R1
#define SDMMC_GEN_CMD_FLAGS SDMMC_RESP_R1 | SD_CMD_ISDATA

// SD Commands
#define SD_SEND_RELATIVE_ADDR_FLAGS SDMMC_RESP_R6
#define SD_SWITCH_FUNC_FLAGS SDMMC_RESP_R1
#define SD_SELECT_CARD_FLAGS SDMMC_RESP_R1b
#define SD_SEND_IF_COND_FLAGS SDMMC_RESP_R7
#define SD_VOLTAGE_SWITCH_FLAGS SDMMC_RESP_R1
#define SD_APP_SEND_SCR_FLAGS SDMMC_RESP_R1 | SDMMC_RESP_DATA_PRESENT | SDMMC_CMD_READ
#define SD_APP_SET_BUS_WIDTH_FLAGS SDMMC_RESP_R1
#define SD_APP_SEND_OP_COND_FLAGS SDMMC_RESP_R3

// MMC Commands
#define MMC_SEND_OP_COND_FLAGS SDMMC_RESP_R3
#define MMC_SET_RELATIVE_ADDR_FLAGS SDMMC_RESP_R1
#define MMC_SLEEP_AWAKE_FLAGS SDMMC_RESP_R1b
#define MMC_SWITCH_FLAGS SDMMC_RESP_R1b
#define MMC_SELECT_CARD_FLAGS SDMMC_RESP_R1
#define MMC_SEND_EXT_CSD_FLAGS SDMMC_RESP_R1 | SDMMC_RESP_DATA_PRESENT | SDMMC_CMD_READ
#define MMC_SEND_TUNING_BLOCK_FLAGS SDMMC_RESP_R1 | SDMMC_RESP_DATA_PRESENT | SDMMC_CMD_READ
#define MMC_ERASE_GROUP_START_FLAGS SDMMC_RESP_R1
#define MMC_ERASE_GROUP_END_FLAGS SDMMC_RESP_R1
#define MMC_ERASE_DISCARD_ARG 0x00000003
#define MMC_ERASE_TRIM_ARG 0x00000001
#define MMC_SET_BLOCK_COUNT_PACKED (1u << 30)
#define MMC_SET_BLOCK_COUNT_RELIABLE_WRITE (1u << 31)

// Common SD/MMC commands
#define SDMMC_GO_IDLE_STATE 0
#define SDMMC_ALL_SEND_CID 2
#define SDMMC_SEND_CSD 9
#define SDMMC_STOP_TRANSMISSION 12
#define SDMMC_SEND_STATUS 13
#define SDMMC_READ_BLOCK 17
#define SDMMC_READ_MULTIPLE_BLOCK 18
#define SDMMC_SET_BLOCK_COUNT 23
#define SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS 0xffff
#define SDMMC_WRITE_BLOCK 24
#define SDMMC_WRITE_MULTIPLE_BLOCK 25
#define SDMMC_ERASE 38
#define SDMMC_LOCK_UNLOCK 42
#define SDMMC_APP_CMD 55
#define SDMMC_GEN_CMD 56

// SD Commands
#define SD_SEND_RELATIVE_ADDR 3
#define SD_SWITCH_FUNC 6
#define SD_SELECT_CARD 7
#define SD_SEND_IF_COND 8
#define SD_VOLTAGE_SWITCH 11
#define SD_APP_SEND_SCR 51
#define SD_SEND_TUNING_BLOCK 19

#define SD_APP_SET_BUS_WIDTH 6
#define SD_APP_SEND_OP_COND 41

// MMC Commands
#define MMC_SEND_OP_COND 1
#define MMC_SET_RELATIVE_ADDR 3
#define MMC_SLEEP_AWAKE 5
#define MMC_SWITCH 6
#define MMC_SELECT_CARD 7
#define MMC_SEND_EXT_CSD 8
#define MMC_SEND_TUNING_BLOCK 21
#define MMC_ERASE_GROUP_START 35
#define MMC_ERASE_GROUP_END 36

// CID fields (SD/MMC)
#define SDMMC_CID_SIZE 16

#define MMC_CID_SPEC_VRSN_40 3
#define MMC_CID_PRODUCT_NAME_START 7
#define MMC_CID_REVISION 6
#define MMC_CID_SERIAL 2

// CSD fields (SD/MMC)
#define SDMMC_CSD_SIZE 16

#define MMC_CSD_SPEC_VERSION 15
#define MMC_CSD_SIZE_START 7

// OCR fields (MMC)
#define MMC_OCR_BUSY (1 << 31)
#define MMC_OCR_ACCESS_MODE_MASK (0b11 << 29)
#define MMC_OCR_SECTOR_MODE (0b10 << 29)

// EXT_CSD fields (MMC)
#define MMC_EXT_CSD_SIZE 512

#define MMC_EXT_CSD_FLUSH_CACHE 32
#define MMC_EXT_CSD_FLUSH_MASK 0x01
#define MMC_EXT_CSD_CACHE_CTRL 33
#define MMC_EXT_CSD_CACHE_EN_MASK 0x01

#define MMC_EXT_CSD_RPMB_SIZE_MULT 168
#define MMC_EXT_CSD_PARTITION_CONFIG 179
#define MMC_EXT_CSD_PARTITION_ACCESS_MASK 0xf8
#define MMC_EXT_CSD_BOOT_PARTITION_ENABLE_MASK 0x38

#define MMC_EXT_CSD_BUS_WIDTH 183
#define MMC_EXT_CSD_BUS_WIDTH_8_DDR 6
#define MMC_EXT_CSD_BUS_WIDTH_4_DDR 5
#define MMC_EXT_CSD_BUS_WIDTH_8 2
#define MMC_EXT_CSD_BUS_WIDTH_4 1
#define MMC_EXT_CSD_BUS_WIDTH_1 0

#define MMC_EXT_CSD_HS_TIMING 185
#define MMC_EXT_CSD_HS_TIMING_LEGACY 0
#define MMC_EXT_CSD_HS_TIMING_HS 1
#define MMC_EXT_CSD_HS_TIMING_HS200 2
#define MMC_EXT_CSD_HS_TIMING_HS400 3

#define MMC_EXT_CSD_EXT_CSD_REV 192
#define MMC_EXT_CSD_EXT_CSD_REV_1_6 6  // Revision 1.6, for MMC v4.5 and v4.51

#define MMC_EXT_CSD_DEVICE_TYPE 196
#define MMC_EXT_CSD_PARTITION_SWITCH_TIME 199
#define MMC_EXT_CSD_REL_WR_SEC_C 222
#define MMC_EXT_CSD_BOOT_SIZE_MULT 226

#define MMC_EXT_CSD_SEC_FEATURE_SUPPORT 231
#define MMC_EXT_CSD_SEC_FEATURE_SUPPORT_SEC_GB_CL_EN 4

#define MMC_EXT_CSD_GENERIC_CMD6_TIME 248
#define MMC_EXT_CSD_CACHE_SIZE_LSB 249
#define MMC_EXT_CSD_CACHE_SIZE_250 250
#define MMC_EXT_CSD_CACHE_SIZE_251 251
#define MMC_EXT_CSD_CACHE_SIZE_MSB 252
#define MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A 268
#define MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B 269

// All invalid values are set to this.
#define MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_INVALID 0xc

#define MMC_EXT_CSD_MAX_PACKED_WRITES 500
#define MMC_EXT_CSD_MAX_PACKED_READS 501

// Device register (CMD13 response) fields (SD/MMC)
#define MMC_STATUS_ADDR_OUT_OF_RANGE (1 << 31)
#define MMC_STATUS_ADDR_MISALIGN (1 << 30)
#define MMC_STATUS_BLOCK_LEN_ERR (1 << 29)
#define MMC_STATUS_ERASE_SEQ_ERR (1 << 28)
#define MMC_STATUS_ERASE_PARAM (1 << 27)
#define MMC_STATUS_WP_VIOLATION (1 << 26)
#define MMC_STATUS_DEVICE_LOCKED (1 << 25)
#define MMC_STATUS_LOCK_UNLOCK_FAILED (1 << 24)
#define MMC_STATUS_COM_CRC_ERR (1 << 23)
#define MMC_STATUS_ILLEGAL_COMMAND (1 << 22)
#define MMC_STATUS_DEVICE_ECC_FAILED (1 << 21)
#define MMC_STATUS_CC_ERR (1 << 20)
#define MMC_STATUS_ERR (1 << 19)
#define MMC_STATUS_CXD_OVERWRITE (1 << 16)
#define MMC_STATUS_WP_ERASE_SKIP (1 << 15)
#define MMC_STATUS_ERASE_RESET (1 << 13)
#define MMC_STATUS_CURRENT_STATE_MASK (0xf << 9)
#define MMC_STATUS_CURRENT_STATE(resp) ((resp) & MMC_STATUS_CURRENT_STATE_MASK)
/* eMMC4.5 Spec, Section 6.13, page 140: CURRENT_STATE Field:
 * 0 = Idle 1 = Ready 2 = Ident 3 = Stby command. 4 = Tran 5 = Data
 * 6 = Rcv 7 = Prg 8 = Dis 9 = Btst 10 = Slp 11–15 = reserved
 */
#define MMC_STATUS_CURRENT_STATE_STBY (0x3 << 9)
#define MMC_STATUS_CURRENT_STATE_TRAN (0x4 << 9)
#define MMC_STATUS_CURRENT_STATE_DATA (0x5 << 9)
#define MMC_STATUS_CURRENT_STATE_RECV (0x6 << 9)
#define MMC_STATUS_CURRENT_STATE_SLP (0xa << 9)
#define MMC_STATUS_READY_FOR_DATA (1 << 8)
#define MMC_STATUS_SWITCH_ERR (1 << 7)
#define MMC_STATUS_EXCEPTION_EVENT (1 << 6)
#define MMC_STATUS_APP_CMD (1 << 5)

__END_CDECLS

constexpr uint32_t kMaxPackedCommandsFor512ByteBlockSize = 63;
struct PackedCommand {
  uint8_t version;
  uint8_t rw;
  uint8_t num_entries;
  uint8_t padding[5];

  struct Arg {
    uint32_t cmd23_arg;
    uint32_t cmdXX_arg;
  } __PACKED;
  Arg arg[kMaxPackedCommandsFor512ByteBlockSize];
} __PACKED;

#endif  // SRC_DEVICES_BLOCK_LIB_SDMMC_INCLUDE_LIB_SDMMC_HW_H_
