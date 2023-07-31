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

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-phy-db.h"

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-debug.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-drv.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-op-mode.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-trans.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/compiler.h"

#define CHANNEL_NUM_SIZE 4 /* num of channels in calib_ch size */

#define PHY_DB_CMD 0x6c

/* for parsing of tx power channel group data that comes from the firmware*/
struct iwl_phy_db_chg_txp {
  __le32 space;
  __le16 max_channel_idx;
} __packed;

struct iwl_phy_db* iwl_phy_db_init(struct iwl_trans* trans) {
  struct iwl_phy_db* phy_db = calloc(1, sizeof(struct iwl_phy_db));

  if (!phy_db) {
    return phy_db;
  }

  phy_db->trans = trans;

  phy_db->n_group_txp = -1;
  phy_db->n_group_papd = -1;

  /* TODO: add default values of the phy db. */
  return phy_db;
}

/*
 * get phy db section: returns a pointer to a phy db section specified by
 * type and channel group id.
 */
struct iwl_phy_db_entry* iwl_phy_db_get_section(struct iwl_phy_db* phy_db,
                                                enum iwl_phy_db_section_type type,
                                                uint16_t chg_id) {
  if (!phy_db || type >= IWL_PHY_DB_MAX) {
    return NULL;
  }

  switch (type) {
    case IWL_PHY_DB_CFG:
      return &phy_db->cfg;
    case IWL_PHY_DB_CALIB_NCH:
      return &phy_db->calib_nch;
    case IWL_PHY_DB_CALIB_CHG_PAPD:
      if (chg_id >= phy_db->n_group_papd) {
        return NULL;
      }
      return &phy_db->calib_ch_group_papd[chg_id];
    case IWL_PHY_DB_CALIB_CHG_TXP:
      if (chg_id >= phy_db->n_group_txp) {
        return NULL;
      }
      return &phy_db->calib_ch_group_txp[chg_id];
    default:
      return NULL;
  }
  return NULL;
}

static void iwl_phy_db_free_section(struct iwl_phy_db* phy_db, enum iwl_phy_db_section_type type,
                                    uint16_t chg_id) {
  struct iwl_phy_db_entry* entry = iwl_phy_db_get_section(phy_db, type, chg_id);
  if (!entry) {
    return;
  }

  kfree(entry->data);
  entry->data = NULL;
  entry->size = 0;
}

void iwl_phy_db_free(struct iwl_phy_db* phy_db) {
  uint16_t i;

  if (!phy_db) {
    return;
  }

  iwl_phy_db_free_section(phy_db, IWL_PHY_DB_CFG, 0);
  iwl_phy_db_free_section(phy_db, IWL_PHY_DB_CALIB_NCH, 0);

  for (i = 0; i < phy_db->n_group_papd; i++) {
    iwl_phy_db_free_section(phy_db, IWL_PHY_DB_CALIB_CHG_PAPD, i);
  }
  kfree(phy_db->calib_ch_group_papd);

  for (i = 0; i < phy_db->n_group_txp; i++) {
    iwl_phy_db_free_section(phy_db, IWL_PHY_DB_CALIB_CHG_TXP, i);
  }
  kfree(phy_db->calib_ch_group_txp);

  kfree(phy_db);
}

