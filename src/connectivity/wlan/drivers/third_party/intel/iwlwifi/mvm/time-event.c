/******************************************************************************
 *
 * Copyright(c) 2012 - 2014 Intel Corporation. All rights reserved.
 * Copyright(c) 2013 - 2015 Intel Mobile Communications GmbH
 * Copyright(c) 2017 Intel Deutschland GmbH
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

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/mvm/time-event.h"

#include <lib/async/time.h>
#include <zircon/status.h>

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/fw/notif-wait.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-io.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-prph.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/iwl-trans.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/mvm/fw-api.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/mvm/mvm.h"
#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/time.h"

/*
 * For the high priority TE use a time event type that has similar priority to
 * the FW's action scan priority.
 */
#define IWL_MVM_ROC_TE_TYPE_NORMAL TE_P2P_DEVICE_DISCOVERABLE
#define IWL_MVM_ROC_TE_TYPE_MGMT_TX TE_P2P_CLIENT_ASSOC

void iwl_mvm_te_clear_data(struct iwl_mvm* mvm, struct iwl_mvm_time_event_data* te_data) {
  iwl_assert_lock_held(&mvm->time_event_lock);

  if (!te_data->mvmvif) {
    return;
  }

  list_delete(&te_data->list);
  te_data->running = false;
  te_data->uid = 0;
  te_data->id = TE_MAX;
  te_data->mvmvif = NULL;
}

#if 0   // NEEDS_PORTING
void iwl_mvm_roc_done_wk(struct work_struct* wk) {
  struct iwl_mvm* mvm = container_of(wk, struct iwl_mvm, roc_done_wk);

  /*
	 * Clear the ROC_RUNNING status bit.
   * This will cause the TX path to drop offchannel transmissions.
   * That would also be done by mac80211, but it is racy, in particular
   * in the case that the time event actually completed in the firmware
   * (which is handled in iwl_mvm_te_handle_notif).
   */
	clear_bit(IWL_MVM_STATUS_ROC_RUNNING, &mvm->status);

  synchronize_net();

  /*
   * Flush the offchannel queue -- this is called when the time
   * event finishes or is canceled, so that frames queued for it
   * won't get stuck on the queue and be transmitted in the next
   * time event.
   */

	mutex_lock(&mvm->mutex);
  if (test_and_clear_bit(IWL_MVM_STATUS_NEED_FLUSH_P2P, &mvm->status)) {
    struct iwl_mvm_vif* mvmvif;

    /*
     * NB: access to this pointer would be racy, but the flush bit
     * can only be set when we had a P2P-Device VIF, and we have a
     * flush of this work in iwl_mvm_prepare_mac_removal() so it's
     * not really racy.
     */

    if (!WARN_ON(!mvm->p2p_device_vif)) {
      mvmvif = iwl_mvm_vif_from_mac80211(mvm->p2p_device_vif);
			iwl_mvm_flush_sta(mvm, &mvmvif->bcast_sta, true);
    }
  }

	/*
	 * Clear the ROC_AUX_RUNNING status bit.
	 * This will cause the TX path to drop offchannel transmissions.
	 * That would also be done by mac80211, but it is racy, in particular
	 * in the case that the time event actually completed in the firmware
	 * (which is handled in iwl_mvm_te_handle_notif).
	 */
	if (test_and_clear_bit(IWL_MVM_STATUS_ROC_AUX_RUNNING, &mvm->status)) {
		/* do the same in case of hot spot 2.0 */
		iwl_mvm_flush_sta(mvm, &mvm->aux_sta, true);

		/* In newer version of this command an aux station is added only
		 * in cases of dedicated tx queue and need to be removed in end
		 * of use */
		if (iwl_fw_lookup_cmd_ver(mvm->fw, ADD_STA, 0) >= 12)
			iwl_mvm_rm_aux_sta(mvm);
	}

	mutex_unlock(&mvm->mutex);
}

static void iwl_mvm_roc_finished(struct iwl_mvm* mvm) {
  /*
   * Of course, our status bit is just as racy as mac80211, so in
   * addition, fire off the work struct which will drop all frames
   * from the hardware queues that made it through the race. First
   * it will of course synchronize the TX path to make sure that
   * any *new* TX will be rejected.
   */
  schedule_work(&mvm->roc_done_wk);
}

static void iwl_mvm_csa_noa_start(struct iwl_mvm* mvm) {
  struct ieee80211_vif* csa_vif;

  rcu_read_lock();

  csa_vif = rcu_dereference(mvm->csa_vif);
  if (!csa_vif || !csa_vif->csa_active) {
    goto out_unlock;
  }

  IWL_DEBUG_TE(mvm, "CSA NOA started\n");

  /*
   * CSA NoA is started but we still have beacons to
   * transmit on the current channel.
   * So we just do nothing here and the switch
   * will be performed on the last TBTT.
   */
	if (!ieee80211_beacon_cntdwn_is_complete(csa_vif)) {
    IWL_WARN(mvm, "CSA NOA started too early\n");
    goto out_unlock;
  }

  ieee80211_csa_finish(csa_vif);

  rcu_read_unlock();

  RCU_INIT_POINTER(mvm->csa_vif, NULL);

  return;

out_unlock:
  rcu_read_unlock();
}
#endif  // NEEDS_PORTING

