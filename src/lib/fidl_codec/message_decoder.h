// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_FIDL_CODEC_MESSAGE_DECODER_H_
#define SRC_LIB_FIDL_CODEC_MESSAGE_DECODER_H_

#include <lib/fidl/txn_header.h>
#include <lib/syslog/cpp/macros.h>

#include <cstdint>
#include <memory>
#include <ostream>
#include <sstream>
#include <string_view>
#include <unordered_set>
#include <vector>

#include "src/lib/fidl_codec/display_options.h"
#include "src/lib/fidl_codec/library_loader.h"
#include "src/lib/fidl_codec/memory_helpers.h"
#include "src/lib/fidl_codec/printer.h"

namespace fidl_codec {

struct DecodedMessageData;
class MessageDecoderDispatcher;
class Struct;
class StructValue;
class Type;
class Value;

enum class Direction { kUnknown, kClient, kServer };

enum class SyscallFidlType {
  kOutputMessage,  // A message (request or response which is written).
  kInputMessage,   // A message (request or response which is read).
  kOutputRequest,  // A request which is written (case of zx_channel_call).
  kInputResponse   // A response which is read (case of zx_channel_call).
};

class DecodedMessage {
 public:
  DecodedMessage() = default;

  zx_txid_t txid() const { return txid_; }
  uint64_t ordinal() const { return ordinal_; }
  zx_status_t epitaph_error() const { return epitaph_error_; }
  const ProtocolMethod* method() const { return method_; }
  std::unique_ptr<Value>& decoded_request() { return decoded_request_; }
  std::stringstream& request_error_stream() { return request_error_stream_; }
  std::unique_ptr<Value>& decoded_response() { return decoded_response_; }
  std::stringstream& response_error_stream() { return response_error_stream_; }
  Direction direction() const { return direction_; }
  bool is_request() const { return is_request_; }
  bool received() const { return received_; }

  // Decodes a message and fill all the fields. Returns true if we can display something.
  bool DecodeMessage(MessageDecoderDispatcher* dispatcher, uint64_t process_koid,
                     zx_handle_t handle, const uint8_t* bytes, size_t num_bytes,
                     const zx_handle_disposition_t* handles, size_t num_handles,
                     SyscallFidlType type, std::ostream& error_stream);

 private:
  const fidl_message_header_t* header_ = nullptr;
  zx_txid_t txid_ = 0;
  uint64_t ordinal_ = 0;
  zx_status_t epitaph_error_ = ZX_OK;
  const ProtocolMethod* method_ = nullptr;
  std::unique_ptr<Value> decoded_request_;
  std::stringstream request_error_stream_;
  bool matched_request_ = false;
  std::unique_ptr<Value> decoded_response_;
  std::stringstream response_error_stream_;
  bool matched_response_ = false;
  Direction direction_ = Direction::kUnknown;
  bool is_request_ = false;
  bool received_ = false;
};

// Class which is able to decode all the messages received/sent.
class MessageDecoderDispatcher {
 public:
  MessageDecoderDispatcher(LibraryLoader* loader, const DisplayOptions& display_options)
      : loader_(loader),
        display_options_(display_options),
        colors_(display_options.needs_colors ? WithColors : WithoutColors) {}

  LibraryLoader* loader() const { return loader_; }
  const DisplayOptions& display_options() const { return display_options_; }
  const Colors& colors() const { return colors_; }
  int columns() const { return display_options_.columns; }
  bool with_process_info() const { return display_options_.with_process_info; }
  std::map<std::tuple<zx_handle_t, uint64_t>, Direction>& handle_directions() {
    return handle_directions_;
  }

  void AddLaunchedProcess(uint64_t process_koid) { launched_processes_.insert(process_koid); }

  bool IsLaunchedProcess(uint64_t process_koid) {
    return launched_processes_.find(process_koid) != launched_processes_.end();
  }

  // Heuristic which computes the direction of a message (outgoing request, incomming response,
  // ...).
  Direction ComputeDirection(uint64_t process_koid, zx_handle_t handle, SyscallFidlType type,
                             const ProtocolMethod* method, bool only_one_valid);

  // Update the direction. Used when the heuristic was wrong.
  void UpdateDirection(uint64_t process_koid, zx_handle_t handle, Direction direction) {
    handle_directions_[std::make_tuple(handle, process_koid)] = direction;
  }

 private:
  LibraryLoader* const loader_;
  const DisplayOptions& display_options_;
  const Colors& colors_;
  std::unordered_set<uint64_t> launched_processes_;
  std::map<std::tuple<zx_handle_t, uint64_t>, Direction> handle_directions_;
};

// Helper to decode a message (request or response). It generates a Value.
class MessageDecoder {
 public:
  MessageDecoder(const uint8_t* bytes, uint64_t num_bytes, const zx_handle_disposition_t* handles,
                 size_t num_handles, std::ostream& error_stream);
  MessageDecoder(MessageDecoder* container, uint64_t offset, uint64_t num_bytes_remaining,
                 uint64_t num_handles_remaining);

