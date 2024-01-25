// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This is separate from test_utils as it is not used in conformance tests and
// can therefore e.g. use handles

#ifndef SRC_LIB_FIDL_LLCPP_TESTS_TYPES_TEST_UTILS_H_
#define SRC_LIB_FIDL_LLCPP_TESTS_TYPES_TEST_UTILS_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif  // __Fuchsia__

#include <lib/fidl/cpp/wire/message.h>
#include <lib/fidl/cpp/wire/traits.h>
#include <lib/zx/event.h>
#include <zircon/fidl.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <vector>

#include <gtest/gtest.h>

namespace llcpp_types_test_utils {

// HandleChecker checks all |zx::event| handles registered with it are freed.
class HandleChecker {
 public:
  HandleChecker() = default;

  size_t size() const { return events_.size(); }

  void AddEvent(zx_handle_t event);
  void AddEvent(const zx::event& event);

  // Tests expectations that all handles registered via |AddEvent| has been
  // closed. Note that the kernel object is still kept alive by this class.
  void CheckEvents();

 private:
  std::vector<zx::event> events_;
};

// Verifies that:
//   - |bytes| and |handles| decodes successfully as |FidlType|
//   - all handles in |handles| are closed
//   - the resulting object fails to encode
//
// Requires that:
//   - If FidlType is a transactional message wrapper, it must have a single
//     `result` field that is either a union or a table.
//   - Otherwise, FidlType should be a wire domain object.
//
// Also runs a checker function on the decoded object, to test any properties.
//
// This is the intended behavior for all flexible types (unions and tables) in
// LLCPP, regardless of resourceness (since no unknown handles are stored, even on
// resource types).
template <typename FidlType, typename CheckerFunc>
void CannotProxyUnknownEnvelope(std::vector<uint8_t> bytes, std::vector<zx_handle_t> handles,
                                CheckerFunc check) {
  HandleChecker handle_checker;
  for (const auto& handle : handles) {
    handle_checker.AddEvent(handle);
  }

  std::vector<zx_handle_info_t> handle_infos;
  for (zx_handle_t handle : handles) {
    handle_infos.push_back({
        .handle = handle,
        .type = ZX_OBJ_TYPE_NONE,
        .rights = ZX_RIGHT_SAME_RIGHTS,
    });
  }

  const char* decode_error;
  uint8_t* trimmed_bytes = bytes.data();
  uint32_t trimmed_num_bytes = static_cast<uint32_t>(bytes.size());
  if (fidl::IsFidlTransactionalMessage<FidlType>::value) {
    zx_status_t status = ::fidl::internal::fidl_exclude_header_bytes(
        trimmed_bytes, trimmed_num_bytes, &trimmed_bytes, &trimmed_num_bytes, &decode_error);
    ASSERT_EQ(status, ZX_OK) << decode_error;
  }

  auto status = fidl_decode_etc(fidl::TypeTraits<FidlType>::kType, trimmed_bytes, trimmed_num_bytes,
                                handle_infos.data(), static_cast<uint32_t>(handle_infos.size()),
                                &decode_error);
  ASSERT_EQ(status, ZX_OK) << decode_error;

  auto result = reinterpret_cast<FidlType*>(&bytes[0]);
  if constexpr (fidl::IsFidlTransactionalMessage<FidlType>::value) {
    check(result->body.result);
  } else {
    check(*result);
  }
  handle_checker.CheckEvents();

  // Here we want to encode a message with unknown data. To be able to encoded all the unknown data,
  // we always use a full size buffer.
  FIDL_ALIGNDECL
  uint8_t encoded_bytes[ZX_CHANNEL_MAX_MSG_BYTES];
  fidl::internal::UnownedEncodedMessage<FidlType> encoded(
      fidl::internal::kLLCPPWireFormatVersion, encoded_bytes, ZX_CHANNEL_MAX_MSG_BYTES, result);
  ASSERT_EQ(encoded.status(), ZX_ERR_INVALID_ARGS) << encoded.status_string();

  // TODO(https://fxbug.dev/42110793): Test a reason enum instead of comparing strings.
  EXPECT_EQ(std::string(encoded.lossy_description()), "unknown union tag") << encoded.error();
}

}  // namespace llcpp_types_test_utils

#endif  // SRC_LIB_FIDL_LLCPP_TESTS_TYPES_TEST_UTILS_H_
