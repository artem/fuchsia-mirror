// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/command_handler.h"

namespace bt::l2cap::internal {
class TestResponse : public CommandHandler::Response {
 public:
  TestResponse(SignalingChannel::Status status) : CommandHandler::Response(status) {}

  bool TestParseReject(const ByteBuffer& rej_payload_buf) { return ParseReject(rej_payload_buf); }
};

void fuzz(const uint8_t* data, size_t size) {
  DynamicByteBuffer buf(size);
  memcpy(buf.mutable_data(), data, size);
  TestResponse test_response(SignalingChannel::Status::kSuccess);
  bool result = test_response.TestParseReject(buf);
  (void)result;
}

}  // namespace bt::l2cap::internal

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  bt::l2cap::internal::fuzz(data, size);
  return 0;
}
