// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fuchsia/media/cpp/fidl.h>
#include <lib/fidl/cpp/clone.h>
#include <lib/media/codec_impl/codec_impl.h>
#include <lib/media/codec_impl/codec_port.h>
#include <lib/media/codec_impl/decryptor_adapter.h>
#include <lib/media/codec_impl/log.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <zircon/assert.h>

#include <algorithm>
#include <cstdint>
#include <limits>

#include <bind/fuchsia/sysmem/heap/cpp/bind.h>
#include <safemath/safe_math.h>

namespace {

constexpr uint64_t kInputBufferConstraintsVersionOrdinal = 1;

constexpr uint32_t kInputMinBufferCountForCamping = 1;
constexpr uint32_t kInputMinBufferCountForDedicatedSlack = 2;

constexpr uint32_t kOutputMinBufferCountForCamping = 1;

}  // namespace

DecryptorAdapter::DecryptorAdapter(std::mutex& lock, CodecAdapterEvents* codec_adapter_events)
    : CodecAdapter(lock, codec_adapter_events),
      input_processing_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
  ZX_DEBUG_ASSERT(codec_adapter_events);
}

DecryptorAdapter::DecryptorAdapter(std::mutex& lock, CodecAdapterEvents* codec_adapter_events,
                                   inspect::Node inspect_node)
    : CodecAdapter(lock, codec_adapter_events),
      inspect_properties_(std::move(inspect_node)),
      input_processing_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
  ZX_DEBUG_ASSERT(codec_adapter_events);
}

DecryptorAdapter::InspectProperties::InspectProperties(inspect::Node temp_node)
    : node(std::move(temp_node)),
      secure_mode(node.CreateBool("secure_mode", false)),
      scheme(node.CreateString("scheme", "")),
      key_id(node.CreateByteVector("key_id", {})) {}

DecryptorAdapter::InspectProperties::PortProperties::PortProperties(inspect::Node& parent,
                                                                    const std::string& port_name)
    : node(parent.CreateChild(port_name)),
      buffer_count(node.CreateUint("buffer_count", 0)),
      packet_count(node.CreateUint("packet_count", 0)) {}

bool DecryptorAdapter::IsCoreCodecRequiringOutputConfigForFormatDetection() { return true; }

bool DecryptorAdapter::IsCoreCodecMappedBufferUseful(CodecPort port) {
  // Only require mapped buffers for input and clear output buffers.
  return (port == kInputPort) || (port == kOutputPort && !is_secure());
}

bool DecryptorAdapter::IsCoreCodecHwBased(CodecPort port) { return false; }

void DecryptorAdapter::CoreCodecInit(
    const fuchsia::media::FormatDetails& initial_input_format_details) {
  zx_status_t result = input_processing_loop_.StartThread(
      "DecryptorAdapter::input_processing_thread_", &input_processing_thread_);
  if (result != ZX_OK) {
    events_->onCoreCodecFailCodec(
        "In DecryptorAdapter::CoreCodecInit(), StartThread() failed (input)");
    return;
  }
  SetProcessingSchedulerProfile(zx::unowned_thread(thrd_get_zx_handle(input_processing_thread_)));
}

void DecryptorAdapter::CoreCodecSetSecureMemoryMode(
    CodecPort port, fuchsia::mediacodec::SecureMemoryMode secure_memory_mode) {
  if (port == kInputPort) {
    if (secure_memory_mode != fuchsia::mediacodec::SecureMemoryMode::OFF) {
      events_->onCoreCodecFailCodec("Decryptors don't do secure input.");
      return;
    }
    // Ignore OFF for input; that's what the code assumes elsewhere.
  } else {
    ZX_DEBUG_ASSERT(port == kOutputPort);
    if (secure_memory_mode != fuchsia::mediacodec::SecureMemoryMode::OFF &&
        secure_memory_mode != fuchsia::mediacodec::SecureMemoryMode::ON) {
      events_->onCoreCodecFailCodec("Unexpected output SecureMemoryMode (maybe DYNAMIC?)");
    }
    secure_mode_ = (secure_memory_mode == fuchsia::mediacodec::SecureMemoryMode::ON);
    if (inspect_properties_) {
      inspect_properties_->secure_mode.Set(secure_mode_);
    }
  }
}