// Parse the notification packet from the firmware, then populate to mvm->phy_db.
//
zx_status_t iwl_phy_db_set_section(struct iwl_phy_db* phy_db, struct iwl_rx_packet* pkt) {
  struct iwl_calib_res_notif_phy_db* phy_db_notif = (struct iwl_calib_res_notif_phy_db*)pkt->data;
  enum iwl_phy_db_section_type type = le16_to_cpu(phy_db_notif->type);
  uint16_t size = le16_to_cpu(phy_db_notif->length);
  struct iwl_phy_db_entry* entry;
  uint16_t chg_id = 0;

  if (!phy_db) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (type == IWL_PHY_DB_CALIB_CHG_PAPD) {
    chg_id = le16_to_cpup((__le16*)phy_db_notif->data);
    if (!phy_db->calib_ch_group_papd) {
      /*
       * Firmware sends the largest index first, so we can use
       * it to know how much we should allocate.
       */
      phy_db->calib_ch_group_papd = calloc(chg_id + 1, sizeof(struct iwl_phy_db_entry));
      if (!phy_db->calib_ch_group_papd) {
        return ZX_ERR_NO_MEMORY;
      }
      phy_db->n_group_papd = chg_id + 1;
    }
  } else if (type == IWL_PHY_DB_CALIB_CHG_TXP) {
    chg_id = le16_to_cpup((__le16*)phy_db_notif->data);
    if (!phy_db->calib_ch_group_txp) {
      /*
       * Firmware sends the largest index first, so we can use
       * it to know how much we should allocate.
       */
      phy_db->calib_ch_group_txp = calloc(chg_id + 1, sizeof(struct iwl_phy_db_entry));
      if (!phy_db->calib_ch_group_txp) {
        return ZX_ERR_NO_MEMORY;
      }
      phy_db->n_group_txp = chg_id + 1;
    }
  }

  entry = iwl_phy_db_get_section(phy_db, type, chg_id);
  if (!entry) {
    return ZX_ERR_INVALID_ARGS;
  }

  free(entry->data);
  entry->data = kmemdup(phy_db_notif->data, size);
  if (!entry->data) {
    entry->size = 0;
    return ZX_ERR_NO_MEMORY;
  }

  entry->size = size;

  IWL_DEBUG_INFO(phy_db->trans, "%s(%d): [PHYDB]SET: Type %d , Size: %d\n", __func__, __LINE__,
                 type, size);

  return ZX_OK;
}

static int is_valid_channel(uint16_t ch_id) {
  if (ch_id <= 14 || (36 <= ch_id && ch_id <= 64 && ch_id % 4 == 0) ||
      (100 <= ch_id && ch_id <= 140 && ch_id % 4 == 0) ||
      (145 <= ch_id && ch_id <= 165 && ch_id % 4 == 1)) {
    return 1;
  }
  return 0;
}

static uint8_t ch_id_to_ch_index(uint16_t ch_id) {
  if (WARN_ON(!is_valid_channel(ch_id))) {
    return 0xff;
  }

  if (ch_id <= 14) {
    return (uint8_t)(ch_id - 1);
  }
  if (ch_id <= 64) {
    return (uint8_t)((ch_id + 20) / 4);
  }
  if (ch_id <= 140) {
    return (uint8_t)((ch_id - 12) / 4);
  }
  return (uint8_t)((ch_id - 13) / 4);
}

static uint16_t channel_id_to_papd(uint16_t ch_id) {
  if (WARN_ON(!is_valid_channel(ch_id))) {
    return 0xff;
  }

  if (1 <= ch_id && ch_id <= 14) {
    return 0;
  }
  if (36 <= ch_id && ch_id <= 64) {
    return 1;
  }
  if (100 <= ch_id && ch_id <= 140) {
    return 2;
  }
  return 3;
}

static uint16_t channel_id_to_txp(struct iwl_phy_db* phy_db, uint16_t ch_id) {
  struct iwl_phy_db_chg_txp* txp_chg;
  uint16_t i;
  uint8_t ch_index = ch_id_to_ch_index(ch_id);
  if (ch_index == 0xff) {
    return 0xff;
  }

  for (i = 0; i < phy_db->n_group_txp; i++) {
    txp_chg = (void*)phy_db->calib_ch_group_txp[i].data;
    if (!txp_chg) {
      return 0xff;
    }
    /*
     * Looking for the first channel group that its max channel is
     * higher then wanted channel.
     */
    if (le16_to_cpu(txp_chg->max_channel_idx) >= ch_index) {
      return i;
    }
  }
  return 0xff;
}

static zx_status_t iwl_phy_db_get_section_data(struct iwl_phy_db* phy_db, uint32_t type,
                                               uint8_t** data, uint16_t* size, uint16_t ch_id) {
  struct iwl_phy_db_entry* entry;
  uint16_t ch_group_id = 0;

  if (!phy_db) {
    return ZX_ERR_INVALID_ARGS;
  }

  /* find wanted channel group */
  if (type == IWL_PHY_DB_CALIB_CHG_PAPD) {
    ch_group_id = channel_id_to_papd(ch_id);
  } else if (type == IWL_PHY_DB_CALIB_CHG_TXP) {
    ch_group_id = channel_id_to_txp(phy_db, ch_id);
  }

  entry = iwl_phy_db_get_section(phy_db, type, ch_group_id);
  if (!entry) {
    return ZX_ERR_INVALID_ARGS;
  }

  *data = entry->data;
  *size = entry->size;

  IWL_DEBUG_INFO(phy_db->trans, "%s(%d): [PHYDB] GET: Type %d , Size: %d\n", __func__, __LINE__,
                 type, *size);

  return ZX_OK;
}

