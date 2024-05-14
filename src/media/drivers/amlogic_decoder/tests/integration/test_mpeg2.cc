// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/compiler.h>

#include <future>

#include <gtest/gtest.h>

#include "amlogic-video.h"
#include "mpeg12_decoder.h"
#include "tests/test_basic_client.h"
#include "tests/test_support.h"
#include "vdec1.h"

namespace amlogic_decoder {
namespace test {

class FakeOwner : public AmlogicVideo::Owner {
 public:
  // AmlogicVideo::Owner implementation.
  void SetThreadProfile(zx::unowned_thread thread, ThreadRole role) const override {}
};

class TestMpeg2 {
 public:
  static void Decode(bool use_parser) {
    FakeOwner owner;
    auto video = std::make_unique<AmlogicVideo>(&owner);
    ASSERT_TRUE(video);

    video->SetDeviceType(DeviceType::kSM1);
    EXPECT_EQ(ZX_OK, video->InitRegisters(TestSupport::parent_device()));

    TestBasicClient client;
    {
      std::lock_guard<std::mutex> lock(video->video_decoder_lock_);
      video->SetDefaultInstance(std::make_unique<Mpeg12Decoder>(video.get(), &client), false);
    }

    uint32_t stream_buffer_size = use_parser ? PAGE_SIZE : (PAGE_SIZE * 1024);
    EXPECT_EQ(ZX_OK, video->InitializeStreamBuffer(use_parser, stream_buffer_size,
                                                   /*is_secure=*/false));

    video->InitializeInterrupts();
    std::promise<void> wait_valid;
    uint32_t frame_count = 0;
    {
      std::lock_guard<std::mutex> lock(video->video_decoder_lock_);

      client.SetFrameReadyNotifier(
          [&video, &frame_count, &wait_valid](std::shared_ptr<VideoFrame> frame) {
#if DUMP_VIDEO_TO_FILE
            DumpVideoFrameToFile(frame, "/tmp/bearmpeg2.yuv");
#endif
            ++frame_count;
            if (frame_count == 28)
              wait_valid.set_value();
            // This is called from the interrupt handler, which already holds the lock.
            video->AssertVideoDecoderLockHeld();
            video->video_decoder_->ReturnFrame(frame);
          });
      EXPECT_EQ(ZX_OK, video->video_decoder_->Initialize());
    }

    auto bear_mpeg2 = TestSupport::LoadFirmwareFile("video_test_data/bear.mpeg2");
    ASSERT_NE(nullptr, bear_mpeg2);
    if (use_parser) {
      EXPECT_EQ(ZX_OK, video->InitializeEsParser());
      EXPECT_EQ(ZX_OK, video->parser()->ParseVideo(bear_mpeg2->ptr, bear_mpeg2->size));
      EXPECT_EQ(ZX_OK, video->parser()->WaitForParsingCompleted(ZX_SEC(10)));
    } else {
      video->core_->InitializeDirectInput();
      EXPECT_EQ(ZX_OK, video->ProcessVideoNoParser(bear_mpeg2->ptr, bear_mpeg2->size));
    }

    EXPECT_EQ(std::future_status::ready, wait_valid.get_future().wait_for(std::chrono::seconds(1)));

    video.reset();
  }
};

TEST(MPEG2, Decode) { TestMpeg2::Decode(true); }

TEST(MPEG2, DecodeNoParser) { TestMpeg2::Decode(false); }

}  // namespace test
}  // namespace amlogic_decoder
