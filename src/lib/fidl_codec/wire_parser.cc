// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/fidl_codec/wire_parser.h"

#include "src/lib/fidl_codec/wire_object.h"
#include "src/lib/fidl_codec/wire_types.h"

namespace fidl_codec {

namespace {

// Takes a Message which holds either a request or a response and extracts a
// JSON object which represents the message. The format of the message is
// specified by type.
// Returns true on success, false on failure.
bool DecodeMessage(const Type* type, const uint8_t* bytes, size_t num_bytes,
                   const zx_handle_disposition_t* handles, size_t num_handles,
                   std::unique_ptr<Value>* decoded_object, std::ostream& error_stream) {
  MessageDecoder decoder(bytes, num_bytes, handles, num_handles, error_stream);
  *decoded_object = decoder.DecodeMessage(type);
  return !decoder.HasError();
}

}  // anonymous namespace

bool DecodeRequest(const ProtocolMethod* method, const uint8_t* bytes, size_t num_bytes,
                   const zx_handle_disposition_t* handles, size_t num_handles,
                   std::unique_ptr<Value>* decoded_object, std::ostream& error_stream) {
  if (!method->has_request()) {
    return false;
  }
  return DecodeMessage(method->request(), bytes, num_bytes, handles, num_handles, decoded_object,
                       error_stream);
}

bool DecodeResponse(const ProtocolMethod* method, const uint8_t* bytes, size_t num_bytes,
                    const zx_handle_disposition_t* handles, size_t num_handles,
                    std::unique_ptr<Value>* decoded_object, std::ostream& error_stream) {
  if (!method->has_response()) {
    return false;
  }
  return DecodeMessage(method->response(), bytes, num_bytes, handles, num_handles, decoded_object,
                       error_stream);
}

}  // namespace fidl_codec
