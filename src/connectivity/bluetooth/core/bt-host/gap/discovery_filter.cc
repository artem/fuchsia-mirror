// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/discovery_filter.h"

#include <endian.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/advertising_data.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/assert.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"

namespace bt::gap {

void DiscoveryFilter::SetGeneralDiscoveryFlags() {
  set_flags(static_cast<uint8_t>(AdvFlag::kLEGeneralDiscoverableMode) |
            static_cast<uint8_t>(AdvFlag::kLELimitedDiscoverableMode));
}

bool DiscoveryFilter::MatchLowEnergyResult(
    const std::optional<std::reference_wrapper<const AdvertisingData>>
        advertising_data,
    bool connectable,
    int8_t rssi) const {
  // No need to check |advertising_data| for the |connectable_| filter.
  if (connectable_ && *connectable_ != connectable) {
    return false;
  }

  // If a pathloss filter is not set then apply the RSSI filter before
  // checking |advertising_data|. (An RSSI value of hci_spec::kRSSIInvalid means
  // that RSSI is not available, which we check for here).
  bool rssi_ok = !rssi_ || (rssi != hci_spec::kRSSIInvalid && rssi >= *rssi_);
  if (!pathloss_ && !rssi_ok) {
    return false;
  }

  // Any of these filters being set requires us to have a valid
  // |advertising_data| to pass.
  bool needs_ad_check = flags_ || !service_uuids_.empty() ||
                        !service_data_uuids_.empty() ||
                        !name_substring_.empty() || manufacturer_code_;

  if (!advertising_data.has_value() && needs_ad_check) {
    return false;
  }

  // Pathloss is complicated because we can pass if it's set and we have no
  // |advertising_data| by passing RSSI instead.
  if (pathloss_) {
    if (!advertising_data.has_value() ||
        !advertising_data->get().tx_power().has_value()) {
      // If no RSSI filter was set OR if one was set but it didn't match the
      // scan result, we fail.
      if (!rssi_ || !rssi_ok) {
        return false;
      }
      // Otherwise we fall back to RSSI passing if tx_power was not set.
    } else {
      int8_t tx_power_lvl = *advertising_data->get().tx_power();
      if (tx_power_lvl < rssi) {
        bt_log(WARN,
               "gap",
               "reported tx-power level is less than RSSI, failed pathloss");
        return false;
      }
      int8_t pathloss = tx_power_lvl - rssi;
      if (pathloss > *pathloss_) {
        return false;
      }
      // mark the rssi_ok since we pass based on pathloss.
      rssi_ok = true;
    }
  }

  // If we made it here without advetising_data, and there's no need to check,
  // we pass if rssi passed (which also passes if RSSI filtering was not set)
  if (!advertising_data.has_value() && !needs_ad_check) {
    return rssi_ok;
  }

  BT_DEBUG_ASSERT(advertising_data.has_value());
  const AdvertisingData& ad = advertising_data->get();

  if (flags_) {
    if (all_flags_required_ && ad.flags() != flags_) {
      return false;
    }
    if (!ad.flags().has_value()) {
      return false;
    }
    uint8_t matched_flags = ad.flags().value() & *flags_;
    if (matched_flags == 0) {
      return false;
    }
  }

  if (!name_substring_.empty()) {
    if (!ad.local_name()) {
      return false;
    }
    // TODO(jamuraa): If this is an incomplete name should we match the first
    // part?
    if (ad.local_name()->name.find(name_substring_) == std::string_view::npos) {
      return false;
    }
  }

  if (manufacturer_code_) {
    if (ad.manufacturer_data_ids().find(*manufacturer_code_) ==
        ad.manufacturer_data_ids().end()) {
      return false;
    }
  }

  if (!service_uuids_.empty()) {
    bool service_found = false;
    const auto& ad_service_uuids = ad.service_uuids();
    for (auto uuid : service_uuids_) {
      if (ad_service_uuids.count(uuid) != 0) {
        service_found = true;
        break;
      }
    }
    if (!service_found) {
      return false;
    }
  }

  if (!service_data_uuids_.empty()) {
    bool service_data_found = false;
    const auto& ad_data_uuids = ad.service_data_uuids();
    for (auto uuid : service_data_uuids_) {
      if (ad_data_uuids.count(uuid) != 0) {
        service_data_found = true;
        break;
      }
    }
    if (!service_data_found) {
      return false;
    }
  }

  // We haven't filtered it out, so it matches.
  return true;
}

void DiscoveryFilter::Reset() {
  service_uuids_.clear();
  service_data_uuids_.clear();
  name_substring_.clear();
  connectable_.reset();
  manufacturer_code_.reset();
  pathloss_.reset();
  rssi_.reset();
}

}  // namespace bt::gap
