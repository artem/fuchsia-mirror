// Copyright (c) 2022 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_SOFTAP_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_SOFTAP_H_

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.ieee80211/cpp/wire_types.h>
#include <lib/sync/completion.h>
#include <netinet/if_ether.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <mutex>
#include <unordered_set>

#include <wlan/common/macaddr.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/event_handler.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_request.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/mlan.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/mlan/mlan_ieee.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/waitable_state.h"

namespace wlan_fullmac_wire = fuchsia_wlan_fullmac::wire;
namespace wlan_ieee80211_wire = fuchsia_wlan_ieee80211::wire;
namespace wlan::nxpfmac {

struct DeviceContext;

class SoftApIfc {
 public:
  virtual ~SoftApIfc() = default;

  virtual void OnStaConnectEvent(uint8_t* sta_mac_addr, uint8_t* ies, uint32_t ie_len) = 0;
  virtual void OnStaDisconnectEvent(uint8_t* sta_mac_addr, uint16_t reason_code,
                                    bool locally_initiated = false) = 0;
};

class SoftAp {
 public:
  using StatusCode = fuchsia_wlan_ieee80211::wire::StatusCode;

  SoftAp(SoftApIfc* ifc, DeviceContext* context, uint32_t bss_index);
  ~SoftAp();
  // Attempt to start the SoftAP on the given bss and channel. Returns appropriate
  // WlanStartResult.
  wlan_fullmac_wire::WlanStartResult Start(
      const fuchsia_wlan_fullmac::wire::WlanFullmacImplBaseStartBssRequest* req)
      __TA_EXCLUDES(mutex_);

  // Returns appropriate WlanStopResult.
  wlan_fullmac_wire::WlanStopResult Stop(const fuchsia_wlan_ieee80211::wire::CSsid* ssid)
      __TA_EXCLUDES(mutex_);
  zx_status_t DeauthSta(const uint8_t sta_mac_addr[ETH_ALEN], uint16_t reason_code)
      __TA_EXCLUDES(mutex_);

 private:
  SoftApIfc* ifc_ = nullptr;
  void OnStaConnect(pmlan_event event) __TA_EXCLUDES(mutex_);
  void OnStaDisconnect(pmlan_event event) __TA_EXCLUDES(mutex_);
  DeviceContext* context_ = nullptr;
  const uint32_t bss_index_;
  fuchsia_wlan_ieee80211::wire::CSsid ssid_ = {};
  bool started_ __TA_GUARDED(mutex_) = false;
  std::unordered_set<wlan::common::MacAddr, wlan::common::MacAddrHasher> deauth_sta_set_;
  std::mutex mutex_;
  sync_completion_t deauth_sync_;
  sync_completion_t softap_start_sync_;
  EventRegistration client_connect_event_;
  EventRegistration client_disconnect_event_;
  EventRegistration softap_start_;
};

}  // namespace wlan::nxpfmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_SOFTAP_H_
