// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "zircon/errors.h"

namespace wlan::brcmfmac {
namespace {
fidl::Array<uint8_t, 3> kIeeeOui = {0x00, 0x0F, 0xAC};
}  // namespace

class SetKeysTest : public SimTest {
 public:
  SetKeysTest() = default;
  void SetUp() override {
    ASSERT_EQ(ZX_OK, SimTest::Init());
    ASSERT_EQ(ZX_OK, StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_));
  }
  void TearDown() override { EXPECT_EQ(SimTest::DeleteInterface(&client_ifc_), ZX_OK); }

  SimInterface client_ifc_;
};

wlan_fullmac_wire::WlanFullmacSetKeysReq FakeSetKeysRequest(
    const uint8_t keys[][wlan_ieee80211::kMaxKeyLen], size_t n, fidl::AnyArena& arena) {
  wlan_fullmac_wire::WlanFullmacSetKeysReq set_keys_req{.num_keys = n};

  for (size_t i = 0; i < n; i++) {
    set_keys_req.keylist[i] =
        fuchsia_wlan_common::wire::WlanKeyConfig::Builder(arena)
            .key(fidl::VectorView<uint8_t>::FromExternal(
                const_cast<uint8_t*>(keys[i]), strlen(reinterpret_cast<const char*>(keys[i]))))
            .key_idx(static_cast<uint8_t>(i))
            .key_type(fuchsia_wlan_common::wire::WlanKeyType::kPairwise)
            .cipher_type(wlan_ieee80211::CipherSuiteType::kCcmp128)
            .cipher_oui(kIeeeOui)
            .rsc({})
            .peer_addr({})
            .protection({})
            .Build();
  }
  return set_keys_req;
}

TEST_F(SetKeysTest, MultipleKeys) {
  const uint8_t keys[wlan_fullmac_wire::kWlanMaxKeylistSize][wlan_ieee80211::kMaxKeyLen] = {
      "One", "Two", "Three", "Four"};
  wlan_fullmac_wire::WlanFullmacSetKeysReq set_keys_req =
      FakeSetKeysRequest(keys, wlan_fullmac_wire::kWlanMaxKeylistSize, client_ifc_.test_arena_);
  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->SetKeysReq(set_keys_req);
  EXPECT_TRUE(result.ok());

  auto& set_keys_resp = result->resp;

  std::vector<brcmf_wsec_key_le> firmware_keys =
      device_->GetSim()->sim_fw->GetKeyList(client_ifc_.iface_id_);
  ASSERT_EQ(firmware_keys.size(), wlan_fullmac_wire::kWlanMaxKeylistSize);
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[0].data), "One");
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[1].data), "Two");
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[2].data), "Three");
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[3].data), "Four");
  ASSERT_EQ(set_keys_resp.num_keys, wlan_fullmac_wire::kWlanMaxKeylistSize);
  for (auto status : set_keys_resp.statuslist.data_) {
    ASSERT_EQ(status, ZX_OK);
  }
}

// Ensure that group key is set correctly by the driver in firmware.
TEST_F(SetKeysTest, SetGroupKey) {
  const uint8_t group_key[wlan_ieee80211::kMaxKeyLen] = "Group Key";
  const uint8_t ucast_key[wlan_ieee80211::kMaxKeyLen] = "My Key";
  const size_t kKeyNum = 2;

  fidl::Array<fuchsia_wlan_common::wire::WlanKeyConfig, wlan_fullmac_wire::kWlanMaxKeylistSize>
      key_list;
  key_list[0] =
      fuchsia_wlan_common::wire::WlanKeyConfig::Builder(client_ifc_.test_arena_)
          .key(fidl::VectorView<uint8_t>::FromExternal(
              const_cast<uint8_t*>(group_key), strlen(reinterpret_cast<const char*>(group_key))))
          .key_idx(static_cast<uint8_t>(0))
          .key_type(fuchsia_wlan_common::wire::WlanKeyType::kGroup)
          .cipher_type(wlan_ieee80211::CipherSuiteType::kCcmp128)
          .cipher_oui(kIeeeOui)
          .rsc({})
          .peer_addr({0xff, 0xff, 0xff, 0xff, 0xff, 0xff})
          .protection({})
          .Build();

  key_list[1] =
      fuchsia_wlan_common::wire::WlanKeyConfig::Builder(client_ifc_.test_arena_)
          .key(fidl::VectorView<uint8_t>::FromExternal(
              const_cast<uint8_t*>(ucast_key), strlen(reinterpret_cast<const char*>(ucast_key))))
          .key_idx(static_cast<uint8_t>(1))
          .key_type(fuchsia_wlan_common::wire::WlanKeyType::kPairwise)
          .cipher_type(wlan_ieee80211::CipherSuiteType::kCcmp128)
          .cipher_oui(kIeeeOui)
          .rsc({})
          .peer_addr({0xde, 0xad, 0xbe, 0xef, 0xab, 0xcd})
          .protection({})
          .Build();

  wlan_fullmac_wire::WlanFullmacSetKeysReq key_req = {
      .num_keys = kKeyNum,
      .keylist = {key_list},
  };

  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->SetKeysReq(key_req);
  EXPECT_TRUE(result.ok());

  auto& set_keys_resp = result->resp;
  ASSERT_EQ(set_keys_resp.num_keys, 2ul);
  ASSERT_EQ(set_keys_resp.statuslist.data()[0], ZX_OK);
  ASSERT_EQ(set_keys_resp.statuslist.data()[1], ZX_OK);

  std::vector<brcmf_wsec_key_le> firmware_keys =
      device_->GetSim()->sim_fw->GetKeyList(client_ifc_.iface_id_);
  ASSERT_EQ(firmware_keys.size(), 2U);
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[0].data),
               reinterpret_cast<const char*>(group_key));
  // Group key should have been set as the PRIMARY KEY
  ASSERT_EQ(firmware_keys[0].flags, (const unsigned int)BRCMF_PRIMARY_KEY);
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[1].data),
               reinterpret_cast<const char*>(ucast_key));
  ASSERT_NE(firmware_keys[1].flags, (const unsigned int)BRCMF_PRIMARY_KEY);
}

