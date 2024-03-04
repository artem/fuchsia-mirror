// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string.h>

#ifndef SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_SDMMC_H_
#define SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_SDMMC_H_

#define AML_SDMMC_TUNING_TEST_ATTEMPTS 5

#define AML_SDMMC_SRAM_MEMORY_BASE 0x200
#define AML_SDMMC_SRAM_MEMORY_SIZE 512
#define AML_SDMMC_PING_BUFFER_SIZE 512
#define AML_SDMMC_PONG_BUFFER_SIZE 512
#define AML_SDMMC_MAX_PIO_DESCS 32  // 16 * 32 = 512
#define AML_SDMMC_MAX_PIO_DATA_SIZE AML_SDMMC_PING_BUFFER_SIZE + AML_SDMMC_PONG_BUFFER_SIZE
#define AML_SDIO_PORTB_GPIO_REG_5_VAL 0x00020000
#define AML_SDIO_PORTB_PERIPHS_PINMUX2_VAL 0x01000000
#define AML_SDIO_PORTB_HHI_GCLK_MPEG0_VAL 0x02000000
#define AML_SDIO_PORTB_SDMMC_CLK_VAL 0xf181ffff

typedef struct {
  uint32_t cmd_info;
  uint32_t cmd_arg;
  uint32_t data_addr;
  uint32_t resp_addr;
} aml_sdmmc_desc_t;

typedef struct {
  uint32_t prefs;
} aml_sdmmc_config_t;

static const uint8_t aml_sdmmc_tuning_blk_pattern_4bit[64] = {
    0xff, 0x0f, 0xff, 0x00, 0xff, 0xcc, 0xc3, 0xcc, 0xc3, 0x3c, 0xcc, 0xff, 0xfe, 0xff, 0xfe, 0xef,
    0xff, 0xdf, 0xff, 0xdd, 0xff, 0xfb, 0xff, 0xfb, 0xbf, 0xff, 0x7f, 0xff, 0x77, 0xf7, 0xbd, 0xef,
    0xff, 0xf0, 0xff, 0xf0, 0x0f, 0xfc, 0xcc, 0x3c, 0xcc, 0x33, 0xcc, 0xcf, 0xff, 0xef, 0xff, 0xee,
    0xff, 0xfd, 0xff, 0xfd, 0xdf, 0xff, 0xbf, 0xff, 0xbb, 0xff, 0xf7, 0xff, 0xf7, 0x7f, 0x7b, 0xde,
};

static const uint8_t aml_sdmmc_tuning_blk_pattern_8bit[128] = {
    0xff, 0xff, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xcc, 0xcc, 0xcc, 0x33, 0xcc, 0xcc,
    0xcc, 0x33, 0x33, 0xcc, 0xcc, 0xcc, 0xff, 0xff, 0xff, 0xee, 0xff, 0xff, 0xff, 0xee, 0xee, 0xff,
    0xff, 0xff, 0xdd, 0xff, 0xff, 0xff, 0xdd, 0xdd, 0xff, 0xff, 0xff, 0xbb, 0xff, 0xff, 0xff, 0xbb,
    0xbb, 0xff, 0xff, 0xff, 0x77, 0xff, 0xff, 0xff, 0x77, 0x77, 0xff, 0x77, 0xbb, 0xdd, 0xee, 0xff,
    0xff, 0xff, 0xff, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xcc, 0xcc, 0xcc, 0x33, 0xcc,
    0xcc, 0xcc, 0x33, 0x33, 0xcc, 0xcc, 0xcc, 0xff, 0xff, 0xff, 0xee, 0xff, 0xff, 0xff, 0xee, 0xee,
    0xff, 0xff, 0xff, 0xdd, 0xff, 0xff, 0xff, 0xdd, 0xdd, 0xff, 0xff, 0xff, 0xbb, 0xff, 0xff, 0xff,
    0xbb, 0xbb, 0xff, 0xff, 0xff, 0x77, 0xff, 0xff, 0xff, 0x77, 0x77, 0xff, 0x77, 0xbb, 0xdd, 0xee,
};

#endif  // SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_SDMMC_H_
