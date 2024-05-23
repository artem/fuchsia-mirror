// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/codec/examples/encode_camera/encoder_client.h"

#include <fidl/fuchsia.sysmem/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.sysmem2/cpp/hlcpp_conversion.h>
#include <lib/async/cpp/task.h>
#include <lib/sysmem-version/sysmem-version.h>

#include <algorithm>
#include <iostream>
#include <random>

namespace {
constexpr uint64_t kInputBufferLifetimeOrdinal = 1;
constexpr uint32_t kInputMinBufferCountForCamping = 1;
constexpr uint32_t kMinOutputBufferSize = 100 * 4096;
constexpr uint32_t kOutputMinBufferCountForCamping = 1;
constexpr char kH264MimeType[] = "video/h264";
constexpr char kH265MimeType[] = "video/h265";
}  // namespace

static void FatalError(std::string message) {
  std::cerr << message << std::endl;
  abort();
}

// Sets the error handler on the provided interface to log an error and abort the process.
template <class T>
static void SetAbortOnError(fidl::InterfacePtr<T>& p, std::string message) {
  p.set_error_handler([message](zx_status_t status) { FatalError(message); });
}

fpromise::result<std::unique_ptr<EncoderClient>, zx_status_t> EncoderClient::Create(
    fuchsia::mediacodec::CodecFactoryHandle codec_factory,
    fuchsia::sysmem2::AllocatorHandle allocator, uint32_t bitrate, uint32_t gop_size,
    const std::string& mime_type) {
  auto encoder = std::unique_ptr<EncoderClient>(new EncoderClient(bitrate, gop_size, mime_type));
  zx_status_t status = encoder->codec_factory_.Bind(std::move(codec_factory));
  if (status != ZX_OK) {
    return fpromise::error(status);
  }

  status = encoder->sysmem_.Bind(std::move(allocator));
  if (status != ZX_OK) {
    return fpromise::error(status);
  }

  return fpromise::ok(std::move(encoder));
}

EncoderClient::EncoderClient(uint32_t bitrate, uint32_t gop_size, const std::string& mime_type)
    : bitrate_(bitrate), gop_size_(gop_size), mime_type_(mime_type) {
  SetAbortOnError(codec_factory_, "fuchsia.mediacodec.CodecFactory disconnected.");
  SetAbortOnError(sysmem_, "fuchsia.sysmem.Allocator disconnected.");
  SetAbortOnError(codec_, "fuchsia.media.StreamProcessor disconnected.");
  SetAbortOnError(input_buffer_collection_, "fuchsia.sysmem.BufferCollection input disconnected.");
  SetAbortOnError(output_buffer_collection_,
                  "fuchsia.sysmem.BufferCollection output disconnected.");

  codec_.events().OnStreamFailed = fit::bind_member<&EncoderClient::OnStreamFailed>(this);
  codec_.events().OnInputConstraints = fit::bind_member<&EncoderClient::OnInputConstraints>(this);
  codec_.events().OnFreeInputPacket = fit::bind_member<&EncoderClient::OnFreeInputPacket>(this);
  codec_.events().OnOutputConstraints = fit::bind_member<&EncoderClient::OnOutputConstraints>(this);
  codec_.events().OnOutputFormat = fit::bind_member<&EncoderClient::OnOutputFormat>(this);
  codec_.events().OnOutputPacket = fit::bind_member<&EncoderClient::OnOutputPacket>(this);
  codec_.events().OnOutputEndOfStream = fit::bind_member<&EncoderClient::OnOutputEndOfStream>(this);
}

EncoderClient::~EncoderClient() {}