  uint64_t absolute_offset() const { return absolute_offset_; }

  uint64_t num_bytes() const { return num_bytes_; }

  const zx_handle_disposition_t* handle_pos() const { return handle_pos_; }

  uint64_t next_object_offset() const { return next_object_offset_; }

  WireVersion version() const { return version_; }

  bool HasError() const { return error_count_ > 0; }

  // Add an error.
  std::ostream& AddError() {
    ++error_count_;
    return error_stream_;
  }

  size_t GetRemainingHandles() const { return end_handle_pos_ - handle_pos_; }

  // Used by numeric types to retrieve a numeric value. If there is not enough
  // data, returns false and value is set to its zero-initialized value.
  template <typename T>
  bool GetValueAt(uint64_t offset, T* value);

  // Gets the address of some data of |size| at |offset|. If there is not enough
  // data, returns null.
  const uint8_t* GetAddress(uint64_t offset, uint64_t size) {
    if ((offset > num_bytes_) || (size > num_bytes_ - offset)) {
      AddError() << std::hex << (absolute_offset_ + offset) << std::dec
                 << ": Not enough data to decode (needs " << size << ", remains "
                 << (num_bytes_ - offset) << ")\n";
      return nullptr;
    }
    return start_byte_pos_ + offset;
  }

  // Sets the next object offset. The current object (which is at the previous value of next object
  // offset) is not decoded yet. It will be decoded just after this call.
  // The new offset is 8 byte aligned.
  void SkipObject(uint64_t size) {
    uint64_t new_offset = (next_object_offset_ + size + 7) & ~7;
    if (new_offset > num_bytes_) {
      AddError() << std::hex << (absolute_offset_ + next_object_offset_) << std::dec
                 << ": Not enough data to decode (needs " << (new_offset - next_object_offset_)
                 << ", remains " << (num_bytes_ - next_object_offset_) << ")\n";
      new_offset = num_bytes_;
    }
    next_object_offset_ = new_offset;
  }

  // Consumes a handle. Returns FIDL_HANDLE_ABSENT if there is no handle
  // available.
  zx_handle_disposition_t GetNextHandle() {
    if (handle_pos_ == end_handle_pos_) {
      AddError() << "Not enough handles\n";
      zx_handle_disposition_t result;
      result.operation = kNoHandleDisposition;
      result.handle = FIDL_HANDLE_ABSENT;
      result.type = ZX_OBJ_TYPE_NONE;
      result.rights = 0;
      result.result = ZX_OK;
      return result;
    }
    return *handle_pos_++;
  }

  // Decodes a whole message (request or response), skipping over the header.
  // The |payload_type| must be EmptyPayloadType, StructType, TableType, or UnionType.
  // Returns an EmptyPayloadValue, StructValue, TableValue, or UnionValue respectively.
  std::unique_ptr<Value> DecodeMessage(const Type* payload_type);

  // Decodes the header for a value which can be null.
  bool DecodeNullableHeader(uint64_t offset, uint64_t size, bool* is_null,
                            uint64_t* nullable_offset);

  // Decodes a value in an envelope.
  std::unique_ptr<Value> DecodeEnvelope(uint64_t offset, const Type* type);

  // Checks that we have a null envelope encoded.
  bool CheckNullEnvelope(uint64_t offset);

  // Skips an unknown envelope content.
  void SkipEnvelope(uint64_t offset);

 private:
  // Decodes a field. Used by envelopes.
  std::unique_ptr<Value> DecodeValue(const Type* type, bool is_inline);

  // The absolute offset in the main buffer.
  const uint64_t absolute_offset_ = 0;

  // The size of the message bytes.
  const uint64_t num_bytes_;

  // The start of the message.
  const uint8_t* const start_byte_pos_;

  // The end of the message.
  const zx_handle_disposition_t* const end_handle_pos_;

  // The current handle decoding position in the message.
  const zx_handle_disposition_t* handle_pos_;

  // Location of the next out of line object.
  uint64_t next_object_offset_ = 0;

  // Errors found during the message decoding.
  int error_count_ = 0;

  // Stream for the errors.
  std::ostream& error_stream_;

  // Wire format version.
  WireVersion version_ = WireVersion::kWireV2;
};

// Used by numeric types to retrieve a numeric value. If there is not enough
// data, returns false and value is set to its zero-initialized-value.
template <typename T>
bool MessageDecoder::GetValueAt(uint64_t offset, T* value) {
  if (offset + sizeof(T) > num_bytes_) {
    if (offset <= num_bytes_) {
      AddError() << std::hex << (absolute_offset_ + offset) << std::dec
                 << ": Not enough data to decode (needs " << sizeof(T) << ", remains "
                 << (num_bytes_ - offset) << ")\n";
    }
    *value = {};
    return false;
  }
  *value = internal::MemoryFrom<T>(start_byte_pos_ + offset);
  return true;
}

}  // namespace fidl_codec

#endif  // SRC_LIB_FIDL_CODEC_MESSAGE_DECODER_H_