static bool iwl_mvm_te_check_disconnect(struct iwl_mvm* mvm, struct iwl_mvm_vif* mvmvif,
                                        const char* errmsg) {
  if (mvmvif->mac_role != WLAN_MAC_ROLE_CLIENT) {
    return false;
  }

  if (!mvmvif->csa_bcn_pending && mvmvif->bss_conf.assoc && mvmvif->bss_conf.dtim_period) {
    return false;
  }
  if (errmsg) {
    IWL_ERR(mvm, "%s\n", errmsg);
  }

#if 0   // NEEDS_PORTING
	if (vif->bss_conf.assoc) {
		/*
		 * When not associated, this will be called from
		 * iwl_mvm_event_mlme_callback_ini()
		 */
		iwl_dbg_tlv_time_point(&mvm->fwrt,
				       IWL_FW_INI_TIME_POINT_ASSOC_FAILED,
				       NULL);
	}

  iwl_mvm_connection_loss(mvm, vif, errmsg);
#endif  // NEEDS_PORTING
  return true;
}

#if 0   // NEEDS_PORTING
static void iwl_mvm_te_handle_notify_csa(struct iwl_mvm* mvm,
                                         struct iwl_mvm_time_event_data* te_data,
                                         struct iwl_time_event_notif* notif) {
  struct ieee80211_vif* vif = te_data->vif;
  struct iwl_mvm_vif* mvmvif = iwl_mvm_vif_from_mac80211(vif);

  if (!notif->status) {
    IWL_DEBUG_TE(mvm, "CSA time event failed to start\n");
  }

  switch (te_data->vif->type) {
    case NL80211_IFTYPE_AP:
      if (!notif->status) {
        mvmvif->csa_failed = true;
      }
      iwl_mvm_csa_noa_start(mvm);
      break;
    case NL80211_IFTYPE_STATION:
      if (!notif->status) {
        iwl_mvm_connection_loss(mvm, vif, "CSA TE failed to start");
        break;
      }
      iwl_mvm_csa_client_absent(mvm, te_data->vif);
		cancel_delayed_work(&mvmvif->csa_work);
      ieee80211_chswitch_done(te_data->vif, true);
      break;
    default:
      /* should never happen */
      WARN_ON_ONCE(1);
      break;
  }

  /* we don't need it anymore */
  iwl_mvm_te_clear_data(mvm, te_data);
}

static void iwl_mvm_te_check_trigger(struct iwl_mvm* mvm, struct iwl_time_event_notif* notif,
                                     struct iwl_mvm_time_event_data* te_data) {
  struct iwl_fw_dbg_trigger_tlv* trig;
  struct iwl_fw_dbg_trigger_time_event* te_trig;
  int i;

  trig = iwl_fw_dbg_trigger_on(&mvm->fwrt, ieee80211_vif_to_wdev(te_data->vif),
                               FW_DBG_TRIGGER_TIME_EVENT);
  if (!trig) {
    return;
  }

  te_trig = (void*)trig->data;

  for (i = 0; i < ARRAY_SIZE(te_trig->time_events); i++) {
    uint32_t trig_te_id = le32_to_cpu(te_trig->time_events[i].id);
    uint32_t trig_action_bitmap = le32_to_cpu(te_trig->time_events[i].action_bitmap);
    uint32_t trig_status_bitmap = le32_to_cpu(te_trig->time_events[i].status_bitmap);

    if (trig_te_id != te_data->id || !(trig_action_bitmap & le32_to_cpu(notif->action)) ||
        !(trig_status_bitmap & BIT(le32_to_cpu(notif->status)))) {
      continue;
    }

    iwl_fw_dbg_collect_trig(&mvm->fwrt, trig, "Time event %d Action 0x%x received status: %d",
                            te_data->id, le32_to_cpu(notif->action), le32_to_cpu(notif->status));
    break;
  }
}
#endif  // NEEDS_PORTING

/*
 * Handles a FW notification for an event that is known to the driver.
 *
 * @mvm: the mvm component
 * @te_data: the time event data
 * @notif: the notification data corresponding the time event data.
 */