zx_status_t EncoderClient::Start(fuchsia::sysmem2::BufferCollectionTokenHandle token,
                                 fuchsia::images2::ImageFormat image_format, uint32_t framerate) {
  if (image_format.pixel_format() != fuchsia::images2::PixelFormat::NV12) {
    std::cout << "Unsupported pixel format" << std::endl;
    return ZX_ERR_INVALID_ARGS;
  }

  std::cout << "Starting encoder at frame rate " << framerate << std::endl;

  auto v2_natural_image_format = fidl::HLCPPToNatural(fidl::Clone(image_format));
  auto v1_natural_image_format_result = sysmem::V1CopyFromV2ImageFormat(v2_natural_image_format);
  if (v1_natural_image_format_result.is_error()) {
    std::cout << "sysmem::V1CopyFromV2ImageFormat failed" << std::endl;
    return ZX_ERR_INVALID_ARGS;
  }
  auto v1_image_format = fidl::NaturalToHLCPP(std::move(v1_natural_image_format_result.value()));

  fuchsia::media::VideoUncompressedFormat uncompressed;
  uncompressed.image_format = std::move(v1_image_format);

  fuchsia::media::VideoFormat video_format;
  video_format.set_uncompressed(uncompressed);

  fuchsia::media::DomainFormat domain;
  fuchsia::media::EncoderSettings encoder_settings;
  domain.set_video(std::move(video_format));

  if (mime_type_ == kH264MimeType) {
    fuchsia::media::H264EncoderSettings h264_settings;
    h264_settings.set_bit_rate(bitrate_);
    h264_settings.set_frame_rate(framerate);
    h264_settings.set_gop_size(gop_size_);
    encoder_settings.set_h264(std::move(h264_settings));
  } else if (mime_type_ == kH265MimeType) {
    fuchsia::media::HevcEncoderSettings hevc_settings;
    hevc_settings.set_bit_rate(bitrate_);
    hevc_settings.set_frame_rate(framerate);
    hevc_settings.set_gop_size(gop_size_);
    encoder_settings.set_hevc(std::move(hevc_settings));
  } else {
    std::cout << "Unsupported codec" << std::endl;
    return ZX_ERR_INVALID_ARGS;
  }

  fuchsia::media::FormatDetails input_details;
  input_details.set_format_details_version_ordinal(0)
      .set_mime_type(mime_type_)
      .set_encoder_settings(std::move(encoder_settings))
      .set_domain(std::move(domain));

  fuchsia::mediacodec::CreateEncoder_Params encoder_params;
  encoder_params.set_input_details(std::move(input_details));

  auto codec_request = codec_.NewRequest();
  codec_factory_->CreateEncoder(std::move(encoder_params), std::move(codec_request));

  input_buffers_token_ = std::move(token);

  return ZX_OK;
}

void EncoderClient::BindAndSyncBufferCollection(
    fuchsia::sysmem2::BufferCollectionPtr& buffer_collection,
    fuchsia::sysmem2::BufferCollectionTokenHandle token,
    fuchsia::sysmem2::BufferCollectionTokenHandle duplicated_token,
    BoundBufferCollectionCallback callback) {
  auto buffer_collection_request = buffer_collection.NewRequest();
  sysmem_->BindSharedCollection(
      std::move(fuchsia::sysmem2::AllocatorBindSharedCollectionRequest{}
                    .set_token(std::move(token))
                    .set_buffer_collection_request(std::move(buffer_collection_request))));

  // After Sync() completes its round trip, we know that sysmem knows about
  // duplicated_token (causally), which is important because we'll shortly
  // send duplicated_token to the codec which will use duplicated_token via
  // a different sysmem channel.
  buffer_collection->Sync(
      [duplicated_token = std::move(duplicated_token),
       callback = std::move(callback)](fuchsia::sysmem2::Node_Sync_Result sync_result) mutable {
        callback(std::move(duplicated_token));
      });
}

// Duplicate passed in token to buffer collection, then bind and sync on it, passing back the
// logical buffer collection and the duplicated token to pass to the next client (the encoder)
void EncoderClient::BindAndSyncBufferCollectionToken(
    fuchsia::sysmem2::BufferCollectionPtr& buffer_collection,
    fuchsia::sysmem2::BufferCollectionTokenHandle token, BoundBufferCollectionCallback callback) {
  fuchsia::sysmem2::BufferCollectionTokenHandle codec_sysmem_token;

  // Bind the passed in client token
  fuchsia::sysmem2::BufferCollectionTokenPtr client_token = token.Bind();

  client_token->Duplicate(std::move(fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest{}
                                        .set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS)
                                        .set_token_request(codec_sysmem_token.NewRequest())));

  // client_token gets converted into a buffer_collection.
  //
  // Start client_token connection and start converting it into a
  // BufferCollection, so we can Sync() the previous Duplicate().
  token = client_token.Unbind();
  BindAndSyncBufferCollection(buffer_collection, std::move(token), std::move(codec_sysmem_token),
                              std::move(callback));
}

void EncoderClient::CreateAndSyncBufferCollection(
    fuchsia::sysmem2::BufferCollectionPtr& buffer_collection,
    BoundBufferCollectionCallback callback) {
  fuchsia::sysmem2::BufferCollectionTokenHandle codec_sysmem_token;

  // Create client_token which will get converted into out_buffer_collection.
  fuchsia::sysmem2::BufferCollectionTokenPtr client_token;
  fidl::InterfaceRequest<fuchsia::sysmem2::BufferCollectionToken> client_token_request =
      client_token.NewRequest();

  client_token->Duplicate(std::move(fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest{}
                                        .set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS)
                                        .set_token_request(codec_sysmem_token.NewRequest())));

  // client_token gets converted into a buffer_collection.
  //
  // Start client_token connection and start converting it into a
  // BufferCollection, so we can Sync() the previous Duplicate().
  sysmem_->AllocateSharedCollection(
      std::move(fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest{}.set_token_request(
          std::move(client_token_request))));

  auto token = client_token.Unbind();
  BindAndSyncBufferCollection(buffer_collection, std::move(token), std::move(codec_sysmem_token),
                              std::move(callback));
}

