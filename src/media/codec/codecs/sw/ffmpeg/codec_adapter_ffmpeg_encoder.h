// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_CODEC_CODECS_SW_FFMPEG_CODEC_ADAPTER_FFMPEG_ENCODER_H_
#define SRC_MEDIA_CODEC_CODECS_SW_FFMPEG_CODEC_ADAPTER_FFMPEG_ENCODER_H_

#include "avcodec_context.h"
#include "codec_adapter_sw.h"

class CodecAdapterFfmpegEncoder : public CodecAdapterSW<AvCodecContext::AVFramePtr> {
 public:
  CodecAdapterFfmpegEncoder(std::mutex& lock, CodecAdapterEvents* codec_adapter_events);
  ~CodecAdapterFfmpegEncoder();

  fuchsia_sysmem2::BufferCollectionConstraints CoreCodecGetBufferCollectionConstraints2(
      CodecPort port, const fuchsia::media::StreamBufferConstraints& stream_buffer_constraints,
      const fuchsia::media::StreamBufferPartialSettings& partial_settings) override;

  void CoreCodecSetBufferCollectionInfo(
      CodecPort port, const fuchsia_sysmem2::BufferCollectionInfo& buffer_collection_info) override;

 protected:
  // Processes input in a loop. Should only execute on input_processing_thread_.
  // Loops for the lifetime of a stream.
  void ProcessInputLoop() override;

  void CleanUpAfterStream() override;

  std::pair<fuchsia::media::FormatDetails, size_t> OutputFormatDetails() override;

 private:
};

#endif  // SRC_MEDIA_CODEC_CODECS_SW_FFMPEG_CODEC_ADAPTER_FFMPEG_ENCODER_H_