static void iwl_mvm_te_handle_notif(struct iwl_mvm* mvm, struct iwl_mvm_time_event_data* te_data,
                                    struct iwl_time_event_notif* notif) {
  iwl_assert_lock_held(&mvm->time_event_lock);

  IWL_DEBUG_TE(mvm, "Handle time event notif - UID = 0x%x action %d\n",
               le32_to_cpu(notif->unique_id), le32_to_cpu(notif->action));

#if 0   // NEEDS_PORTING
  iwl_mvm_te_check_trigger(mvm, notif, te_data);
#endif  // NEEDS_PORTING

  /*
   * The FW sends the start/end time event notifications even for events
   * that it fails to schedule. This is indicated in the status field of
   * the notification. This happens in cases that the scheduler cannot
   * find a schedule that can handle the event (for example requesting a
   * P2P Device discoveribility, while there are other higher priority
   * events in the system).
   */
  if (!le32_to_cpu(notif->status)) {
    const char* msg;

    if (notif->action & cpu_to_le32(TE_V2_NOTIF_HOST_EVENT_START)) {
      msg = "Time Event start notification failure";
    } else {
      msg = "Time Event end notification failure";
    }

    IWL_DEBUG_TE(mvm, "%s\n", msg);

    if (iwl_mvm_te_check_disconnect(mvm, te_data->mvmvif, msg)) {
      iwl_mvm_te_clear_data(mvm, te_data);
      return;
    }
  }

  if (le32_to_cpu(notif->action) & TE_V2_NOTIF_HOST_EVENT_END) {
    IWL_DEBUG_TE(mvm, "TE ended - current time %lu, estimated end %lu\n", iwl_time_now(mvm->dev),
                 te_data->end_time);

    switch (te_data->mvmvif->mac_role) {
#if 0   // NEEDS_PORTING
      case NL80211_IFTYPE_P2P_DEVICE:
        ieee80211_remain_on_channel_expired(mvm->hw);
			iwl_mvm_p2p_roc_finished(mvm);
        break;
#endif  // NEEDS_PORTING
      case WLAN_MAC_ROLE_CLIENT:

        /*
         * By now, we should have finished association
         * and know the dtim period.
         */
        iwl_mvm_te_check_disconnect(mvm, te_data->mvmvif,
                                    "No beacon heard and the time event is over already...");
        break;
      default:
        break;
    }

    iwl_mvm_te_clear_data(mvm, te_data);
  } else if (le32_to_cpu(notif->action) & TE_V2_NOTIF_HOST_EVENT_START) {
    te_data->running = true;
    te_data->end_time = TU_TO_DURATION(te_data->duration);

#if 0   // NEEDS_PORTING
    if (te_data->vif->type == NL80211_IFTYPE_P2P_DEVICE) {
      set_bit(IWL_MVM_STATUS_ROC_RUNNING, &mvm->status);
      ieee80211_ready_on_channel(mvm->hw);
    } else if (te_data->id == TE_CHANNEL_SWITCH_PERIOD) {
      iwl_mvm_te_handle_notify_csa(mvm, te_data, notif);
    }
#endif  // NEEDS_PORTING
  } else {
    IWL_WARN(mvm, "Got TE with unknown action\n");
  }
}

#if 0   // NEEDS_PORTING
/*
 * Handle A Aux ROC time event
 */
static int iwl_mvm_aux_roc_te_handle_notif(struct iwl_mvm* mvm,
                                           struct iwl_time_event_notif* notif) {
  struct iwl_mvm_time_event_data *te_data, *tmp;
  bool aux_roc_te = false;

  list_for_each_entry_safe(te_data, tmp, &mvm->aux_roc_te_list, list) {
    if (le32_to_cpu(notif->unique_id) == te_data->uid) {
      aux_roc_te = true;
      break;
    }
  }
  if (!aux_roc_te) { /* Not a Aux ROC time event */
    return -EINVAL;
  }

  iwl_mvm_te_check_trigger(mvm, notif, te_data);

  IWL_DEBUG_TE(mvm, "Aux ROC time event notification  - UID = 0x%x action %d (error = %d)\n",
               le32_to_cpu(notif->unique_id), le32_to_cpu(notif->action),
               le32_to_cpu(notif->status));

  if (!le32_to_cpu(notif->status) || le32_to_cpu(notif->action) == TE_V2_NOTIF_HOST_EVENT_END) {
    /* End TE, notify mac80211 */
    ieee80211_remain_on_channel_expired(mvm->hw);
    iwl_mvm_roc_finished(mvm); /* flush aux queue */
    list_del(&te_data->list);  /* remove from list */
    te_data->running = false;
    te_data->vif = NULL;
    te_data->uid = 0;
    te_data->id = TE_MAX;
  } else if (le32_to_cpu(notif->action) == TE_V2_NOTIF_HOST_EVENT_START) {
    set_bit(IWL_MVM_STATUS_ROC_AUX_RUNNING, &mvm->status);
    te_data->running = true;
    ieee80211_ready_on_channel(mvm->hw); /* Start TE */
  } else {
    IWL_DEBUG_TE(mvm, "ERROR: Unknown Aux ROC Time Event (action = %d)\n",
                 le32_to_cpu(notif->action));
    return -EINVAL;
  }

  return 0;
}
#endif  // NEEDS_PORTING

/*
 * The Rx handler for time event notifications
 */
void iwl_mvm_rx_time_event_notif(struct iwl_mvm* mvm, struct iwl_rx_cmd_buffer* rxb) {
  struct iwl_rx_packet* pkt = rxb_addr(rxb);
  struct iwl_time_event_notif* notif = (void*)pkt->data;
  struct iwl_mvm_time_event_data *te_data, *tmp;

  IWL_DEBUG_TE(mvm, "Time event notification - UID = 0x%x action %d\n",
               le32_to_cpu(notif->unique_id), le32_to_cpu(notif->action));

  mtx_lock(&mvm->time_event_lock);

#if 0   // NEEDS_PORTING
  /* This time event is triggered for Aux ROC request */
  if (!iwl_mvm_aux_roc_te_handle_notif(mvm, notif)) {
    goto unlock;
  }
#endif  // NEEDS_PORTING

  list_for_every_entry_safe (&mvm->time_event_list, te_data, tmp, struct iwl_mvm_time_event_data,
                             list) {
    if (le32_to_cpu(notif->unique_id) == te_data->uid) {
      iwl_mvm_te_handle_notif(mvm, te_data, notif);
    }
  }

  mtx_unlock(&mvm->time_event_lock);
}