void EncoderClient::OnInputConstraints(fuchsia::media::StreamBufferConstraints input_constraints) {
  input_constraints_.emplace(std::move(input_constraints));
  BindAndSyncBufferCollectionToken(
      input_buffer_collection_, std::move(input_buffers_token_),
      [this](fuchsia::sysmem2::BufferCollectionTokenHandle codec_sysmem_token) mutable {
        ConfigurePortBufferCollection(
            input_buffer_collection_, std::move(codec_sysmem_token), false,
            kInputBufferLifetimeOrdinal, input_constraints_->buffer_constraints_version_ordinal(),
            [this](auto result) { OnInputBuffersReady(std::move(result)); });
      });
}

void EncoderClient::OnInputBuffersReady(
    fpromise::result<std::pair<fuchsia::sysmem2::BufferCollectionInfo, uint32_t>, zx_status_t>
        result) {
  if (result.is_error()) {
    FatalError("failed to get input buffers");
    return;
  }

  auto ready = result.take_value();
  auto buffer_collection_info = std::move(ready.first);
  input_packet_count_ = ready.second;

  all_input_buffers_.reserve(buffer_collection_info.buffers().size());
  for (uint32_t i = 0; i < buffer_collection_info.buffers().size(); i++) {
    std::unique_ptr<CodecBuffer> local_buffer = CodecBuffer::CreateFromVmo(
        i, std::move(*buffer_collection_info.mutable_buffers()->at(i).mutable_vmo()),
        static_cast<uint32_t>(buffer_collection_info.buffers()[i].vmo_usable_start()),
        static_cast<uint32_t>(buffer_collection_info.settings().buffer_settings().size_bytes()),
        false, buffer_collection_info.settings().buffer_settings().is_physically_contiguous());
    if (!local_buffer) {
      FatalError("CodecBuffer::CreateFromVmo() failed");
    }
    ZX_ASSERT(all_input_buffers_.size() == i);
    all_input_buffers_.push_back(std::move(local_buffer));
  }
}

void EncoderClient::OnFreeInputPacket(fuchsia::media::PacketHeader free_input_packet) {
  if (!free_input_packet.has_packet_index()) {
    FatalError("OnFreeInputPacket(): Packet has no index.");
  }

  // drop packet release fence
  input_packets_queued_.erase(free_input_packet.packet_index());
}

void EncoderClient::QueueInputPacket(uint32_t buffer_index, zx::eventpair release_fence) {
  fuchsia::media::Packet packet;

  packet.set_stream_lifetime_ordinal(kInputBufferLifetimeOrdinal);
  packet.mutable_header()->set_buffer_lifetime_ordinal(kInputBufferLifetimeOrdinal);
  packet.mutable_header()->set_packet_index(buffer_index);
  packet.set_start_offset(0);
  packet.set_valid_length_bytes(
      static_cast<uint32_t>(all_input_buffers_[buffer_index]->size_bytes()));
  packet.set_buffer_index(buffer_index);

  input_packets_queued_.insert({buffer_index, std::move(release_fence)});

  codec_->QueueInputPacket(std::move(packet));
}

void EncoderClient::ConfigurePortBufferCollection(
    fuchsia::sysmem2::BufferCollectionPtr& buffer_collection,
    fuchsia::sysmem2::BufferCollectionTokenHandle token, bool is_output,
    uint64_t new_buffer_lifetime_ordinal, uint64_t buffer_constraints_version_ordinal,
    ConfigurePortBufferCollectionCallback callback) {
  fuchsia::media::StreamBufferPartialSettings settings;
  settings.set_buffer_lifetime_ordinal(new_buffer_lifetime_ordinal);
  settings.set_buffer_constraints_version_ordinal(buffer_constraints_version_ordinal);

  settings.set_sysmem_token(
      fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>(token.TakeChannel()));

  fuchsia::sysmem2::BufferCollectionConstraints constraints;

  if (is_output) {
    constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_READ_OFTEN);
    constraints.mutable_buffer_memory_constraints()->set_min_size_bytes(kMinOutputBufferSize);
    constraints.set_min_buffer_count_for_camping(kOutputMinBufferCountForCamping);
  } else {
    constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_READ);
    constraints.mutable_buffer_memory_constraints()->set_ram_domain_supported(true);
    constraints.set_min_buffer_count_for_camping(kInputMinBufferCountForCamping);
  }

  if (is_output) {
    codec_->SetOutputBufferPartialSettings(std::move(settings));
  } else {
    codec_->SetInputBufferPartialSettings(std::move(settings));
  }

  buffer_collection->SetConstraints(
      std::move(fuchsia::sysmem2::BufferCollectionSetConstraintsRequest{}.set_constraints(
          std::move(constraints))));

  buffer_collection->WaitForAllBuffersAllocated(
      [callback =
           std::move(callback)](fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result
                                    wait_result) mutable {
        if (!wait_result.is_response()) {
          zx_status_t failure_status;
          if (wait_result.is_framework_err()) {
            failure_status = ZX_ERR_INTERNAL;
          } else {
            failure_status = sysmem::V1CopyFromV2Error(fidl::HLCPPToNatural(wait_result.err()));
          }
          callback(fpromise::error(failure_status));
          return;
        }
        auto buffer_collection_info =
            std::move(*wait_result.response().mutable_buffer_collection_info());
        callback(fpromise::ok(
            std::pair(std::move(buffer_collection_info), buffer_collection_info.buffers().size())));
      });
}

