/******************************************************************************
 *
 * Copyright(c) 2005 - 2014 Intel Corporation. All rights reserved.
 * Copyright(c) 2015 - 2017 Intel Deutschland GmbH
 * Copyright(c) 2018        Intel Corporation
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
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_XVT_XVT_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_XVT_XVT_H_

#include <linux/if_ether.h>
#include <linux/spinlock.h>

#include "constants.h"
#include "fw-api.h"
#include "fw/img.h"
#include "fw/notif-wait.h"
#include "fw/runtime.h"
#include "iwl-config.h"
#include "iwl-drv.h"
#include "iwl-op-mode.h"
#include "iwl-trans.h"

enum iwl_xvt_state {
  IWL_XVT_STATE_UNINITIALIZED = 0,
  IWL_XVT_STATE_NO_FW,
  IWL_XVT_STATE_INIT_STARTED,
  IWL_XVT_STATE_OPERATIONAL,
};

#define IWL_XVT_LOAD_MASK_INIT BIT(0)
#define IWL_XVT_LOAD_MASK_RUNTIME BIT(1)

#define NUM_OF_LMACS (2)
#define IWL_XVT_DBG_FLAGS_NO_DEFAULT_TXQ (BIT(2))
#define IWL_XVT_MAX_PAYLOADS_AMOUNT (16)

#define IWL_MAX_BAID 32
#define IWL_XVT_INVALID_STA 0xFF

/**
 * tx_meta_data - Holds data and member needed for tx
 * @tx_mod_thread: thread dedicated for tx traffic
 * @mod_tx_wq: send packets queue
 * @tx_task_operating: whether tx is active
 * @queue: FW queue ID for TX_CMD
 * @tx_counter: counts number of sent packets
 * @tot_tx: total number of packets required to sent
 * @mod_tx_done_wq: queue to wait on until all packets are sent and received
 * @txq_full: set to true when mod_tx_wq is full
 * @seq_num: sequence number of qos frames (per-tid)
 */
struct tx_meta_data {
  struct task_struct* tx_mod_thread;
  wait_queue_head_t mod_tx_wq;
  bool tx_task_operating;
  int queue;
  uint64_t tx_counter;
  uint32_t tot_tx;
  wait_queue_head_t mod_tx_done_wq;
  bool txq_full;
  uint16_t seq_num[IWL_MAX_TID_COUNT];
};

/*
 * struct iwl_xvt_reorder_statistics - reorder buffer statistics
 * @dropped: number of frames dropped (e.g. too old)
 * @released: total number of frames released (either in-order or
 *  out of order (after passing the reorder buffer)
 * @skipped: number of frames skipped the reorder buffer (in-order)
 * @reordered: number of frames gone through the reorder buffer (unordered)
 */
struct iwl_xvt_reorder_statistics {
  uint32_t dropped;
  uint32_t released;
  uint32_t skipped;
  uint32_t reordered;
};

/**
 * struct iwl_xvt_reorder_buffer - per ra/tid/queue reorder buffer
 * @head_sn: reorder window head sn
 * @num_stored: number of mpdus stored in the buffer
 * @buf_size: the reorder buffer size as set by the last addba request
 * @sta_id: sta id of this reorder buffer
 * @tid: tid of this reorder buffer
 * @queue: queue of this reorder buffer
 * @last_amsdu: track last ASMDU SN for duplication detection
 * @last_sub_index: track ASMDU sub frame index for duplication detection
 * @entries: number of pending frames (for each index)
 * @lock: protect reorder buffer internal state
 * @stats: reorder buffer statistics
 */
struct iwl_xvt_reorder_buffer {
  uint16_t head_sn;
  uint16_t num_stored;
  uint8_t buf_size;
  uint8_t sta_id;
  uint8_t tid;
  int queue;
  uint16_t last_amsdu;
  uint8_t last_sub_index;