static bool iwl_mvm_te_notif(struct iwl_notif_wait_data* notif_wait, struct iwl_rx_packet* pkt,
                             void* data) {
  struct iwl_mvm_time_event_data* te_data = data;
  struct iwl_time_event_notif* resp;
  int resp_len = iwl_rx_packet_payload_len(pkt);

  if (pkt->hdr.cmd != TIME_EVENT_NOTIFICATION) {
    IWL_WARN(notif_wait, "response packet ID is not expected: 0x%02x\n", pkt->hdr.cmd);
    return true;
  }

  if (resp_len != sizeof(*resp)) {
    IWL_ERR(mvm, "Invalid TIME_EVENT_NOTIFICATION response\n");
    return true;
  }

  resp = (void*)pkt->data;

  /* te_data->uid is already set in the TIME_EVENT_CMD response */
  if (le32_to_cpu(resp->unique_id) != te_data->uid) {
    return false;
  }

  IWL_DEBUG_TE(mvm, "TIME_EVENT_NOTIFICATION response - UID = 0x%x\n", te_data->uid);
  if (!resp->status) {
    IWL_ERR(mvm, "TIME_EVENT_NOTIFICATION received but not executed\n");
  }

  return true;
}

static bool iwl_mvm_time_event_response(struct iwl_notif_wait_data* notif_wait,
                                        struct iwl_rx_packet* pkt, void* data) {
  struct iwl_mvm_time_event_data* te_data = data;
  struct iwl_time_event_resp* resp;
  int resp_len = iwl_rx_packet_payload_len(pkt);

  if (pkt->hdr.cmd != TIME_EVENT_CMD) {
    IWL_WARN(notif_wait, "time event packet cmd is not exoected: 0x%x\n", pkt->hdr.cmd);
    return true;
  }

  if (resp_len != sizeof(*resp)) {
    IWL_ERR(mvm, "Invalid TIME_EVENT_CMD response\n");
    return true;
  }

  resp = (void*)pkt->data;

  /* we should never get a response to another TIME_EVENT_CMD here */
  if (le32_to_cpu(resp->id) != te_data->id) {
    IWL_ERR(notif_wait, "response ID is not expected: %d vs %d\n", le32_to_cpu(resp->id),
            te_data->id);
    return false;
  }

  te_data->uid = le32_to_cpu(resp->unique_id);
  IWL_DEBUG_TE(mvm, "TIME_EVENT_CMD response - UID = 0x%x\n", te_data->uid);
  return true;
}

static zx_status_t iwl_mvm_time_event_send_add(struct iwl_mvm_vif* mvmvif,
                                               struct iwl_mvm_time_event_data* te_data,
                                               struct iwl_time_event_cmd* te_cmd) {
  struct iwl_mvm* mvm = mvmvif->mvm;
  static const uint16_t time_event_response[] = {TIME_EVENT_CMD};
  struct iwl_notification_wait wait_time_event;
  int ret;

  iwl_assert_lock_held(&mvm->mutex);

  IWL_DEBUG_TE(mvm, "Add new TE, duration %d TU\n", le32_to_cpu(te_cmd->duration));

  mtx_lock(&mvm->time_event_lock);
  if (te_data->id != TE_MAX) {
    IWL_WARN(mvmvif, "te_data->id should be TE_MAX(%d), but actually (%d)\n", TE_MAX, te_data->id);
    mtx_unlock(&mvm->time_event_lock);
    return ZX_ERR_BAD_STATE;
  }
  // TODO(fxbug.dev/88102): ensure pointer validation.
  te_data->mvmvif = mvmvif;
  te_data->duration = le32_to_cpu(te_cmd->duration);
  te_data->id = le32_to_cpu(te_cmd->id);
  list_add_tail(&mvm->time_event_list, &te_data->list);
  mtx_unlock(&mvm->time_event_lock);

  /*
   * Use a notification wait, which really just processes the
   * command response and doesn't wait for anything, in order
   * to be able to process the response and get the UID inside
   * the RX path. Using CMD_WANT_SKB doesn't work because it
   * stores the buffer and then wakes up this thread, by which
   * time another notification (that the time event started)
   * might already be processed unsuccessfully.
   */
  iwl_init_notification_wait(&mvm->notif_wait, &wait_time_event, time_event_response,
                             ARRAY_SIZE(time_event_response), iwl_mvm_time_event_response, te_data);

  ret = iwl_mvm_send_cmd_pdu(mvm, TIME_EVENT_CMD, 0, sizeof(*te_cmd), te_cmd);
  if (ret != ZX_OK) {
    IWL_ERR(mvm, "Couldn't send TIME_EVENT_CMD: %d\n", ret);
    iwl_remove_notification(&mvm->notif_wait, &wait_time_event);
    goto out_clear_te;
  }

  /* No need to wait for anything, so just pass a time in the past */
  ret = iwl_wait_notification(&mvm->notif_wait, &wait_time_event, ZX_TIME_INFINITE_PAST);
  /* should never fail */
  if (ret != ZX_OK) {
    IWL_WARN(mvmvif, "wait_time_event failed: %s\n", zx_status_get_string(ret));
  }

out_clear_te:
  if (ret != ZX_OK) {
    mtx_lock(&mvm->time_event_lock);
    iwl_mvm_te_clear_data(mvm, te_data);
    mtx_unlock(&mvm->time_event_lock);
  }

  return ret;
}