fuchsia_sysmem2::BufferCollectionConstraints
DecryptorAdapter::CoreCodecGetBufferCollectionConstraints2(
    CodecPort port, const fuchsia::media::StreamBufferConstraints& stream_buffer_constraints,
    const fuchsia::media::StreamBufferPartialSettings& partial_settings) {
  fuchsia_sysmem2::BufferCollectionConstraints result;

  // The CodecImpl won't hand us the sysmem token, so we shouldn't expect to have the token here.
  ZX_DEBUG_ASSERT(!partial_settings.has_sysmem_token());

  // We also ask for some dedicated slack on input, since decryption involves some per-buffer
  // process hops per buffer.  Some slack should help avoid falling behind.
  //
  // We can't ask for slack on output because VDEC has limited space and the HW requires each buffer
  // to be large enough to hold the largest compressed frame we might potentially see.
  if (port == kOutputPort) {
    result.min_buffer_count_for_camping() = kOutputMinBufferCountForCamping;
    ZX_DEBUG_ASSERT(!result.min_buffer_count_for_dedicated_slack().has_value());
  } else {
    result.min_buffer_count_for_camping() = kInputMinBufferCountForCamping;
    result.min_buffer_count_for_dedicated_slack() = kInputMinBufferCountForDedicatedSlack;
  }
  ZX_DEBUG_ASSERT(!result.min_buffer_count_for_shared_slack().has_value());
  ZX_DEBUG_ASSERT(!result.max_buffer_count().has_value());

  if (port == kOutputPort && is_secure()) {
    result.buffer_memory_constraints() = GetSecureOutputMemoryConstraints2();
  } else {
    auto& bmc = result.buffer_memory_constraints().emplace();
    bmc.physically_contiguous_required() = false;
    bmc.secure_required() = false;
  }

  ZX_DEBUG_ASSERT(!result.image_format_constraints().has_value());

  // We don't have to fill out usage - CodecImpl takes care of that.
  ZX_DEBUG_ASSERT(!result.usage().has_value());

  return result;
}

void DecryptorAdapter::CoreCodecSetBufferCollectionInfo(
    CodecPort port, const fuchsia_sysmem2::BufferCollectionInfo& buffer_collection_info) {
  const auto& buffer_settings = *buffer_collection_info.settings()->buffer_settings();
  if (port == kInputPort) {
    if (*buffer_settings.coherency_domain() != fuchsia_sysmem2::CoherencyDomain::kCpu) {
      events_->onCoreCodecFailCodec("DecryptorAdapter only supports CPU coherent input buffers");
      return;
    }
  } else if (!is_secure()) {  // port == kOutputPort
    if (*buffer_settings.coherency_domain() != fuchsia_sysmem2::CoherencyDomain::kCpu) {
      events_->onCoreCodecFailCodec(
          "DecryptorAdapter only supports CPU coherent clear output buffers");
      return;
    }
  } else {  // port == kOutputPort && is_secure()
    if (!*buffer_settings.is_secure()) {
      events_->onCoreCodecFailCodec("Secure DecryptorAdapter requires secure buffers");
      return;
    }
    if (*buffer_settings.coherency_domain() != fuchsia_sysmem2::CoherencyDomain::kInaccessible) {
      events_->onCoreCodecFailCodec(
          "Secure DecryptorAdapter only supports INACCESSIBLE coherent output buffers");
      return;
    }
  }

  if (inspect_properties_) {
    if (port == kInputPort) {
      inspect_properties_->input =
          InspectProperties::PortProperties(inspect_properties_->node, "input_port");
    } else {  // port == kOutputPort
      inspect_properties_->output =
          InspectProperties::PortProperties(inspect_properties_->node, "output_port");
    }
  }
}

void DecryptorAdapter::CoreCodecStartStream() {
  {  // scope lock
    std::lock_guard<std::mutex> lock(lock_);
    is_stream_failed_ = false;
    input_queue_.clear();
  }  // ~lock

  constexpr bool kKeepData = true;
  free_output_packets_.Reset(kKeepData);
  free_output_buffers_.Reset(kKeepData);
}

