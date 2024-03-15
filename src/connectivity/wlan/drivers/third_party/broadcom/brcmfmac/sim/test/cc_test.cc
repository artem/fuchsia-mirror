// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <zircon/errors.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/common.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

namespace wlan::brcmfmac {

class CountryCodeTest : public SimTest {
 public:
  CountryCodeTest() = default;

  void SetUp() override {
    Init();
    CreateInterface();
  }

  void Init();
  void CreateInterface();
  void DeleteInterface();
  zx_status_t SetCountryCode(const fuchsia_wlan_phyimpl::wire::WlanPhyCountry* country);
  void GetCountryCodeFromFirmware(brcmf_fil_country_le* ccode);
  zx_status_t SetCountryCodeInFirmware(const fuchsia_wlan_phyimpl_wire::WlanPhyCountry* country);
  zx_status_t ClearCountryCode();

 private:
  SimInterface client_ifc_;
};

void CountryCodeTest::Init() { ASSERT_EQ(SimTest::Init(), ZX_OK); }

void CountryCodeTest::CreateInterface() {
  zx_status_t status;

  status = StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_);
  ASSERT_EQ(status, ZX_OK);
}

void CountryCodeTest::DeleteInterface() {
  EXPECT_EQ(SimTest::DeleteInterface(&client_ifc_), ZX_OK);
}

zx_status_t CountryCodeTest::SetCountryCode(
    const fuchsia_wlan_phyimpl::wire::WlanPhyCountry* country) {
  auto result = client_.buffer(test_arena_)->SetCountry(*country);
  EXPECT_TRUE(result.ok());
  if (result->is_error()) {
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t CountryCodeTest::ClearCountryCode() {
  auto result = client_.buffer(test_arena_)->ClearCountry();
  EXPECT_TRUE(result.ok());
  if (result->is_error()) {
    return result->error_value();
  }
  return ZX_OK;
}

// Note that this function is meant for SIM only. It retrieves the internal
// state of the country code setting by bypassing the interfaces.
void CountryCodeTest::GetCountryCodeFromFirmware(brcmf_fil_country_le* ccode) {
  WithSimDevice([&](brcmfmac::SimDevice* device) {
    brcmf_simdev* sim = device->GetSim();
    struct brcmf_if* ifp = brcmf_get_ifp(sim->drvr, client_ifc_.iface_id_);
    zx_status_t status =
        brcmf_fil_iovar_data_get(ifp, "country", ccode, sizeof(brcmf_fil_country_le), nullptr);
    EXPECT_EQ(status, ZX_OK);
  });
}

zx_status_t CountryCodeTest::SetCountryCodeInFirmware(
    const fuchsia_wlan_phyimpl_wire::WlanPhyCountry* country) {
  EXPECT_NE(country, nullptr);
  zx_status_t status = ZX_ERR_INTERNAL;
  WithSimDevice([&](brcmfmac::SimDevice* device) {
    brcmf_simdev* sim = device->GetSim();
    status = brcmf_set_country(sim->drvr, country);
  });

  return status;
}

TEST_F(CountryCodeTest, SetDefault) { DeleteInterface(); }

TEST_F(CountryCodeTest, SetCCode) {
  const auto valid_country = fuchsia_wlan_phyimpl::wire::WlanPhyCountry::WithAlpha2({'U', 'S'});
  const auto invalid_country = fuchsia_wlan_phyimpl::wire::WlanPhyCountry::WithAlpha2({'X', 'X'});
  struct brcmf_fil_country_le country_code;
  zx_status_t status;
  uint8_t code;

  // Get the country code and verify that it is set to WW.
  GetCountryCodeFromFirmware(&country_code);
  code = memcmp(country_code.ccode, "WW", fuchsia_wlan_phyimpl_wire::kWlanphyAlpha2Len);
  ASSERT_EQ(code, 0);

  // Set an invalid CC and verify it fails
  status = SetCountryCode(&invalid_country);
  ASSERT_NE(status, ZX_OK);

  // Verify that it stays with the default
  GetCountryCodeFromFirmware(&country_code);
  code = memcmp(country_code.ccode, "WW", fuchsia_wlan_phyimpl_wire::kWlanphyAlpha2Len);
  ASSERT_EQ(code, 0);
  // Set a valid CC and verify it succeeds
  status = SetCountryCode(&valid_country);
  ASSERT_EQ(status, ZX_OK);
  GetCountryCodeFromFirmware(&country_code);
  code = memcmp(&valid_country.alpha2(), country_code.ccode,
                fuchsia_wlan_phyimpl_wire::kWlanphyAlpha2Len);
  ASSERT_EQ(code, 0);
}

TEST_F(CountryCodeTest, GetCCode) {
  {
    const auto country = fuchsia_wlan_phyimpl_wire::WlanPhyCountry::WithAlpha2({'W', 'W'});
    ASSERT_EQ(ZX_OK, SetCountryCodeInFirmware(&country));
    auto result = client_.buffer(test_arena_)->GetCountry();
    EXPECT_TRUE(result.ok());
    ASSERT_FALSE(result->is_error());
    auto& get_country_result = result->value();
    EXPECT_EQ(get_country_result->alpha2().data()[0], 'W');
    EXPECT_EQ(get_country_result->alpha2().data()[1], 'W');
  }

  // Try again, just in case the first one was a default value.
  {
    const auto country = fuchsia_wlan_phyimpl_wire::WlanPhyCountry::WithAlpha2({'U', 'S'});
    ASSERT_EQ(ZX_OK, SetCountryCodeInFirmware(&country));
    auto result = client_.buffer(test_arena_)->GetCountry();
    EXPECT_TRUE(result.ok());
    ASSERT_FALSE(result->is_error());
    auto& get_country_result = result->value();
    EXPECT_EQ(get_country_result->alpha2().data()[0], 'U');
    EXPECT_EQ(get_country_result->alpha2().data()[1], 'S');
  }
}

TEST_F(CountryCodeTest, ClearCCode) {
  const auto world_safe_country = fuchsia_wlan_phyimpl_wire::WlanPhyCountry::WithAlpha2({'W', 'W'});
  struct brcmf_fil_country_le country_code;
  zx_status_t status;
  uint8_t code;

  status = ClearCountryCode();
  ASSERT_EQ(status, ZX_OK);
  GetCountryCodeFromFirmware(&country_code);
  code = memcmp(world_safe_country.alpha2().data(), country_code.ccode,
                fuchsia_wlan_phyimpl_wire::kWlanphyAlpha2Len);
  ASSERT_EQ(code, 0);
}

}  // namespace wlan::brcmfmac
