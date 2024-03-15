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
#include <zircon/errors.h>

#include "fidl/fuchsia.wlan.phyimpl/cpp/wire_types.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

namespace wlan::brcmfmac {
namespace {

std::pair<zx::channel, zx::channel> make_channel() {
  zx::channel local;
  zx::channel remote;
  zx::channel::create(0, &local, &remote);
  return {std::move(local), std::move(remote)};
}

class LifecycleTest : public SimTest {
  void SetUp() override { ASSERT_OK(SimTest::Init()); }
};

TEST_F(LifecycleTest, CreateIfaceInvalidArgsFails) {
  auto req =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(test_arena_).Build();
  auto result = client_.buffer(test_arena_)->CreateIface(req);

  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_error());
  ASSERT_EQ(result->error_value(), ZX_ERR_INVALID_ARGS);
}

TEST_F(LifecycleTest, CreateMultipleClientIfaceFails) {
  uint16_t iface_id = 0;
  {
    // First call to create iface with client role should succeed
    auto [local, _remote] = make_channel();
    auto req = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(test_arena_)
                   .role(wlan_common::WlanMacRole::kClient)
                   .mlme_channel(std::move(local))
                   .Build();
    auto result = client_.buffer(test_arena_)->CreateIface(req);
    ASSERT_TRUE(result.ok() && !result->is_error());
    iface_id = result->value()->iface_id();
  }

  {
    // Second call to create iface with client role fails
    auto [local, _remote] = make_channel();
    auto req = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(test_arena_)
                   .role(wlan_common::WlanMacRole::kClient)
                   .mlme_channel(std::move(local))
                   .Build();
    auto result = client_.buffer(test_arena_)->CreateIface(req);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_error());
    ASSERT_EQ(result->error_value(), ZX_ERR_NO_RESOURCES);
  }

  {
    // destroy client iface manually to clean up
    auto req = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_)
                   .iface_id(iface_id)
                   .Build();

    auto result = client_.buffer(test_arena_)->DestroyIface(req);
    ASSERT_TRUE(result.ok() && !result->is_error());
  }
}

TEST_F(LifecycleTest, DestroyIfaceWithoutIfaceIdFails) {
  uint16_t iface_id = 0;
  {
    // First call to create iface with client role should succeed
    auto [local, _remote] = make_channel();
    auto req = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(test_arena_)
                   .role(wlan_common::WlanMacRole::kClient)
                   .mlme_channel(std::move(local))
                   .Build();
    auto result = client_.buffer(test_arena_)->CreateIface(req);
    ASSERT_TRUE(result.ok() && !result->is_error());
    iface_id = result->value()->iface_id();
  }

  {
    // destroy client iface without iface_id fails
    auto req =
        fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_).Build();

    auto result = client_.buffer(test_arena_)->DestroyIface(req);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_error());
  }

  {
    // destroy client iface manually to clean up
    auto req = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_)
                   .iface_id(iface_id)
                   .Build();

    auto result = client_.buffer(test_arena_)->DestroyIface(req);
    ASSERT_TRUE(result.ok() && !result->is_error());
  }
}

TEST_F(LifecycleTest, FullmacStartMultipleCallsFails) {
  uint16_t iface_id = 0;
  {
    // First call to create iface with client role should succeed
    auto [local, _remote] = make_channel();
    auto req = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(test_arena_)
                   .role(wlan_common::WlanMacRole::kClient)
                   .mlme_channel(std::move(local))
                   .Build();
    auto result = client_.buffer(test_arena_)->CreateIface(req);
    ASSERT_TRUE(result.ok() && !result->is_error());
    iface_id = result->value()->iface_id();
  }

  {
    zx::result driver_connect_result =
        fdf::internal::DriverTransportConnect<fuchsia_wlan_fullmac::Service::WlanFullmacImpl>(
            CreateDriverSvcClient(), "brcmfmac-wlan-fullmac-client");
    EXPECT_EQ(ZX_OK, driver_connect_result.status_value());

    auto fullmac_client = fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl>(
        std::move(driver_connect_result.value()));

    {
      // first call to start succeeds
      auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
      auto result = fullmac_client.buffer(test_arena_)->Start(std::move(endpoints->client));
      ASSERT_TRUE(result.ok() && !result->is_error());
    }

    {
      // second call to start fails
      auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
      auto result = fullmac_client.buffer(test_arena_)->Start(std::move(endpoints->client));
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_error());
      ASSERT_EQ(result->error_value(), ZX_ERR_ALREADY_BOUND);
    }
  }

  {
    // destroy client iface manually to clean up
    auto req = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_)
                   .iface_id(iface_id)
                   .Build();

    auto result = client_.buffer(test_arena_)->DestroyIface(req);
    ASSERT_TRUE(result.ok() && !result->is_error());
  }
}

}  // namespace
}  // namespace wlan::brcmfmac