void DecryptorAdapter::CoreCodecQueueInputFormatDetails(
    const fuchsia::media::FormatDetails& per_stream_override_format_details) {
  QueueInputItem(CodecInputItem::FormatDetails(per_stream_override_format_details));
}

void DecryptorAdapter::CoreCodecQueueInputPacket(CodecPacket* packet) {
  QueueInputItem(CodecInputItem::Packet(packet));
}

void DecryptorAdapter::CoreCodecQueueInputEndOfStream() {
  // This queues a marker, but doesn't force the decryptor to necessarily decrypt all the way up to
  // the marker, depending on whether the client closes the stream or switches to a different stream
  // first - in those cases it's fine for the marker to never show up as output EndOfStream.
  QueueInputItem(CodecInputItem::EndOfStream());
}

void DecryptorAdapter::CoreCodecStopStream() {
  free_output_packets_.StopAllWaits();
  free_output_buffers_.StopAllWaits();

  std::unique_lock<std::mutex> lock(lock_);

  // This helps any previously-queued ProcessInput() calls return faster.
  is_cancelling_input_processing_ = true;
  // TODO(dustingreen): Remove testonly=true from one_shot_event, and use it here.  But keep
  // is_cancelling_input_processing_ since needed to make input loop stop quickly.
  std::condition_variable stop_input_processing_condition;
  // We know there won't be any new queuing of input, so once this posted work
  // runs, we know all previously-queued ProcessInput() calls have returned.
  PostToInputProcessingThread([this, &stop_input_processing_condition] {
    std::list<CodecInputItem> leftover_input_items;
    {  // scope lock
      std::lock_guard<std::mutex> lock(lock_);
      ZX_DEBUG_ASSERT(is_cancelling_input_processing_);
      leftover_input_items = std::move(input_queue_);
      is_cancelling_input_processing_ = false;
    }  // ~lock
    for (auto& input_item : leftover_input_items) {
      if (input_item.is_packet()) {
        events_->onCoreCodecInputPacketDone(std::move(input_item.packet()));
      }
    }
    stop_input_processing_condition.notify_all();
  });
  while (is_cancelling_input_processing_) {
    stop_input_processing_condition.wait(lock);
  }
  ZX_DEBUG_ASSERT(!is_cancelling_input_processing_);
}

void DecryptorAdapter::CoreCodecAddBuffer(CodecPort port, const CodecBuffer* buffer) {
  if (inspect_properties_) {
    if (port == kInputPort) {
      inspect_properties_->input->buffer_count.Add(1);
    } else if (port == kOutputPort) {
      inspect_properties_->output->buffer_count.Add(1);
    }
  }

  if (port == kOutputPort) {
    all_output_buffers_.push_back(buffer);
  }
}

void DecryptorAdapter::CoreCodecConfigureBuffers(
    CodecPort port, const std::vector<std::unique_ptr<CodecPacket>>& packets) {
  if (inspect_properties_) {
    if (port == kInputPort) {
      inspect_properties_->input->packet_count.Set(packets.size());
    } else if (port == kOutputPort) {
      inspect_properties_->output->packet_count.Set(packets.size());
    }
  }

  if (port != kOutputPort) {
    return;
  }

  ZX_DEBUG_ASSERT(!all_output_buffers_.empty());

  std::vector<CodecPacket*> all_packets;
  for (auto& packet : packets) {
    all_packets.push_back(packet.get());
  }
  std::shuffle(all_packets.begin(), all_packets.end(), not_for_security_prng_);
  for (CodecPacket* packet : all_packets) {
    free_output_packets_.Push(packet);
  }

  for (auto buffer : all_output_buffers_) {
    free_output_buffers_.Push(buffer);
  }
}

void DecryptorAdapter::CoreCodecRecycleOutputPacket(CodecPacket* packet) {
  if (packet->is_new()) {
    packet->SetIsNew(false);
    return;
  }
  ZX_DEBUG_ASSERT(!packet->is_new());

  const CodecBuffer* buffer = packet->buffer();
  packet->SetBuffer(nullptr);

  free_output_packets_.Push(packet);
  free_output_buffers_.Push(buffer);
}

