// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <functional>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

namespace wlan::brcmfmac {

class SetMulticastPromiscTest : public SimTest {
 protected:
  void VerifySetAllmulti(SimInterface& ifc, bool enable) {
    EXPECT_EQ(ifc.SetMulticastPromisc(enable), ZX_OK);

    WithSimDevice([&](brcmfmac::SimDevice* device) {
      brcmf_simdev* sim = device->GetSim();
      struct brcmf_if* ifp = brcmf_get_ifp(sim->drvr, ifc.iface_id_);

      uint32_t allmulti_value;
      zx_status_t status = brcmf_fil_iovar_int_get(ifp, "allmulti", &allmulti_value, nullptr);
      EXPECT_EQ(status, ZX_OK);

      EXPECT_EQ(allmulti_value, (uint32_t)enable);
    });
  }
};

// Verify that "allmulti" is set when brcmf_if_set_multicast_promisc() is called for both ifaces.
TEST_F(SetMulticastPromiscTest, SetMulticastPromisc) {
  ASSERT_EQ(Init(), ZX_OK);

  SimInterface client_ifc;
  SimInterface ap_ifc;
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc), ZX_OK);
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kAp, &ap_ifc), ZX_OK);

  VerifySetAllmulti(client_ifc, true);
  VerifySetAllmulti(client_ifc, false);
  VerifySetAllmulti(ap_ifc, true);
  VerifySetAllmulti(ap_ifc, false);

  EXPECT_EQ(DeleteInterface(&client_ifc), ZX_OK);
  EXPECT_EQ(DeleteInterface(&ap_ifc), ZX_OK);
}

}  // namespace wlan::brcmfmac