TEST_F(SetKeysTest, CustomOuiNotSupported) {
  const uint8_t key[wlan_ieee80211::kMaxKeyLen] = "My Key";

  fidl::Array<uint8_t, 3> kCustomOui = {1, 2, 3};

  fidl::Array<fuchsia_wlan_common::wire::WlanKeyConfig, wlan_fullmac_wire::kWlanMaxKeylistSize>
      key_list;
  key_list[0] = fuchsia_wlan_common::wire::WlanKeyConfig::Builder(client_ifc_.test_arena_)
                    .key(fidl::VectorView<uint8_t>::FromExternal(
                        const_cast<uint8_t*>(key), strlen(reinterpret_cast<const char*>(key))))
                    .key_idx(static_cast<uint8_t>(0))
                    .key_type(fuchsia_wlan_common::wire::WlanKeyType::kPairwise)
                    .cipher_type(wlan_ieee80211::CipherSuiteType::kCcmp128)
                    .cipher_oui(kCustomOui)
                    .rsc({})
                    .peer_addr({0xde, 0xad, 0xbe, 0xef, 0xab, 0xcd})
                    .protection({})
                    .Build();

  wlan_fullmac_wire::WlanFullmacSetKeysReq key_req = {
      .num_keys = 1,
      .keylist = {key_list},
  };

  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->SetKeysReq(key_req);
  EXPECT_TRUE(result.ok());

  auto& set_keys_resp = result->resp;
  ASSERT_EQ(set_keys_resp.num_keys, 1ul);
  ASSERT_EQ(set_keys_resp.statuslist.data()[0], ZX_ERR_NOT_SUPPORTED);

  std::vector<brcmf_wsec_key_le> firmware_keys =
      device_->GetSim()->sim_fw->GetKeyList(client_ifc_.iface_id_);
  ASSERT_EQ(firmware_keys.size(), 0U);
}

TEST_F(SetKeysTest, OptionalOuiSupported) {
  const uint8_t key[wlan_ieee80211::kMaxKeyLen] = "My Key";

  fidl::Array<fuchsia_wlan_common::wire::WlanKeyConfig, wlan_fullmac_wire::kWlanMaxKeylistSize>
      key_list;
  key_list[0] = fuchsia_wlan_common::wire::WlanKeyConfig::Builder(client_ifc_.test_arena_)
                    .key(fidl::VectorView<uint8_t>::FromExternal(
                        const_cast<uint8_t*>(key), strlen(reinterpret_cast<const char*>(key))))
                    .key_idx(static_cast<uint8_t>(0))
                    .key_type(fuchsia_wlan_common::wire::WlanKeyType::kPairwise)
                    .cipher_type(wlan_ieee80211::CipherSuiteType::kCcmp128)
                    .rsc({})
                    .peer_addr({0xde, 0xad, 0xbe, 0xef, 0xab, 0xcd})
                    .protection({})
                    .Build();

  wlan_fullmac_wire::WlanFullmacSetKeysReq key_req = {
      .num_keys = 1,
      .keylist = {key_list},
  };

  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->SetKeysReq(key_req);
  EXPECT_TRUE(result.ok());

  auto& set_keys_resp = result->resp;
  ASSERT_EQ(set_keys_resp.num_keys, 1ul);
  ASSERT_EQ(set_keys_resp.statuslist.data()[0], ZX_OK);

  std::vector<brcmf_wsec_key_le> firmware_keys =
      device_->GetSim()->sim_fw->GetKeyList(client_ifc_.iface_id_);
  ASSERT_EQ(firmware_keys.size(), 1U);

  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[0].data),
               reinterpret_cast<const char*>(key));
}

}  // namespace wlan::brcmfmac