// Tells firmware to start a time event and waits for the notification.
//
// Protect the session to make sure we hear the first beacon on the new channel.
//
// Read the time-event.h for more details about what it is and how it works.
//
// Args:
//   duration, min_duration, max_delay: all in beacon time unit
//
zx_status_t iwl_mvm_protect_session(struct iwl_mvm* mvm, struct iwl_mvm_vif* mvmvif,
                                    uint32_t duration, uint32_t min_duration, uint32_t max_delay,
                                    bool wait_for_notif) {
  struct iwl_mvm_time_event_data* te_data = &mvmvif->time_event_data;
  const uint16_t te_notif_response[] = {TIME_EVENT_NOTIFICATION};
  struct iwl_notification_wait wait_te_notif;
  struct iwl_time_event_cmd time_cmd = {};

  iwl_assert_lock_held(&mvm->mutex);

  if (te_data->running &&
      te_data->end_time > iwl_time_now(mvm->dev) + TU_TO_DURATION(min_duration)) {
    IWL_DEBUG_TE(mvm, "We have enough time in the current TE: %lu\n",
                 zx_time_sub_time(te_data->end_time, iwl_time_now(mvm->dev)));
    return ZX_OK;
  }

  if (te_data->running) {
    IWL_DEBUG_TE(mvm, "extend 0x%x: only %lu ms left\n", te_data->uid,
                 zx_time_sub_time(te_data->end_time, iwl_time_now(mvm->dev)));
    /*
     * we don't have enough time
     * cancel the current TE and issue a new one
     * Of course it would be better to remove the old one only
     * when the new one is added, but we don't care if we are off
     * channel for a bit. All we need to do, is not to return
     * before we actually begin to be on the channel.
     */
    iwl_mvm_stop_session_protection(mvmvif);
  }

  time_cmd.action = cpu_to_le32(FW_CTXT_ACTION_ADD);
  time_cmd.id_and_color = cpu_to_le32(FW_CMD_ID_AND_COLOR(mvmvif->id, mvmvif->color));
  time_cmd.id = cpu_to_le32(TE_BSS_STA_AGGRESSIVE_ASSOC);

  time_cmd.apply_time = cpu_to_le32(0);

  time_cmd.max_frags = TE_V2_FRAG_NONE;
  time_cmd.max_delay = cpu_to_le32(max_delay);
  /* TODO: why do we need to interval = bi if it is not periodic? */
  time_cmd.interval = cpu_to_le32(1);
  time_cmd.duration = cpu_to_le32(duration);
  time_cmd.repeat = 1;
  time_cmd.policy = cpu_to_le16(TE_V2_NOTIF_HOST_EVENT_START | TE_V2_NOTIF_HOST_EVENT_END |
                                TE_V2_START_IMMEDIATELY);

  if (!wait_for_notif) {
    return iwl_mvm_time_event_send_add(mvmvif, te_data, &time_cmd);
  }

  /*
   * Create notification_wait for the TIME_EVENT_NOTIFICATION to use
   * right after we send the time event
   */
  iwl_init_notification_wait(&mvm->notif_wait, &wait_te_notif, te_notif_response,
                             ARRAY_SIZE(te_notif_response), iwl_mvm_te_notif, te_data);

  /* If TE was sent OK - wait for the notification that started */
  zx_status_t ret;
  if (ZX_OK != (ret = iwl_mvm_time_event_send_add(mvmvif, te_data, &time_cmd))) {
    IWL_ERR(mvm, "Failed to add TE to protect session: %s\n", zx_status_get_string(ret));
    iwl_remove_notification(&mvm->notif_wait, &wait_te_notif);
  } else if (ZX_OK != (ret = iwl_wait_notification(&mvm->notif_wait, &wait_te_notif,
                                                   TU_TO_DURATION(max_delay)))) {
    IWL_ERR(mvm, "Failed to protect session until TE: %s\n", zx_status_get_string(ret));
    ret = ZX_OK;  // TODO(51671): remove this line after fake TIME_EVENT_NOTIFICATION is implemented
  }

  return ret;
}

// After this function is executed, the time event is guaranteed to be removed -- even in the case
// of returning false.
//
static bool __iwl_mvm_remove_time_event(struct iwl_mvm* mvm,
                                        struct iwl_mvm_time_event_data* te_data, uint32_t* uid) {
  uint32_t id;

  /*
   * It is possible that by the time we got to this point the time
   * event was already removed.
   */
  mtx_lock(&mvm->time_event_lock);

  /* Save time event uid before clearing its data */
  *uid = te_data->uid;
  id = te_data->id;

  /*
   * The clear_data function handles time events that were already removed
   */
  iwl_mvm_te_clear_data(mvm, te_data);
  mtx_unlock(&mvm->time_event_lock);

  /*
   * It is possible that by the time we try to remove it, the time event
   * has already ended and removed. In such a case there is no need to
   * send a removal command.
   */
  if (id == TE_MAX) {
    IWL_DEBUG_TE(mvm, "TE 0x%x has already ended\n", *uid);
    return false;
  }

  return true;
}