  /*
   * we don't care about the actual frames, only their count.
   * avoid messing with reorder timer for that reason as well
   */
  uint16_t entries[IEEE80211_MAX_AMPDU_BUF_HT];

  spinlock_t lock; /* protect reorder buffer internal state */
  struct iwl_xvt_reorder_statistics stats;
};

/**
 * tx_queue_data - Holds tx data per tx queue
 * @tx_wq: TX sw queue
 * @tx_counter: Number of packets that were sent from this queue. Counts TX_RSP
 * @txq_full: Set to true when tx_wq is full
 * @allocated_queue: Whether queue is allocated
 */
struct tx_queue_data {
  wait_queue_head_t tx_wq;
  uint64_t tx_counter;
  bool txq_full;
  bool allocated_queue;
};

/**
 * tx_payload - Holds tx payload
 * @length: Payload length in bytes
 * @payload: Payload buffer
 */
struct tx_payload {
  uint16_t length;
  uint8_t payload[];
};

/**
 * iwl_sw_stack_config - Holds active SW stack config as set from user space
 * @load_mask: Which FW are to be loaded during SW stack up
 * @iwl_phy_cfg_cmd: Which calibrations should be done
 */
struct iwl_sw_stack_config {
  uint32_t load_mask;
  uint32_t calib_override_mask;
  uint32_t fw_dbg_flags;
  struct iwl_phy_cfg_cmd fw_calib_cmd_cfg[IWL_UCODE_TYPE_MAX];
};

/* Note: This structure is read from the device with IO accesses,
 * and the reading already does the endian conversion. As it is
 * read with uint32_t-sized accesses, any members with a different size
 * need to be ordered correctly though!
 */
struct iwl_error_event_table_v1 {
  uint32_t valid;          /* (nonzero) valid, (0) log is empty */
  uint32_t error_id;       /* type of error */
  uint32_t pc;             /* program counter */
  uint32_t blink1;         /* branch link */
  uint32_t blink2;         /* branch link */
  uint32_t ilink1;         /* interrupt link */
  uint32_t ilink2;         /* interrupt link */
  uint32_t data1;          /* error-specific data */
  uint32_t data2;          /* error-specific data */
  uint32_t data3;          /* error-specific data */
  uint32_t bcon_time;      /* beacon timer */
  uint32_t tsf_low;        /* network timestamp function timer */
  uint32_t tsf_hi;         /* network timestamp function timer */
  uint32_t gp1;            /* GP1 timer register */
  uint32_t gp2;            /* GP2 timer register */
  uint32_t gp3;            /* GP3 timer register */
  uint32_t ucode_ver;      /* uCode version */
  uint32_t hw_ver;         /* HW Silicon version */
  uint32_t brd_ver;        /* HW board version */
  uint32_t log_pc;         /* log program counter */
  uint32_t frame_ptr;      /* frame pointer */
  uint32_t stack_ptr;      /* stack pointer */
  uint32_t hcmd;           /* last host command header */
  uint32_t isr0;           /* isr status register LMPM_NIC_ISR0:
                            * rxtx_flag
                            */
  uint32_t isr1;           /* isr status register LMPM_NIC_ISR1:
                            * host_flag
                            */
  uint32_t isr2;           /* isr status register LMPM_NIC_ISR2:
                            * enc_flag
                            */
  uint32_t isr3;           /* isr status register LMPM_NIC_ISR3:
                            * time_flag
                            */
  uint32_t isr4;           /* isr status register LMPM_NIC_ISR4:
                            * wico interrupt
                            */
  uint32_t isr_pref;       /* isr status register LMPM_NIC_PREF_STAT */
  uint32_t wait_event;     /* wait event() caller address */
  uint32_t l2p_control;    /* L2pControlField */
  uint32_t l2p_duration;   /* L2pDurationField */
  uint32_t l2p_mhvalid;    /* L2pMhValidBits */
  uint32_t l2p_addr_match; /* L2pAddrMatchStat */
  uint32_t lmpm_pmg_sel;   /* indicate which clocks are turned on
                            * (LMPM_PMG_SEL)
                            */
  uint32_t u_timestamp;    /* indicate when the date and time of the
                            * compilation
                            */
  uint32_t flow_handler;   /* FH read/write pointers, RX credit */
} __packed;

