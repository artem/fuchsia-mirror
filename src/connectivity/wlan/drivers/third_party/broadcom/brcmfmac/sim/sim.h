/*
 * Copyright (c) 2019 The Fuchsia Authors
 *
 * Permission to use, copy, modify, and/or distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
 * SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
 * OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
 * CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_H_

#include <memory>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_fw.h"

struct brcmf_bus;

struct brcmf_simdev {
  std::unique_ptr<::wlan::brcmfmac::SimFirmware> sim_fw;
  std::unique_ptr<brcmf_mp_device> settings;
  ::wlan::simulation::Environment* env;
  brcmf_pub* drvr;
};

// Allocate device and bus structures
std::unique_ptr<brcmf_bus> brcmf_sim_alloc(brcmf_pub* drvr, ::wlan::simulation::Environment* env);

// Perform initialization on the appropriate bus structures
zx_status_t brcmf_sim_register(brcmf_pub* drvr);

// Pass an event to the driver from the simulated firmware
void brcmf_sim_rx_event(brcmf_simdev* simdev, std::shared_ptr<std::vector<uint8_t>> buffer);
// Pass a frame to the driver from the simulated firmware
void brcmf_sim_rx_frame(brcmf_simdev* simdev, std::shared_ptr<std::vector<uint8_t>> buffer);
// Simulator cleanup
void brcmf_sim_exit(brcmf_bus* bus);
// Conduct bus layer recovery in SIM
zx_status_t brcmf_sim_recovery(brcmf_bus* bus);
// Simluates the firmware crash and notify driver through RecoveryTrigger
void brcmf_sim_firmware_crash(brcmf_simdev* simdev);

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_H_
