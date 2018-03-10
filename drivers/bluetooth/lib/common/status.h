// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#include <cstdint>
#include <string>

#include "lib/fxl/logging.h"
#include "lib/fxl/strings/string_printf.h"

namespace btlib {
namespace common {

// Status types used for internal errors generated by the host
enum class HostError : uint8_t {
  kNoError = 0u,

  // Not found.
  kNotFound,

  // The time limit for the operation has expired.
  kTimedOut,

  // The operation was initiated with invalid parameters.
  kInvalidParameters,

  // The operation was canceled.
  kCanceled,

  // Operation is already in progress.
  kInProgress,

  // Operation is not supported by the host.
  kNotSupported,

  // Received an invalid packet from the controller.
  kPacketMalformed,

  // Ran out of resources.
  kOutOfMemory,

  // Error code for protocol errors. The actual error code is specified by a
  // protocol error code type.
  kProtocolError,

  // Generic error code. Use this only if another error code does not accurately
  // capture the failure condition.
  kFailed,
};

// Returns a string representation of HostError.
std::string HostErrorToString(HostError error);

// Required trait for ProtocolErrorCode types.
template <typename ProtocolErrorCode>
struct ProtocolErrorTraits {
  // Returns a string representation of the given ProtocolErrorCode value.
  static std::string ToString(ProtocolErrorCode);
};

template <typename ProtocolErrorCode>
class Status {
  using ProtoTraits = ProtocolErrorTraits<ProtocolErrorCode>;

 public:
  // The result will carry a protocol error code.
  constexpr explicit Status(ProtocolErrorCode proto_code)
      : error_(HostError::kProtocolError), protocol_error_(proto_code) {}

  // The result will carry a host error. Constructs a success result by default.
  constexpr explicit Status(HostError ecode = HostError::kNoError)
      : error_(ecode) {
    FXL_DCHECK(ecode != HostError::kProtocolError)
        << "HostError::kProtocolError not allowed in HostError constructor";
  }

  // Returns true if this is a success result.
  bool is_success() const { return error_ == HostError::kNoError; }

  // Returns the host error code.
  HostError error() const { return error_; }

  // Returns true if this result is a protocol error.
  bool is_protocol_error() const { return error_ == HostError::kProtocolError; }

  // Returns the protocol error code. This value is undefined if error() is
  // not equal to HostError::kProtocolError.
  ProtocolErrorCode protocol_error() const { return protocol_error_; }

  // Returns true if this is a success result.
  operator bool() const { return is_success(); }

  // Returns a string representation.
  inline std::string ToString() const {
    return fxl::StringPrintf(
        "[status: %s]",
        (is_protocol_error() ? ProtoTraits::ToString(protocol_error())
                             : HostErrorToString(error()))
            .c_str());
  }

 private:
  HostError error_;
  ProtocolErrorCode protocol_error_;
};

}  // namespace common
}  // namespace btlib