void DecryptorAdapter::CoreCodecEnsureBuffersNotConfigured(CodecPort port) {
  std::lock_guard<std::mutex> lock(lock_);

  // This adapter should ensure that zero old CodecPacket* or CodecBuffer*
  // remain in this adapter (or below).

  if (port == kInputPort) {
    // There shouldn't be any queued input at this point, but if there is any,
    // fail here even in a release build.
    ZX_ASSERT(input_queue_.empty());

    if (inspect_properties_) {
      inspect_properties_->input = std::nullopt;
    }
  } else {
    ZX_DEBUG_ASSERT(port == kOutputPort);

    // The old all_output_buffers_ are no longer valid.
    all_output_buffers_.clear();
    free_output_buffers_.Reset();
    free_output_packets_.Reset();

    if (inspect_properties_) {
      inspect_properties_->output = std::nullopt;
    }
  }
}

std::unique_ptr<const fuchsia::media::StreamBufferConstraints>
DecryptorAdapter::CoreCodecBuildNewInputConstraints() {
  auto constraints = std::make_unique<fuchsia::media::StreamBufferConstraints>();
  constraints->set_buffer_constraints_version_ordinal(kInputBufferConstraintsVersionOrdinal);
  return constraints;
}

std::unique_ptr<const fuchsia::media::StreamOutputConstraints>
DecryptorAdapter::CoreCodecBuildNewOutputConstraints(
    uint64_t stream_lifetime_ordinal, uint64_t new_output_buffer_constraints_version_ordinal,
    bool buffer_constraints_action_required) {
  auto config = std::make_unique<fuchsia::media::StreamOutputConstraints>();

  config->set_stream_lifetime_ordinal(stream_lifetime_ordinal);

  auto* constraints = config->mutable_buffer_constraints();

  // For the moment, there will be only one StreamOutputConstraints, and it'll
  // need output buffers configured for it.
  ZX_DEBUG_ASSERT(buffer_constraints_action_required);
  config->set_buffer_constraints_action_required(buffer_constraints_action_required);
  constraints->set_buffer_constraints_version_ordinal(
      new_output_buffer_constraints_version_ordinal);

  return config;
}

fuchsia::media::StreamOutputFormat DecryptorAdapter::CoreCodecGetOutputFormat(
    uint64_t stream_lifetime_ordinal, uint64_t new_output_format_details_version_ordinal) {
  fuchsia::media::StreamOutputFormat result;
  result.set_stream_lifetime_ordinal(stream_lifetime_ordinal);
  result.mutable_format_details()->set_format_details_version_ordinal(
      new_output_format_details_version_ordinal);

  // This sets each of format_details, domain, crypto, decrypted.  So far there aren't any fields in
  // DecryptedFormat.
  result.mutable_format_details()->mutable_domain()->crypto().decrypted();

  return result;
}

void DecryptorAdapter::CoreCodecMidStreamOutputBufferReConfigPrepare() {
  // For this adapter, nothing to do here.
}

void DecryptorAdapter::CoreCodecMidStreamOutputBufferReConfigFinish() {
  // For this adapter, nothing to do here.
}

fuchsia::sysmem::BufferMemoryConstraints DecryptorAdapter::GetSecureOutputMemoryConstraints()
    const {
  fuchsia::sysmem::BufferMemoryConstraints constraints;
  constraints.physically_contiguous_required = true;
  constraints.secure_required = true;
  constraints.ram_domain_supported = false;
  constraints.cpu_domain_supported = false;
  constraints.inaccessible_domain_supported = true;

  // This is only the default in the base class; actual decryptors that only output to secure heaps
  // should only list those secure heaps.
  constraints.heap_permitted_count = 1;
  constraints.heap_permitted[0] = fuchsia::sysmem::HeapType::SYSTEM_RAM;

  return constraints;
}

