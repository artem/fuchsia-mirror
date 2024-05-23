
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CAMERA_LIB_FAKE_CONTROLLER_FAKE_CONTROLLER_H_
#define SRC_CAMERA_LIB_FAKE_CONTROLLER_FAKE_CONTROLLER_H_

#include <fuchsia/camera2/hal/cpp/fidl.h>
#include <fuchsia/camera3/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fpromise/result.h>
#include <zircon/status.h>

#include <memory>
#include <queue>
#include <vector>

#include "src/camera/lib/fake_legacy_stream/fake_legacy_stream.h"

class FakeController : public fuchsia::camera2::hal::Controller {
 public:
  FakeController();
  ~FakeController() override;
  static fpromise::result<std::unique_ptr<FakeController>, zx_status_t> Create(
      fidl::InterfaceRequest<fuchsia::camera2::hal::Controller> request,
      fuchsia::sysmem2::AllocatorHandle allocator);
  static std::vector<fuchsia::camera2::hal::Config> GetDefaultConfigs();
  zx_status_t SendFrameViaLegacyStream(fuchsia::camera2::FrameAvailableInfo info);
  bool LegacyStreamBufferIsOutstanding(uint32_t id);

 private:
  // |fuchsia::camera2::hal::Controller|
  void GetNextConfig(fuchsia::camera2::hal::Controller::GetNextConfigCallback callback) override;
  void CreateStream(uint32_t config_index, uint32_t stream_index, uint32_t image_format_index,
                    fidl::InterfaceRequest<fuchsia::camera2::Stream> stream) override;
  void EnableStreaming() override;
  void DisableStreaming() override;
  void GetDeviceInfo(fuchsia::camera2::hal::Controller::GetDeviceInfoCallback callback) override;

  async::Loop loop_;
  uint32_t get_configs_call_count_ = 0;
  fidl::Binding<fuchsia::camera2::hal::Controller> binding_;
  std::unique_ptr<camera::FakeLegacyStream> stream_;
  fuchsia::sysmem2::AllocatorPtr allocator_;
  bool streaming_enabled_ = true;
};

#endif  // SRC_CAMERA_LIB_FAKE_CONTROLLER_FAKE_CONTROLLER_H_