#if 0   // NEEDS_PORTING
/*
 * Explicit request to remove a aux roc time event. The removal of a time
 * event needs to be synchronized with the flow of a time event's end
 * notification, which also removes the time event from the op mode
 * data structures.
 */
static void iwl_mvm_remove_aux_roc_te(struct iwl_mvm *mvm,
				      struct iwl_mvm_vif *mvmvif,
				      struct iwl_mvm_time_event_data *te_data)
{
	struct iwl_hs20_roc_req aux_cmd = {};
	u16 len = sizeof(aux_cmd) - iwl_mvm_chan_info_padding(mvm);

	u32 uid;
	int ret;

	if (!__iwl_mvm_remove_time_event(mvm, te_data, &uid))
		return;

	aux_cmd.event_unique_id = cpu_to_le32(uid);
	aux_cmd.action = cpu_to_le32(FW_CTXT_ACTION_REMOVE);
	aux_cmd.id_and_color =
		cpu_to_le32(FW_CMD_ID_AND_COLOR(mvmvif->id, mvmvif->color));
	IWL_DEBUG_TE(mvm, "Removing BSS AUX ROC TE 0x%x\n",
		     le32_to_cpu(aux_cmd.event_unique_id));
	ret = iwl_mvm_send_cmd_pdu(mvm, HOT_SPOT_CMD, 0,
				   len, &aux_cmd);

	if (WARN_ON(ret))
		return;
}
#endif  // NEEDS_PORTING

/*
 * Explicit request to remove a time event. The removal of a time event needs to
 * be synchronized with the flow of a time event's end notification, which also
 * removes the time event from the op mode data structures.
 */
zx_status_t iwl_mvm_remove_time_event(struct iwl_mvm_vif* mvmvif,
                                      struct iwl_mvm_time_event_data* te_data) {
  struct iwl_time_event_cmd time_cmd = {};
  uint32_t uid;

  if (!__iwl_mvm_remove_time_event(mvmvif->mvm, te_data, &uid)) {
    // The time event had been removed. No need to send another command to remove it.
    return ZX_OK;
  }

  /* When we remove a TE, the UID is to be set in the id field */
  time_cmd.id = cpu_to_le32(uid);
  time_cmd.action = cpu_to_le32(FW_CTXT_ACTION_REMOVE);
  time_cmd.id_and_color = cpu_to_le32(FW_CMD_ID_AND_COLOR(mvmvif->id, mvmvif->color));

  IWL_DEBUG_TE(mvmvif->mvm, "Removing TE 0x%x\n", le32_to_cpu(time_cmd.id));
  return iwl_mvm_send_cmd_pdu(mvmvif->mvm, TIME_EVENT_CMD, 0, sizeof(time_cmd), &time_cmd);
}

zx_status_t iwl_mvm_stop_session_protection(struct iwl_mvm_vif* mvmvif) {
  struct iwl_mvm_time_event_data* te_data = &mvmvif->time_event_data;
  uint32_t id;

  iwl_assert_lock_held(&mvmvif->mvm->mutex);

  mtx_lock(&mvmvif->mvm->time_event_lock);
  id = te_data->id;
  mtx_unlock(&mvmvif->mvm->time_event_lock);

  if (id != TE_BSS_STA_AGGRESSIVE_ASSOC) {
    IWL_DEBUG_TE(mvmvif->mvm, "don't remove TE with id=%u (not session protection)\n", id);
    return ZX_ERR_BAD_STATE;
  }

  return iwl_mvm_remove_time_event(mvmvif, te_data);
}

#if 0   // NEEDS_PORTING
int iwl_mvm_start_p2p_roc(struct iwl_mvm* mvm, struct ieee80211_vif* vif, int duration,
                          enum ieee80211_roc_type type) {
  struct iwl_mvm_vif* mvmvif = iwl_mvm_vif_from_mac80211(vif);
  struct iwl_mvm_time_event_data* te_data = &mvmvif->time_event_data;
  struct iwl_time_event_cmd time_cmd = {};

  iwl_assert_lock_held(&mvm->mutex);
  if (te_data->running) {
    IWL_WARN(mvm, "P2P_DEVICE remain on channel already running\n");
    return -EBUSY;
  }

	if (fw_has_capa(&mvm->fw->ucode_capa,
			IWL_UCODE_TLV_CAPA_SESSION_PROT_CMD))
		return iwl_mvm_start_p2p_roc_session_protection(mvm, vif,
								duration,
								type);

  time_cmd.action = cpu_to_le32(FW_CTXT_ACTION_ADD);
  time_cmd.id_and_color = cpu_to_le32(FW_CMD_ID_AND_COLOR(mvmvif->id, mvmvif->color));

  switch (type) {
    case IEEE80211_ROC_TYPE_NORMAL:
      time_cmd.id = cpu_to_le32(IWL_MVM_ROC_TE_TYPE_NORMAL);
      break;
    case IEEE80211_ROC_TYPE_MGMT_TX:
      time_cmd.id = cpu_to_le32(IWL_MVM_ROC_TE_TYPE_MGMT_TX);
      break;
    default:
      WARN_ONCE(1, "Got an invalid ROC type\n");
      return -EINVAL;
  }

  time_cmd.apply_time = cpu_to_le32(0);
  time_cmd.interval = cpu_to_le32(1);

  /*
   * The P2P Device TEs can have lower priority than other events
   * that are being scheduled by the driver/fw, and thus it might not be
   * scheduled. To improve the chances of it being scheduled, allow them
   * to be fragmented, and in addition allow them to be delayed.
   */
  time_cmd.max_frags = min(MSEC_TO_TU(duration) / 50, TE_V2_FRAG_ENDLESS);
  time_cmd.max_delay = cpu_to_le32(MSEC_TO_TU(duration / 2));
  time_cmd.duration = cpu_to_le32(MSEC_TO_TU(duration));
  time_cmd.repeat = 1;
  time_cmd.policy = cpu_to_le16(TE_V2_NOTIF_HOST_EVENT_START | TE_V2_NOTIF_HOST_EVENT_END |
                                TE_V2_START_IMMEDIATELY);

  return iwl_mvm_time_event_send_add(mvm, vif, te_data, &time_cmd);
}