fuchsia_sysmem2::BufferMemoryConstraints DecryptorAdapter::GetSecureOutputMemoryConstraints2()
    const {
  fuchsia::sysmem::BufferMemoryConstraints v1_constraints = GetSecureOutputMemoryConstraints();
  auto constraints_result =
      sysmem::V2CopyFromV1BufferMemoryConstraints(fidl::HLCPPToNatural(v1_constraints));
  // DecryptorAdapter sub-class must provide valid v1 constraints (or preferably, provide valid
  // v2 constraints by overriding GetSecureOutputMemoryConstraints2 instead).
  ZX_ASSERT(constraints_result.is_ok());
  return std::move(constraints_result.value());
}

void DecryptorAdapter::PostSerial(async_dispatcher_t* dispatcher, fit::closure to_run) {
  zx_status_t post_result = async::PostTask(dispatcher, std::move(to_run));
  ZX_ASSERT_MSG(post_result == ZX_OK, "async::PostTask() failed - result: %d", post_result);
}

void DecryptorAdapter::PostToInputProcessingThread(fit::closure to_run) {
  PostSerial(input_processing_loop_.dispatcher(), std::move(to_run));
}

void DecryptorAdapter::QueueInputItem(CodecInputItem input_item) {
  bool is_trigger_needed = false;
  {  // scope lock
    std::lock_guard<std::mutex> lock(lock_);
    // For now we don't worry about avoiding a trigger if we happen to queue
    // when ProcessInput() has removed the last item but ProcessInput() is still
    // running.
    if (!is_process_input_queued_) {
      is_trigger_needed = input_queue_.empty();
      is_process_input_queued_ = is_trigger_needed;
    }
    input_queue_.emplace_back(std::move(input_item));
  }  // ~lock
  if (is_trigger_needed) {
    PostToInputProcessingThread(fit::bind_member(this, &DecryptorAdapter::ProcessInput));
  }
}

