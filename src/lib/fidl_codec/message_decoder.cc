// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/fidl_codec/message_decoder.h"

#include <lib/syslog/cpp/macros.h>

#include <ostream>
#include <sstream>

#include <rapidjson/stringbuffer.h>
#include <rapidjson/writer.h>

#include "src/lib/fidl_codec/library_loader.h"
#include "src/lib/fidl_codec/status.h"
#include "src/lib/fidl_codec/wire_object.h"
#include "src/lib/fidl_codec/wire_parser.h"
#include "src/lib/fidl_codec/wire_types.h"

namespace fidl_codec {

std::string DocumentToString(rapidjson::Document* document);

bool DecodedMessage::DecodeMessage(MessageDecoderDispatcher* dispatcher, uint64_t process_koid,
                                   zx_handle_t handle, const uint8_t* bytes, size_t num_bytes,
                                   const zx_handle_disposition_t* handles, size_t num_handles,
                                   SyscallFidlType type, std::ostream& error_stream) {
  if ((bytes == nullptr) || (num_bytes < sizeof(fidl_message_header_t))) {
    error_stream << "not enough data for message\n";
    return false;
  }

  header_ = reinterpret_cast<const fidl_message_header_t*>(bytes);
  txid_ = header_->txid;
  ordinal_ = header_->ordinal;

  // Handle the epitaph header explicitly.
  if (ordinal_ == kFidlOrdinalEpitaph) {
    if (num_bytes < sizeof(fidl_epitaph)) {
      error_stream << "not enough data for epitaph\n";
      return false;
    }
    switch (type) {
      case SyscallFidlType::kOutputRequest:
      case SyscallFidlType::kOutputMessage:
        received_ = false;
        break;
      case SyscallFidlType::kInputResponse:
      case SyscallFidlType::kInputMessage:
        received_ = true;
        break;
    }
    auto epitaph = reinterpret_cast<const fidl_epitaph_t*>(header_);
    epitaph_error_ = epitaph->error;
    return true;
  }

  if ((dispatcher == nullptr) || (dispatcher->loader() == nullptr)) {
    return false;
  }

  const std::vector<ProtocolMethod*>* methods = dispatcher->loader()->GetByOrdinal(ordinal_);
  if (methods == nullptr || methods->empty()) {
    error_stream << "Protocol method with ordinal 0x" << std::hex << header_->ordinal
                 << " not found\n";
    return false;
  }

  method_ = (*methods)[0];

  matched_request_ = DecodeRequest(method_, bytes, num_bytes, handles, num_handles,
                                   &decoded_request_, request_error_stream_);
  matched_response_ = DecodeResponse(method_, bytes, num_bytes, handles, num_handles,
                                     &decoded_response_, response_error_stream_);

  direction_ = dispatcher->ComputeDirection(process_koid, handle, type, method_,
                                            matched_request_ != matched_response_);
  switch (type) {
    case SyscallFidlType::kOutputMessage:
      if (direction_ == Direction::kClient) {
        is_request_ = true;
      }
      received_ = false;
      break;
    case SyscallFidlType::kInputMessage:
      if (direction_ == Direction::kServer) {
        is_request_ = true;
      }
      received_ = true;
      break;
    case SyscallFidlType::kOutputRequest:
      is_request_ = true;
      received_ = false;
      direction_ = Direction::kClient;
      dispatcher->UpdateDirection(process_koid, handle, direction_);
      return true;
    case SyscallFidlType::kInputResponse:
      received_ = true;
      direction_ = Direction::kClient;
      dispatcher->UpdateDirection(process_koid, handle, direction_);
      return true;
  }
  if (direction_ != Direction::kUnknown) {
    if ((is_request_ && !matched_request_) || (!is_request_ && !matched_response_)) {
      if ((is_request_ && matched_response_) || (!is_request_ && matched_request_)) {
        if ((type == SyscallFidlType::kOutputRequest) ||
            (type == SyscallFidlType::kInputResponse)) {
          // We know the direction: we can't be wrong => we haven't been able to decode the message.
          // However, we can still display something.
          return true;
        }
        // The first determination seems to be wrong. That is, we are expecting
        // a request but only a response has been successfully decoded or we are
        // expecting a response but only a request has been successfully
        // decoded.
        // Invert the deduction which should now be the right one.
        dispatcher->UpdateDirection(
            process_koid, handle,
            (direction_ == Direction::kClient) ? Direction::kServer : Direction::kClient);
        is_request_ = !is_request_;
      }
    }
  }
  return true;
}

Direction MessageDecoderDispatcher::ComputeDirection(uint64_t process_koid, zx_handle_t handle,
                                                     SyscallFidlType type,
                                                     const ProtocolMethod* method,
                                                     bool only_one_valid) {
  auto handle_direction = handle_directions_.find(std::make_tuple(handle, process_koid));
  if (handle_direction != handle_directions_.end()) {
    return handle_direction->second;
  }
  // This is the first read or write we intercept for this handle/koid. If we
  // launched the process, we suppose we intercepted the very first read or
  // write.
  // If this is not an event (which would mean method->has_request() is false),
  // a write means that we are watching a client (a client starts by writing a
  // request) and a read means that we are watching a server (a server starts
  // by reading the first client request).
  // If we attached to a running process, we can only determine correctly if
  // we are watching a client or a server if we have only one matched_request
  // or one matched_response.
  if (IsLaunchedProcess(process_koid) || only_one_valid) {
    // We launched the process or exactly one of request and response are
    // valid => we can determine the direction.
    switch (type) {
      case SyscallFidlType::kOutputMessage:
        handle_directions_[std::make_tuple(handle, process_koid)] =
            method->has_request() ? Direction::kClient : Direction::kServer;
        break;
      case SyscallFidlType::kInputMessage:
        handle_directions_[std::make_tuple(handle, process_koid)] =
            method->has_request() ? Direction::kServer : Direction::kClient;
        break;
      case SyscallFidlType::kOutputRequest:
      case SyscallFidlType::kInputResponse:
        handle_directions_[std::make_tuple(handle, process_koid)] = Direction::kClient;
    }
    return handle_directions_[std::make_tuple(handle, process_koid)];
  }
  return Direction::kUnknown;
}

MessageDecoder::MessageDecoder(const uint8_t* bytes, uint64_t num_bytes,
                               const zx_handle_disposition_t* handles, size_t num_handles,
                               std::ostream& error_stream)
    : num_bytes_(num_bytes),
      start_byte_pos_(bytes),
      end_handle_pos_(handles + num_handles),
      handle_pos_(handles),
      error_stream_(error_stream) {
  version_ = WireVersion::kWireV2;
}

MessageDecoder::MessageDecoder(MessageDecoder* container, uint64_t offset, uint64_t num_bytes,
                               uint64_t num_handles)
    : absolute_offset_(container->absolute_offset() + offset),
      num_bytes_(num_bytes),
      start_byte_pos_(container->start_byte_pos_ + offset),
      end_handle_pos_(container->handle_pos_ + num_handles),
      handle_pos_(container->handle_pos_),
      error_stream_(container->error_stream_) {
  container->handle_pos_ += num_handles;
  version_ = container->version_;
}

std::unique_ptr<Value> MessageDecoder::DecodeMessage(const Type* payload_type) {
  SkipObject(kTransactionHeaderSize);
  SkipObject(payload_type->InlineSize(version_));
  std::unique_ptr<Value> message = payload_type->Decode(this, kTransactionHeaderSize);

  // It's an error if we didn't use all the bytes in the buffer.
  if (next_object_offset_ != num_bytes_) {
    AddError() << "Message not fully decoded (decoded=" << next_object_offset_
               << ", size=" << num_bytes_ << ")\n";
  }
  // It's an error if we didn't use all the handles in the buffer.
  if (GetRemainingHandles() != 0) {
    AddError() << "Message not fully decoded (remain " << GetRemainingHandles() << " handles)\n";
  }

  return message;
}

std::unique_ptr<Value> MessageDecoder::DecodeValue(const Type* type, bool is_inline) {
  if (!is_inline) {
    // Set the offset for the next object (just after this one).
    SkipObject(type->InlineSize(version_));
  }
  // Decode the envelope.
  std::unique_ptr<Value> result = type->Decode(this, 0);
  if (!is_inline) {
    // It's an error if we didn't use all the bytes in the buffer.
    if (next_object_offset_ != num_bytes_) {
      AddError() << "Message envelope not fully decoded (decoded=" << next_object_offset_
                 << ", size=" << num_bytes_ << ")\n";
    }
  }
  // It's an error if we didn't use all the handles in the buffer.
  if (GetRemainingHandles() != 0) {
    AddError() << "Message envelope not fully decoded (remain " << GetRemainingHandles()
               << " handles)\n";
  }
  return result;
}

bool MessageDecoder::DecodeNullableHeader(uint64_t offset, uint64_t size, bool* is_null,
                                          uint64_t* nullable_offset) {
  uintptr_t data;
  if (!GetValueAt(offset, &data)) {
    return false;
  }

  if (data == FIDL_ALLOC_ABSENT) {
    *is_null = true;
    *nullable_offset = 0;
    return true;
  }
  if (data != FIDL_ALLOC_PRESENT) {
    AddError() << std::hex << (absolute_offset() + offset) << std::dec << ": Invalid value <"
               << std::hex << data << std::dec << "> for nullable\n";
    return false;
  }
  *is_null = false;
  *nullable_offset = next_object_offset();
  // Set the offset for the next object (just after this one).
  SkipObject(size);
  return true;
}

std::unique_ptr<Value> MessageDecoder::DecodeEnvelope(uint64_t offset, const Type* type) {
  FX_DCHECK(type != nullptr);
  uint32_t envelope_bytes;
  uint32_t envelope_handles;
  bool is_null;
  uint64_t nullable_offset;
  bool is_inline = false;
  uint64_t inline_offset = offset;
  GetValueAt(offset, &envelope_bytes);
  offset += sizeof(envelope_bytes);
  uint16_t envelope_handles_16;
  GetValueAt(offset, &envelope_handles_16);
  offset += sizeof(envelope_handles_16);
  uint16_t flags;
  GetValueAt(offset, &flags);
  offset += sizeof(flags);

  is_inline = (flags & 1) != 0;

  envelope_handles = envelope_handles_16;
  is_null = !is_inline && (envelope_bytes == 0) && (envelope_handles == 0);
  if (is_null) {
    return std::make_unique<NullValue>();
  }
  nullable_offset = is_inline ? inline_offset : next_object_offset();
  envelope_bytes = is_inline ? sizeof(uint32_t) : envelope_bytes;
  if ((envelope_bytes > 0) && !is_inline) {
    SkipObject(envelope_bytes);
  }
  if (!is_inline && (envelope_bytes > num_bytes() - nullable_offset)) {
    AddError() << std::hex << (absolute_offset() + nullable_offset) << std::dec
               << ": Not enough data to decode an envelope\n";
    return std::make_unique<InvalidValue>();
  }
  if (envelope_handles > GetRemainingHandles()) {
    AddError() << std::hex << (absolute_offset() + nullable_offset) << std::dec
               << ": Not enough handles to decode an envelope\n";
    return std::make_unique<InvalidValue>();
  }
  MessageDecoder envelope_decoder(this, nullable_offset, envelope_bytes, envelope_handles);
  return envelope_decoder.DecodeValue(type, is_inline);
}

bool MessageDecoder::CheckNullEnvelope(uint64_t offset) {
  uint32_t envelope_bytes;
  uint32_t envelope_handles;
  bool is_null;
  GetValueAt(offset, &envelope_bytes);
  offset += sizeof(envelope_bytes);
  uint16_t envelope_handles_16;
  GetValueAt(offset, &envelope_handles_16);
  offset += sizeof(envelope_handles_16);
  uint16_t flags;
  GetValueAt(offset, &flags);
  offset += sizeof(flags);

  bool is_inline = (flags & 1) != 0;

  envelope_handles = envelope_handles_16;
  is_null = (envelope_bytes == 0) && (envelope_handles == 0);
  envelope_bytes = is_inline ? sizeof(uint32_t) : envelope_bytes;
  if (!is_null) {
    AddError() << std::hex << (absolute_offset() + offset) << std::dec
               << ": Expecting null envelope\n";
    return false;
  }
  if (envelope_bytes != 0) {
    AddError() << std::hex << (absolute_offset() + offset) << std::dec
               << ": Null envelope shouldn't have bytes\n";
    return false;
  }
  if (envelope_handles != 0) {
    AddError() << std::hex << (absolute_offset() + offset) << std::dec
               << ": Null envelope shouldn't have handles\n";
    return false;
  }
  return true;
}

void MessageDecoder::SkipEnvelope(uint64_t offset) {
  uint32_t envelope_bytes;
  uint32_t envelope_handles;
  bool is_null;
  uint64_t nullable_offset;
  bool is_inline = false;
  uint64_t inline_offset = offset;
  GetValueAt(offset, &envelope_bytes);
  offset += sizeof(envelope_bytes);
  uint16_t envelope_handles_16;
  GetValueAt(offset, &envelope_handles_16);
  offset += sizeof(envelope_handles_16);
  uint16_t flags;
  GetValueAt(offset, &flags);
  offset += sizeof(flags);

  is_inline = (flags & 1) != 0;

  envelope_handles = envelope_handles_16;
  is_null = (envelope_bytes == 0) && (envelope_handles == 0);
  nullable_offset = is_null ? 0 : (is_inline ? inline_offset : next_object_offset());
  envelope_bytes = is_inline ? sizeof(uint32_t) : envelope_bytes;
  if (is_null) {
    return;
  }
  if ((envelope_bytes > 0) && !is_inline) {
    SkipObject(envelope_bytes);
  }
  if (!is_inline && (envelope_bytes > num_bytes() - nullable_offset)) {
    AddError() << std::hex << (absolute_offset() + nullable_offset) << std::dec
               << ": Not enough data to decode an envelope\n";
    return;
  }
  if (envelope_handles > GetRemainingHandles()) {
    AddError() << std::hex << (absolute_offset() + nullable_offset) << std::dec
               << ": Not enough handles to decode an envelope\n";
    return;
  }
}

}  // namespace fidl_codec
