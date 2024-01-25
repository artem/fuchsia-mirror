// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CAMERA_BIN_USB_DEVICE_UTIL_H_
#define SRC_CAMERA_BIN_USB_DEVICE_UTIL_H_

#include <fuchsia/camera/cpp/fidl.h>
#include <fuchsia/camera3/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/interface_ptr.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include "src/lib/fsl/handles/object_info.h"

namespace camera {

// Safely unbinds a client connection, doing so on the connection's thread if it differs from the
// caller's thread.
template <class T>
inline void Unbind(fidl::InterfacePtr<T>& p) {
  if (!p) {
    return;
  }

  if (p.dispatcher() == async_get_default_dispatcher()) {
    p.Unbind();
    return;
  }

  async::PostTask(p.dispatcher(), [&]() { p.Unbind(); });
}

// Converts a camera2.hal.Config to a camera3.Configuration
inline fpromise::result<fuchsia::camera3::Configuration2, zx_status_t> Convert(
    const fuchsia::camera2::hal::Config& config) {
  if (config.stream_configs.empty()) {
    FX_LOGS(ERROR) << "Config reported no streams.";
    return fpromise::error(ZX_ERR_INTERNAL);
  }
  std::vector<fuchsia::camera3::StreamProperties2> streams;
  for (const auto& stream_config : config.stream_configs) {
    if (stream_config.image_formats.empty()) {
      FX_LOGS(ERROR) << "Stream reported no image formats.";
      return fpromise::error(ZX_ERR_INTERNAL);
    }
    fuchsia::camera3::StreamProperties2 stream_properties;
    stream_properties.set_frame_rate(
        {.numerator = stream_config.frame_rate.frames_per_sec_numerator,
         .denominator = stream_config.frame_rate.frames_per_sec_denominator});
    stream_properties.set_image_format(stream_config.image_formats[0]);
    // TODO(https://fxbug.dev/42128040): Detect ROI support during initialization.
    stream_properties.set_supports_crop_region(true);
    std::vector<fuchsia::math::Size> supported_resolutions;
    for (const auto& format : stream_config.image_formats) {
      supported_resolutions.push_back({.width = static_cast<int32_t>(format.coded_width),
                                       .height = static_cast<int32_t>(format.coded_height)});
    }
    stream_properties.set_supported_resolutions(std::move(supported_resolutions));
    streams.push_back(std::move(stream_properties));
  }
  fuchsia::camera3::Configuration2 ret;
  ret.set_streams(std::move(streams));
  return fpromise::ok(std::move(ret));
}

// Converts a valid camera3.StreamProperties2 to a camera3.StreamProperties, dropping the
// |supported_resolutions| field.
inline fuchsia::camera3::StreamProperties Convert(
    const fuchsia::camera3::StreamProperties2& properties) {
  return {.image_format = properties.image_format(),
          .frame_rate = properties.frame_rate(),
          .supports_crop_region = properties.supports_crop_region()};
}

// Represents the mute state of a device as defined by the Device.WatchMuteState API.
struct MuteState {
  bool software_muted = false;
  bool hardware_muted = false;
  // Returns true iff any mute is active.
  bool muted() const { return software_muted || hardware_muted; }
  bool operator==(const MuteState& other) const {
    return other.software_muted == software_muted && other.hardware_muted == hardware_muted;
  }
};

// Wraps an async::Wait and frame release fence such that object lifetime requirements are enforced
// automatically, namely that the waited-upon object must outlive the wait object.
class FrameWaiter {
 public:
  FrameWaiter(async_dispatcher_t* dispatcher, std::vector<zx::eventpair> fences,
              fit::closure signaled)
      : fences_(std::move(fences)),
        pending_fences_(fences_.size()),
        signaled_(std::move(signaled)) {
    for (auto& fence : fences_) {
      waits_.push_back(std::make_unique<async::WaitOnce>(fence.get(), ZX_EVENTPAIR_PEER_CLOSED, 0));
      waits_.back()->Begin(dispatcher, fit::bind_member(this, &FrameWaiter::Handler));
    }
  }
  ~FrameWaiter() {
    for (auto& wait : waits_) {
      wait->Cancel();
    }
    signaled_ = nullptr;
    fences_.clear();
  }

 private:
  void Handler(async_dispatcher_t* dispatcher, async::WaitOnce* wait, zx_status_t status,
               const zx_packet_signal_t* signal) {
    if (status != ZX_OK) {
      return;
    }
    if (--pending_fences_ > 0) {
      return;
    }
    // |signaled_| may delete |this|, so move it to a local before calling it. This ensures captures
    // are persisted for the duration of the callback.
    auto signaled = std::move(signaled_);
    signaled();
  }
  std::vector<zx::eventpair> fences_;
  std::vector<std::unique_ptr<async::WaitOnce>> waits_;
  size_t pending_fences_;
  fit::closure signaled_;
};

template <typename T, typename Enable = void>
struct IsFidlChannelWrapper : std::false_type {};

template <typename T>
struct IsFidlChannelWrapper<fidl::InterfaceHandle<T>> : std::true_type {};

template <typename T>
struct IsFidlChannelWrapper<fidl::InterfacePtr<T>> : std::true_type {};

template <typename T>
struct IsFidlChannelWrapper<fidl::SynchronousInterfacePtr<T>> : std::true_type {};

template <typename T>
struct IsFidlChannelWrapper<fidl::InterfaceRequest<T>> : std::true_type {};

template <typename T>
struct IsFidlChannelWrapper<fidl::Binding<T>> : std::true_type {};

template <typename T>
inline constexpr bool IsFidlChannelWrapperV = IsFidlChannelWrapper<T>::value;

template <typename T>
inline zx_handle_t GetFidlChannelHandle(const T& fidl) {
  static_assert(IsFidlChannelWrapperV<T>, "'fidl' must be one one of the fidl channel wrappers");
  return fidl.channel().get();
}

template <typename T>
inline zx_koid_t GetKoid(const T& fidl) {
  return fsl::GetKoid(GetFidlChannelHandle(fidl));
}

template <typename T>
inline zx_koid_t GetRelatedKoid(const T& fidl) {
  return fsl::GetRelatedKoid(GetFidlChannelHandle(fidl));
}

template <typename T>
inline std::pair<zx_koid_t, zx_koid_t> GetKoids(const T& fidl) {
  return fsl::GetKoids(GetFidlChannelHandle(fidl));
}

}  // namespace camera

#endif  // SRC_CAMERA_BIN_USB_DEVICE_UTIL_H_