static struct iwl_mvm_time_event_data* iwl_mvm_get_roc_te(struct iwl_mvm* mvm) {
  struct iwl_mvm_time_event_data* te_data;

  iwl_assert_lock_held(&mvm->mutex);

  spin_lock_bh(&mvm->time_event_lock);

  /*
   * Iterate over the list of time events and find the time event that is
   * associated with a P2P_DEVICE interface.
   * This assumes that a P2P_DEVICE interface can have only a single time
   * event at any given time and this time event coresponds to a ROC
   * request
   */
  list_for_each_entry(te_data, &mvm->time_event_list, list) {
    if (te_data->vif->type == NL80211_IFTYPE_P2P_DEVICE) {
      goto out;
    }
  }

  /* There can only be at most one AUX ROC time event, we just use the
   * list to simplify/unify code. Remove it if it exists.
   */
  te_data = list_first_entry_or_null(&mvm->aux_roc_te_list, struct iwl_mvm_time_event_data, list);
out:
  spin_unlock_bh(&mvm->time_event_lock);
  return te_data;
}

void iwl_mvm_cleanup_roc_te(struct iwl_mvm* mvm) {
  struct iwl_mvm_time_event_data* te_data;
  uint32_t uid;

  te_data = iwl_mvm_get_roc_te(mvm);
  if (te_data) {
    __iwl_mvm_remove_time_event(mvm, te_data, &uid);
  }
}

void iwl_mvm_stop_roc(struct iwl_mvm* mvm) {
  struct iwl_mvm_vif* mvmvif;
  struct iwl_mvm_time_event_data* te_data;

	if (fw_has_capa(&mvm->fw->ucode_capa,
			IWL_UCODE_TLV_CAPA_SESSION_PROT_CMD)) {
		mvmvif = iwl_mvm_vif_from_mac80211(vif);

		if (vif->type == NL80211_IFTYPE_P2P_DEVICE) {
			iwl_mvm_cancel_session_protection(mvm, mvmvif,
							  mvmvif->time_event_data.id);
			iwl_mvm_p2p_roc_finished(mvm);
		} else {
			iwl_mvm_remove_aux_roc_te(mvm, mvmvif,
						  &mvmvif->hs_time_event_data);
			iwl_mvm_roc_finished(mvm);
		}

		return;
	}

  te_data = iwl_mvm_get_roc_te(mvm);
  if (!te_data) {
    IWL_WARN(mvm, "No remain on channel event\n");
    return;
  }

  mvmvif = iwl_mvm_vif_from_mac80211(te_data->vif);

  if (te_data->vif->type == NL80211_IFTYPE_P2P_DEVICE) {
    iwl_mvm_remove_time_event(mvm, mvmvif, te_data);
		iwl_mvm_p2p_roc_finished(mvm);
  } else {
    iwl_mvm_remove_aux_roc_te(mvm, mvmvif, te_data);
		iwl_mvm_roc_finished(mvm);
	}
}

int iwl_mvm_schedule_csa_period(struct iwl_mvm* mvm, struct ieee80211_vif* vif, uint32_t duration,
                                uint32_t apply_time) {
  struct iwl_mvm_vif* mvmvif = iwl_mvm_vif_from_mac80211(vif);
  struct iwl_mvm_time_event_data* te_data = &mvmvif->time_event_data;
  struct iwl_time_event_cmd time_cmd = {};

  iwl_assert_lock_held(&mvm->mutex);

  if (te_data->running) {
    uint32_t id;

    spin_lock_bh(&mvm->time_event_lock);
    id = te_data->id;
    spin_unlock_bh(&mvm->time_event_lock);

    if (id == TE_CHANNEL_SWITCH_PERIOD) {
      IWL_DEBUG_TE(mvm, "CS period is already scheduled\n");
      return -EBUSY;
    }

    /*
     * Remove the session protection time event to allow the
     * channel switch. If we got here, we just heard a beacon so
     * the session protection is not needed anymore anyway.
     */
    iwl_mvm_remove_time_event(mvm, mvmvif, te_data);
  }

  time_cmd.action = cpu_to_le32(FW_CTXT_ACTION_ADD);
  time_cmd.id_and_color = cpu_to_le32(FW_CMD_ID_AND_COLOR(mvmvif->id, mvmvif->color));
  time_cmd.id = cpu_to_le32(TE_CHANNEL_SWITCH_PERIOD);
  time_cmd.apply_time = cpu_to_le32(apply_time);
  time_cmd.max_frags = TE_V2_FRAG_NONE;
  time_cmd.duration = cpu_to_le32(duration);
  time_cmd.repeat = 1;
  time_cmd.interval = cpu_to_le32(1);
  time_cmd.policy = cpu_to_le16(TE_V2_NOTIF_HOST_EVENT_START | TE_V2_ABSENCE);
  if (!apply_time) {
    time_cmd.policy |= cpu_to_le16(TE_V2_START_IMMEDIATELY);
  }

  return iwl_mvm_time_event_send_add(mvm, vif, te_data, &time_cmd);
}