/* Note: This structure is read from the device with IO accesses,
 * and the reading already does the endian conversion. As it is
 * read with uint32_t-sized accesses, any members with a different size
 * need to be ordered correctly though!
 */
struct iwl_error_event_table_v2 {
  uint32_t valid;          /* (nonzero) valid, (0) log is empty */
  uint32_t error_id;       /* type of error */
  uint32_t trm_hw_status0; /* TRM HW status */
  uint32_t trm_hw_status1; /* TRM HW status */
  uint32_t blink2;         /* branch link */
  uint32_t ilink1;         /* interrupt link */
  uint32_t ilink2;         /* interrupt link */
  uint32_t data1;          /* error-specific data */
  uint32_t data2;          /* error-specific data */
  uint32_t data3;          /* error-specific data */
  uint32_t bcon_time;      /* beacon timer */
  uint32_t tsf_low;        /* network timestamp function timer */
  uint32_t tsf_hi;         /* network timestamp function timer */
  uint32_t gp1;            /* GP1 timer register */
  uint32_t gp2;            /* GP2 timer register */
  uint32_t fw_rev_type;    /* firmware revision type */
  uint32_t major;          /* uCode version major */
  uint32_t minor;          /* uCode version minor */
  uint32_t hw_ver;         /* HW Silicon version */
  uint32_t brd_ver;        /* HW board version */
  uint32_t log_pc;         /* log program counter */
  uint32_t frame_ptr;      /* frame pointer */
  uint32_t stack_ptr;      /* stack pointer */
  uint32_t hcmd;           /* last host command header */
  uint32_t isr0;           /* isr status register LMPM_NIC_ISR0:
                            * rxtx_flag
                            */
  uint32_t isr1;           /* isr status register LMPM_NIC_ISR1:
                            * host_flag
                            */
  uint32_t isr2;           /* isr status register LMPM_NIC_ISR2:
                            * enc_flag
                            */
  uint32_t isr3;           /* isr status register LMPM_NIC_ISR3:
                            * time_flag
                            */
  uint32_t isr4;           /* isr status register LMPM_NIC_ISR4:
                            * wico interrupt
                            */
  uint32_t last_cmd_id;    /* last HCMD id handled by the firmware */
  uint32_t wait_event;     /* wait event() caller address */
  uint32_t l2p_control;    /* L2pControlField */
  uint32_t l2p_duration;   /* L2pDurationField */
  uint32_t l2p_mhvalid;    /* L2pMhValidBits */
  uint32_t l2p_addr_match; /* L2pAddrMatchStat */
  uint32_t lmpm_pmg_sel;   /* indicate which clocks are turned on
                            * (LMPM_PMG_SEL)
                            */
  uint32_t u_timestamp;    /* indicate when the date and time of the
                            * compilation
                            */
  uint32_t flow_handler;   /* FH read/write pointers, RX credit */
} __packed /* LOG_ERROR_TABLE_API_S_VER_3 */;

/* UMAC error struct - relevant starting from family 8000 chip.
 * Note: This structure is read from the device with IO accesses,
 * and the reading already does the endian conversion. As it is
 * read with uint32_t-sized accesses, any members with a different size
 * need to be ordered correctly though!
 */