void EncoderClient::OnOutputConstraints(
    fuchsia::media::StreamOutputConstraints output_constraints) {
  if (!output_constraints.has_stream_lifetime_ordinal()) {
    FatalError("StreamOutputConstraints missing stream_lifetime_ordinal");
  }
  last_output_constraints_.emplace(std::move(output_constraints));

  // Free the old output buffers, if any.
  all_output_buffers_.clear();

  auto new_output_buffer_lifetime_ordinal = next_output_buffer_lifetime_ordinal_;
  next_output_buffer_lifetime_ordinal_ += 2;

  CreateAndSyncBufferCollection(
      output_buffer_collection_,
      [this, new_output_buffer_lifetime_ordinal](
          fuchsia::sysmem2::BufferCollectionTokenHandle codec_sysmem_token) {
        //
        // Tell the server about output settings.
        //
        const fuchsia::media::StreamBufferConstraints& buffer_constraints =
            last_output_constraints_->buffer_constraints();

        current_output_buffer_lifetime_ordinal_ = new_output_buffer_lifetime_ordinal;

        ConfigurePortBufferCollection(
            output_buffer_collection_, std::move(codec_sysmem_token), true,
            new_output_buffer_lifetime_ordinal,
            buffer_constraints.buffer_constraints_version_ordinal(),
            [this](auto result) { OnOutputBuffersReady(std::move(result)); });
      });
}

void EncoderClient::OnOutputBuffersReady(
    fpromise::result<std::pair<fuchsia::sysmem2::BufferCollectionInfo, uint32_t>, zx_status_t>
        result) {
  if (result.is_error()) {
    FatalError("Failed to get output buffers");
    return;
  }

  auto ready = result.take_value();
  auto buffer_collection_info = std::move(ready.first);
  output_packet_count_ = ready.second;

  all_output_buffers_.reserve(output_packet_count_);
  for (uint32_t i = 0; i < output_packet_count_; i++) {
    std::unique_ptr<CodecBuffer> buffer = CodecBuffer::CreateFromVmo(
        i, std::move(*buffer_collection_info.mutable_buffers()->at(i).mutable_vmo()),
        static_cast<uint32_t>(buffer_collection_info.buffers()[i].vmo_usable_start()),
        static_cast<uint32_t>(buffer_collection_info.settings().buffer_settings().size_bytes()),
        false, buffer_collection_info.settings().buffer_settings().is_physically_contiguous());
    if (!buffer) {
      FatalError("CodecBuffer::Allocate() failed (output)");
    }
    ZX_ASSERT(all_output_buffers_.size() == i);
    all_output_buffers_.push_back(std::move(buffer));
  }

  codec_->CompleteOutputBufferPartialSettings(current_output_buffer_lifetime_ordinal_);
}

void EncoderClient::OnOutputFormat(fuchsia::media::StreamOutputFormat output_format) {}

void EncoderClient::OnOutputPacket(fuchsia::media::Packet output_packet, bool error_detected_before,
                                   bool error_detected_during) {
  output_packet_handler_(all_output_buffers_[output_packet.buffer_index()]->base(),
                         output_packet.valid_length_bytes());

  codec_->RecycleOutputPacket(fidl::Clone(output_packet.header()));
}

void EncoderClient::OnOutputEndOfStream(uint64_t stream_lifetime_ordinal,
                                        bool error_detected_before) {
  std::cout << "end of stream" << std::endl;
}

void EncoderClient::OnStreamFailed(uint64_t stream_lifetime_ordinal,
                                   fuchsia::media::StreamError error) {
  std::cout << "stream_lifetime_ordinal: " << stream_lifetime_ordinal << " error: " << std::hex
            << static_cast<uint32_t>(error);
  FatalError("OnStreamFailed");
}