static zx_status_t iwl_send_phy_db_cmd(struct iwl_phy_db* phy_db, uint16_t type, uint16_t length,
                                       void* data) {
  struct iwl_phy_db_cmd phy_db_cmd;
  struct iwl_host_cmd cmd = {
      .id = PHY_DB_CMD,
  };

  IWL_DEBUG_INFO(phy_db->trans, "Sending PHY-DB hcmd of type %d, of length %d\n", type, length);

  /* Set phy db cmd variables */
  phy_db_cmd.type = cpu_to_le16(type);
  phy_db_cmd.length = cpu_to_le16(length);

  /* Set hcmd variables */
  cmd.data[0] = &phy_db_cmd;
  cmd.len[0] = sizeof(struct iwl_phy_db_cmd);
  cmd.data[1] = data;
  cmd.len[1] = length;
  cmd.dataflags[1] = IWL_HCMD_DFL_NOCOPY;

  return iwl_trans_send_cmd(phy_db->trans, &cmd);
}

zx_status_t iwl_phy_db_send_all_channel_groups(struct iwl_phy_db* phy_db,
                                               enum iwl_phy_db_section_type type,
                                               int max_ch_groups) {
  zx_status_t err;
  struct iwl_phy_db_entry* entry;

  /* Send all the  channel specific groups to operational fw */
  for (int i = 0; i < max_ch_groups; i++) {
    entry = iwl_phy_db_get_section(phy_db, type, (uint16_t)i);
    if (!entry) {
      return ZX_ERR_INVALID_ARGS;
    }

    if (!entry->size) {
      continue;
    }

    /* Send the requested PHY DB section */
    err = iwl_send_phy_db_cmd(phy_db, (uint16_t)type, entry->size, entry->data);
    if (err != ZX_OK) {
      IWL_ERR(phy_db->trans, "Can't SEND phy_db section %d (%d), err %d\n", type, i, err);
      return err;
    }

    IWL_DEBUG_INFO(phy_db->trans, "Sent PHY_DB HCMD, type = %d num = %d\n", type, i);
  }

  return ZX_OK;
}

zx_status_t iwl_send_phy_db_data(struct iwl_phy_db* phy_db) {
  uint8_t* data = NULL;
  uint16_t size = 0;
  zx_status_t err;

  IWL_DEBUG_INFO(phy_db->trans, "Sending phy db data and configuration to runtime image\n");

  /* Send PHY DB CFG section */
  err = iwl_phy_db_get_section_data(phy_db, IWL_PHY_DB_CFG, &data, &size, 0);
  if (err != ZX_OK) {
    IWL_ERR(phy_db->trans, "Cannot get Phy DB cfg section\n");
    return err;
  }

  err = iwl_send_phy_db_cmd(phy_db, IWL_PHY_DB_CFG, size, data);
  if (err != ZX_OK) {
    IWL_ERR(phy_db->trans, "Cannot send HCMD of  Phy DB cfg section\n");
    return err;
  }

  err = iwl_phy_db_get_section_data(phy_db, IWL_PHY_DB_CALIB_NCH, &data, &size, 0);
  if (err != ZX_OK) {
    IWL_ERR(phy_db->trans, "Cannot get Phy DB non specific channel section\n");
    return err;
  }

  err = iwl_send_phy_db_cmd(phy_db, IWL_PHY_DB_CALIB_NCH, size, data);
  if (err != ZX_OK) {
    IWL_ERR(phy_db->trans, "Cannot send HCMD of Phy DB non specific channel section\n");
    return err;
  }

  /* Send all the TXP channel specific data */
  err = iwl_phy_db_send_all_channel_groups(phy_db, IWL_PHY_DB_CALIB_CHG_PAPD, phy_db->n_group_papd);
  if (err != ZX_OK) {
    IWL_ERR(phy_db->trans, "Cannot send channel specific PAPD groups\n");
    return err;
  }

  /* Send all the TXP channel specific data */
  err = iwl_phy_db_send_all_channel_groups(phy_db, IWL_PHY_DB_CALIB_CHG_TXP, phy_db->n_group_txp);
  if (err != ZX_OK) {
    IWL_ERR(phy_db->trans, "Cannot send channel specific TX power groups\n");
    return err;
  }

  IWL_DEBUG_INFO(phy_db->trans, "Finished sending phy db non channel data\n");
  return ZX_OK;
}
