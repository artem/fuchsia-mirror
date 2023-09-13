// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.clientsuite/cpp/markers.h>
#include <zircon/fidl.h>

#include "src/tests/fidl/channel_util/bytes.h"
#include "src/tests/fidl/client_suite/harness/harness.h"
#include "src/tests/fidl/client_suite/harness/ordinals.h"

namespace client_suite {
namespace {

using namespace ::channel_util;

// The client should reject V1 messages (no payload).
CLIENT_TEST(V1TwoWayNoPayload) {
  Bytes expected_request = Header{
      .txid = kTxidNotKnown,
      .ordinal = kOrdinalTwoWayNoPayload,
  };
  Bytes response = Header{
      .txid = kTxidNotKnown,
      .at_rest_flags = {0, 0},  // at-rest flags without V2 bit set
      .ordinal = kOrdinalTwoWayNoPayload,
  };
  runner()->CallTwoWayNoPayload({{.target = TakeClosedClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(result.value().fidl_error().value(), fidl_clientsuite::FidlErrorKind::kDecodingError);
  });
  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(server_end().read_and_check_unknown_txid(expected_request, &response.txid()));
  ASSERT_NE(response.txid(), 0u);
  ASSERT_OK(server_end().write(response));
  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should reject V1 messages (struct payload).
CLIENT_TEST(V1TwoWayStructPayload) {
  Bytes expected_request = Header{
      .txid = kTxidNotKnown,
      .ordinal = kOrdinalTwoWayStructPayload,
  };
  Bytes response = {
      Header{
          .txid = kTxidNotKnown,
          .at_rest_flags = {0, 0},  // at-rest flags without V2 bit set
          .ordinal = kOrdinalTwoWayStructPayload,
      },
      {int32(42), padding(4)},
  };
  runner()
      ->CallTwoWayStructPayload({{.target = TakeClosedClient()}})
      .ThenExactlyOnce([&](auto result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().fidl_error().has_value());
        ASSERT_EQ(result.value().fidl_error().value(),
                  fidl_clientsuite::FidlErrorKind::kDecodingError);
      });
  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(server_end().read_and_check_unknown_txid(expected_request, &response.txid()));
  ASSERT_NE(response.txid(), 0u);
  ASSERT_OK(server_end().write(response));
  WAIT_UNTIL_CALLBACK_RUN();
}

}  // namespace
}  // namespace client_suite