struct iwl_umac_error_event_table {
  uint32_t valid;    /* (nonzero) valid, (0) log is empty */
  uint32_t error_id; /* type of error */
  uint32_t blink1;   /* branch link */
  uint32_t blink2;   /* branch link */
  uint32_t ilink1;   /* interrupt link */
  uint32_t ilink2;   /* interrupt link */
  uint32_t data1;    /* error-specific data */
  uint32_t data2;    /* error-specific data */
  uint32_t data3;    /* error-specific data */
  uint32_t umac_major;
  uint32_t umac_minor;
  uint32_t frame_pointer; /* core register 27*/
  uint32_t stack_pointer; /* core register 28 */
  uint32_t cmd_header;    /* latest host cmd sent to UMAC */
  uint32_t nic_isr_pref;  /* ISR status register */
} __packed;

/**
 * struct iwl_xvt_skb_info - driver data per skb
 * @dev_cmd: a pointer to the iwl_dev_cmd associated with this skb
 * @trans: transport data
 */
struct iwl_xvt_skb_info {
  struct iwl_device_cmd* dev_cmd;
  void* trans[2];
};

/**
 * struct iwl_xvt - the xvt op_mode
 *
 * @trans: pointer to the transport layer
 * @cfg: pointer to the driver's configuration
 * @fw: a pointer to the fw object
 * @dev: pointer to struct device for printing purposes
 */
struct iwl_xvt {
  struct iwl_trans* trans;
  const struct iwl_cfg* cfg;
  struct iwl_phy_db* phy_db;
  const struct iwl_fw* fw;
  struct device* dev;
  struct dentry* debugfs_dir;

  struct mutex mutex; /* Protects access to xVT struct */
  spinlock_t notif_lock;
  ; /* Protects notifications processing */
  enum iwl_xvt_state state;
  bool fw_error;

  struct iwl_notif_wait_data notif_wait;

  uint32_t error_event_table[2];
  bool fw_running;
  uint32_t umac_error_event_table;
  bool support_umac_log;

  struct iwl_sw_stack_config sw_stack_cfg;
  bool rx_hdr_enabled;

  bool apmg_pd_en;
  /* DMA buffer information */
  uint32_t dma_buffer_size;
  uint8_t* dma_cpu_addr;
  dma_addr_t dma_addr;

  struct iwl_fw_runtime fwrt;

  bool is_nvm_mac_override;
  uint8_t nvm_hw_addr[ETH_ALEN];
  uint8_t nvm_mac_addr[ETH_ALEN];

  struct tx_meta_data tx_meta_data[NUM_OF_LMACS];

  struct iwl_xvt_reorder_buffer reorder_bufs[IWL_MAX_BAID];

  /* members for enhanced tx command */
  struct tx_payload* payloads[IWL_XVT_MAX_PAYLOADS_AMOUNT];
  struct task_struct* tx_task;
  bool is_enhanced_tx;
  bool send_tx_resp;
  bool send_rx_mpdu;
  uint64_t num_of_tx_resp;
  uint64_t expected_tx_amount;
  wait_queue_head_t tx_done_wq;
  struct tx_queue_data queue_data[IWL_MAX_HW_QUEUES];
};

#define IWL_OP_MODE_GET_XVT(_op_mode) ((struct iwl_xvt*)((_op_mode)->op_mode_specific))

/******************
 * XVT Methods
 ******************/

/* Host Commands */
int __must_check iwl_xvt_send_cmd(struct iwl_xvt* xvt, struct iwl_host_cmd* cmd);
int __must_check iwl_xvt_send_cmd_pdu(struct iwl_xvt* xvt, uint32_t id, uint32_t flags,
                                      uint16_t len, const void* data);

/* Utils */
void iwl_xvt_get_nic_error_log_v1(struct iwl_xvt* xvt, struct iwl_error_event_table_v1* table);
void iwl_xvt_dump_nic_error_log_v1(struct iwl_xvt* xvt, struct iwl_error_event_table_v1* table);
void iwl_xvt_get_nic_error_log_v2(struct iwl_xvt* xvt, struct iwl_error_event_table_v2* table);
void iwl_xvt_dump_nic_error_log_v2(struct iwl_xvt* xvt, struct iwl_error_event_table_v2* table);
void iwl_xvt_get_umac_error_log(struct iwl_xvt* xvt, struct iwl_umac_error_event_table* table);
void iwl_xvt_dump_umac_error_log(struct iwl_xvt* xvt, struct iwl_umac_error_event_table* table);