void DecryptorAdapter::ProcessInput() {
  {  // scope lock
    std::lock_guard<std::mutex> lock(lock_);
    is_process_input_queued_ = false;
  }  // ~lock
  while (true) {
    CodecInputItem item = DequeueInputItem();
    if (!item.is_valid()) {
      return;
    }

    if (item.is_format_details()) {
      if (!item.format_details().has_domain() || !item.format_details().domain().is_crypto() ||
          !item.format_details().domain().crypto().is_encrypted()) {
        events_->onCoreCodecFailCodec("InputFormatDetails does not include EncryptedFormat");
        return;
      }

      if (!UpdateEncryptionParams(item.format_details().domain().crypto().encrypted())) {
        events_->onCoreCodecFailCodec("Invalid EncryptedFormat");
      }
      continue;
    }

    if (item.is_end_of_stream()) {
      events_->onCoreCodecOutputEndOfStream(false);
      continue;
    }

    ZX_DEBUG_ASSERT(item.is_packet());

    std::optional<CodecPacket*> maybe_output_packet = free_output_packets_.WaitForElement();

    if (!maybe_output_packet) {
      return;
    }
    auto output_packet = *maybe_output_packet;
    ZX_DEBUG_ASSERT(output_packet);

    std::optional<const CodecBuffer*> maybe_output_buffer = free_output_buffers_.WaitForElement();
    if (!maybe_output_buffer) {
      // Return the output_packet to the free list.
      free_output_packets_.Push(output_packet);
      return;
    }
    auto output_buffer = *maybe_output_buffer;
    ZX_DEBUG_ASSERT(output_buffer);

    uint32_t data_length = item.packet()->valid_length_bytes();

    InputBuffer input;
    input.data = item.packet()->buffer()->base() + item.packet()->start_offset();
    input.data_length = data_length;

    OutputBuffer output;
    if (is_secure()) {
      auto checked_data_offset =
          safemath::MakeCheckedNum(output_buffer->vmo_offset()).Cast<uint32_t>();
      if (!checked_data_offset.IsValid()) {
        events_->onCoreCodecFailCodec("Can not convert data offset to unsigned 32-bit value");
        return;
      }

      auto checked_data_length = safemath::MakeCheckedNum(output_buffer->size()).Cast<uint32_t>();
      if (!checked_data_length.IsValid()) {
        events_->onCoreCodecFailCodec("Can not convert data length to unsigned 32-bit value");
        return;
      }

      SecureOutputBuffer secure_output;
      secure_output.vmo = zx::unowned_vmo(output_buffer->vmo());
      secure_output.data_offset = checked_data_offset.ValueOrDie();
      secure_output.data_length = checked_data_length.ValueOrDie();
      output = secure_output;
    } else if (IsCoreCodecMappedBufferUseful(kOutputPort)) {
      auto checked_data_length = safemath::MakeCheckedNum(output_buffer->size()).Cast<uint32_t>();
      if (!checked_data_length.IsValid()) {
        events_->onCoreCodecFailCodec("Can not convert data length to unsigned 32-bit value");
        return;
      }

      ClearOutputBuffer clear_output;
      clear_output.data = output_buffer->base();
      clear_output.data_length = checked_data_length.ValueOrDie();
      output = clear_output;
    } else {
      events_->onCoreCodecFailCodec("Unmapped clear output buffer is unsupported.");
      return;
    }

    auto error = Decrypt(encryption_params_, input, output, output_packet);
    if (error) {
      // Release output buffer and packet since they can be re-used later for a new stream.
      free_output_packets_.Push(output_packet);
      free_output_buffers_.Push(output_buffer);

      OnCoreCodecFailStream(*error);

      // Free the active packet.
      // This is done after the OnStreamFailed event as a temporary kludge so that clients can
      // identify that this packet failed to decrypt. If it was sent before the OnStreamFailed
      // event, the client could assume that this input packet was successfully decrypted and would
      // not make another attempt at decrypting it on subsequent attempts (like after the keys
      // arrive). However, relying on this order of events is hazardous for other implementations of
      // the StreamProcessor protocol.
      events_->onCoreCodecInputPacketDone(std::move(item.packet()));
      return;
    }

    output_packet->SetBuffer(output_buffer);
    output_packet->SetStartOffset(0);
    output_packet->SetValidLengthBytes(data_length);
    if (item.packet()->has_timestamp_ish()) {
      output_packet->SetTimstampIsh(item.packet()->timestamp_ish());
    } else {
      output_packet->ClearTimestampIsh();
    }

    events_->onCoreCodecOutputPacket(output_packet, false, false);
    events_->onCoreCodecInputPacketDone(item.packet());
    // At this point CodecInputItem is holding a packet pointer which may get
    // re-used in a new CodecInputItem, but that's ok since CodecInputItem is
    // going away here.
    //
    // ~item
  }
}

bool DecryptorAdapter::UpdateEncryptionParams(
    const fuchsia::media::EncryptedFormat& encrypted_format) {
  if (encrypted_format.has_scheme()) {
    encryption_params_.scheme = encrypted_format.scheme();
    if (inspect_properties_) {
      inspect_properties_->scheme.Set(encryption_params_.scheme);
    }
  }
  if (encrypted_format.has_key_id()) {
    encryption_params_.key_id = encrypted_format.key_id();
    if (inspect_properties_) {
      inspect_properties_->key_id.Set(encryption_params_.key_id);
    }
  }
  if (encrypted_format.has_init_vector()) {
    encryption_params_.init_vector = encrypted_format.init_vector();
  }
  if (encrypted_format.has_pattern()) {
    encryption_params_.pattern = encrypted_format.pattern();
  }
  if (encrypted_format.has_subsamples()) {
    encryption_params_.subsamples = encrypted_format.subsamples();
  }

  return true;
}

CodecInputItem DecryptorAdapter::DequeueInputItem() {
  std::lock_guard<std::mutex> lock(lock_);
  if (is_stream_failed_ || is_cancelling_input_processing_ || input_queue_.empty()) {
    return CodecInputItem::Invalid();
  }
  CodecInputItem to_ret = std::move(input_queue_.front());
  input_queue_.pop_front();
  return to_ret;
}

void DecryptorAdapter::OnCoreCodecFailStream(fuchsia::media::StreamError error) {
  {  // scope lock
    std::lock_guard<std::mutex> lock(lock_);
    is_stream_failed_ = true;
  }

  events_->onCoreCodecFailStream(error);
}