static bool iwl_mvm_session_prot_notif(struct iwl_notif_wait_data *notif_wait,
				       struct iwl_rx_packet *pkt, void *data)
{
	struct iwl_mvm *mvm =
		container_of(notif_wait, struct iwl_mvm, notif_wait);
	struct iwl_mvm_session_prot_notif *resp;
	int resp_len = iwl_rx_packet_payload_len(pkt);

	if (WARN_ON(pkt->hdr.cmd != SESSION_PROTECTION_NOTIF ||
		    pkt->hdr.group_id != MAC_CONF_GROUP))
		return true;

	if (WARN_ON_ONCE(resp_len != sizeof(*resp))) {
		IWL_ERR(mvm, "Invalid SESSION_PROTECTION_NOTIF response\n");
		return true;
	}

	resp = (void *)pkt->data;

	if (!resp->status)
		IWL_ERR(mvm,
			"TIME_EVENT_NOTIFICATION received but not executed\n");

	return true;
}

void iwl_mvm_schedule_session_protection(struct iwl_mvm *mvm,
					 struct ieee80211_vif *vif,
					 u32 duration, u32 min_duration,
					 bool wait_for_notif)
{
	struct iwl_mvm_vif *mvmvif = iwl_mvm_vif_from_mac80211(vif);
	struct iwl_mvm_time_event_data *te_data = &mvmvif->time_event_data;
	const u16 notif[] = { WIDE_ID(MAC_CONF_GROUP, SESSION_PROTECTION_NOTIF) };
	struct iwl_notification_wait wait_notif;
	struct iwl_mvm_session_prot_cmd cmd = {
		.id_and_color =
			cpu_to_le32(FW_CMD_ID_AND_COLOR(mvmvif->id,
							mvmvif->color)),
		.action = cpu_to_le32(FW_CTXT_ACTION_ADD),
		.conf_id = cpu_to_le32(SESSION_PROTECT_CONF_ASSOC),
		.duration_tu = cpu_to_le32(MSEC_TO_TU(duration)),
	};

	lockdep_assert_held(&mvm->mutex);

	spin_lock_bh(&mvm->time_event_lock);
	if (te_data->running &&
	    time_after(te_data->end_jiffies, TU_TO_EXP_TIME(min_duration))) {
		IWL_DEBUG_TE(mvm, "We have enough time in the current TE: %u\n",
			     jiffies_to_msecs(te_data->end_jiffies - jiffies));
		spin_unlock_bh(&mvm->time_event_lock);

		return;
	}

	iwl_mvm_te_clear_data(mvm, te_data);
	/*
	 * The time_event_data.id field is reused to save session
	 * protection's configuration.
	 */
	te_data->id = le32_to_cpu(cmd.conf_id);
	te_data->duration = le32_to_cpu(cmd.duration_tu);
	te_data->vif = vif;
	spin_unlock_bh(&mvm->time_event_lock);

	IWL_DEBUG_TE(mvm, "Add new session protection, duration %d TU\n",
		     le32_to_cpu(cmd.duration_tu));

	if (!wait_for_notif) {
		if (iwl_mvm_send_cmd_pdu(mvm,
					 WIDE_ID(MAC_CONF_GROUP, SESSION_PROTECTION_CMD),
					 0, sizeof(cmd), &cmd)) {
			IWL_ERR(mvm,
				"Couldn't send the SESSION_PROTECTION_CMD\n");
			spin_lock_bh(&mvm->time_event_lock);
			iwl_mvm_te_clear_data(mvm, te_data);
			spin_unlock_bh(&mvm->time_event_lock);
		}

		return;
	}

	iwl_init_notification_wait(&mvm->notif_wait, &wait_notif,
				   notif, ARRAY_SIZE(notif),
				   iwl_mvm_session_prot_notif, NULL);

	if (iwl_mvm_send_cmd_pdu(mvm,
				 WIDE_ID(MAC_CONF_GROUP, SESSION_PROTECTION_CMD),
				 0, sizeof(cmd), &cmd)) {
		IWL_ERR(mvm,
			"Couldn't send the SESSION_PROTECTION_CMD\n");
		iwl_remove_notification(&mvm->notif_wait, &wait_notif);
	} else if (iwl_wait_notification(&mvm->notif_wait, &wait_notif,
					 TU_TO_JIFFIES(100))) {
		IWL_ERR(mvm,
			"Failed to protect session until session protection\n");
	}
}
#endif  // NEEDS_PORTING