/* User interface */
int iwl_xvt_user_cmd_execute(struct iwl_testmode* testmode, uint32_t cmd,
                             struct iwl_tm_data* data_in, struct iwl_tm_data* data_out,
                             bool* supported_cmd);

/* FW */
int iwl_xvt_run_fw(struct iwl_xvt* xvt, uint32_t ucode_type, bool cont_run);

/* NVM */
int iwl_xvt_nvm_init(struct iwl_xvt* xvt);

/* RX */
bool iwl_xvt_reorder(struct iwl_xvt* xvt, struct iwl_rx_packet* pkt);
void iwl_xvt_rx_frame_release(struct iwl_xvt* xvt, struct iwl_rx_packet* pkt);
void iwl_xvt_destroy_reorder_buffer(struct iwl_xvt* xvt, struct iwl_xvt_reorder_buffer* buf);

/* Based on mvm function: iwl_mvm_has_new_tx_api */
static inline bool iwl_xvt_is_unified_fw(struct iwl_xvt* xvt) {
  /* TODO - replace with TLV once defined */
  return xvt->trans->cfg->device_family >= IWL_DEVICE_FAMILY_22000;
}

static inline bool iwl_xvt_is_cdb_supported(struct iwl_xvt* xvt) {
  /*
   * TODO:
   * The issue of how to determine CDB APIs and usage is still not fully
   * defined.
   * There is a compilation for CDB and non-CDB FW, but there may
   * be also runtime check.
   * For now there is a TLV for checking compilation mode, but a
   * runtime check will also have to be here - once defined.
   */
  return fw_has_capa(&xvt->fw->ucode_capa, IWL_UCODE_TLV_CAPA_CDB_SUPPORT);
}

static inline struct agg_tx_status* iwl_xvt_get_agg_status(struct iwl_xvt* xvt,
                                                           struct iwl_mvm_tx_resp* tx_resp) {
  if (iwl_xvt_is_unified_fw(xvt)) {
    return &((struct iwl_mvm_tx_resp*)tx_resp)->status;
  } else {
    return ((struct iwl_mvm_tx_resp_v3*)tx_resp)->status;
  }
}

static inline uint32_t iwl_xvt_get_scd_ssn(struct iwl_xvt* xvt, struct iwl_mvm_tx_resp* tx_resp) {
  return le32_to_cpup((__le32*)iwl_xvt_get_agg_status(xvt, tx_resp) + tx_resp->frame_count) & 0xfff;
}

static inline bool iwl_xvt_has_default_txq(struct iwl_xvt* xvt) {
  return !(xvt->sw_stack_cfg.fw_dbg_flags & IWL_XVT_DBG_FLAGS_NO_DEFAULT_TXQ);
}

void iwl_xvt_free_tx_queue(struct iwl_xvt* xvt, uint8_t lmac_id);

int iwl_xvt_allocate_tx_queue(struct iwl_xvt* xvt, uint8_t sta_id, uint8_t lmac_id);

void iwl_xvt_txq_disable(struct iwl_xvt* xvt);

/* XVT debugfs */
#ifdef CPTCFG_IWLWIFI_DEBUGFS
int iwl_xvt_dbgfs_register(struct iwl_xvt* xvt, struct dentry* dbgfs_dir);
#else
static inline int iwl_xvt_dbgfs_register(struct iwl_xvt* xvt, struct dentry* dbgfs_dir) {
  return 0;
}
#endif /* CPTCFG_IWLWIFI_DEBUGFS */

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_XVT_XVT_H_
