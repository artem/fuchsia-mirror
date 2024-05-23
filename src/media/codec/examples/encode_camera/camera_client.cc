// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/codec/examples/encode_camera/camera_client.h"

#include <fidl/fuchsia.sysmem/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.sysmem2/cpp/hlcpp_conversion.h>
#include <lib/async-loop/default.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <zircon/types.h>

#include <iostream>

static void FatalError(std::string message) {
  std::cerr << message << std::endl;
  abort();
}

// Sets the error handler on the provided interface to log an error and abort the process.
template <class T>
static void SetAbortOnError(fidl::InterfacePtr<T>& p, std::string message) {
  p.set_error_handler([message](zx_status_t status) { FatalError(message); });
}

CameraClient::CameraClient(bool list_configs, uint32_t config_index, uint32_t stream_index)
    : list_configs_(list_configs), config_index_(config_index), stream_index_(stream_index) {
  SetAbortOnError(watcher_, "fuchsia.camera3.DeviceWatcher disconnected.");
  SetAbortOnError(allocator_, "fuchsia.sysmem.Allocator disconnected.");
  SetAbortOnError(device_, "fuchsia.camera3.Device disconnected.");
}

CameraClient::~CameraClient() {}

fpromise::result<std::unique_ptr<CameraClient>, zx_status_t> CameraClient::Create(
    fuchsia::camera3::DeviceWatcherHandle watcher, fuchsia::sysmem2::AllocatorHandle allocator,
    bool list_configs, uint32_t config_index, uint32_t stream_index) {
  auto cycler =
      std::unique_ptr<CameraClient>(new CameraClient(list_configs, config_index, stream_index));

  zx_status_t status = cycler->watcher_.Bind(std::move(watcher));
  if (status != ZX_OK) {
    return fpromise::error(status);
  }

  status = cycler->allocator_.Bind(std::move(allocator));
  if (status != ZX_OK) {
    return fpromise::error(status);
  }

  cycler->watcher_->WatchDevices(
      fit::bind_member<&CameraClient::WatchDevicesCallback>(cycler.get()));

  return fpromise::ok(std::move(cycler));
}

void CameraClient::SetHandlers(CameraClient::AddCollectionHandler on_add_collection,
                               CameraClient::RemoveCollectionHandler on_remove_collection,
                               CameraClient::ShowBufferHandler on_show_buffer,
                               CameraClient::MuteStateHandler on_mute_changed) {
  add_collection_handler_ = std::move(on_add_collection);
  remove_collection_handler_ = std::move(on_remove_collection);
  show_buffer_handler_ = std::move(on_show_buffer);
  mute_state_handler_ = std::move(on_mute_changed);
}

void CameraClient::WatchDevicesCallback(std::vector<fuchsia::camera3::WatchDevicesEvent> events) {
  for (auto& event : events) {
    if (event.is_added()) {
      // Connect to device.
      watcher_->ConnectToDevice(event.added(), device_.NewRequest());

      // Watch for mute changes.
      device_->WatchMuteState(fit::bind_member<&CameraClient::WatchMuteStateHandler>(this));

      // Fetch camera configurations
      device_->GetConfigurations(
          [this](std::vector<fuchsia::camera3::Configuration> configurations) {
            configurations_ = std::move(configurations);

            if (list_configs_) {
              DumpConfigs();
              exit(0);
            }

            ZX_ASSERT(configurations_.size() > config_index_);
            ZX_ASSERT(!configurations_[config_index_].streams.empty());
            device_->SetCurrentConfiguration(config_index_);
            device_->WatchCurrentConfiguration(
                [this](uint32_t index) { ConnectToStream(config_index_, stream_index_); });
          });
    }
  }

  // Hanging get.
  watcher_->WatchDevices(fit::bind_member<&CameraClient::WatchDevicesCallback>(this));
}

void CameraClient::DumpConfigs() {
  for (size_t i = 0; i < configurations_.size(); i++) {
    auto& c = configurations_[i];
    std::cout << "Configuration " << i << std::endl;
    for (size_t j = 0; j < c.streams.size(); j++) {
      auto& s = c.streams[j];
      std::cout << "Stream " << j << std::endl;
      std::cout << "  " << s.image_format.display_width << "x" << s.image_format.display_height
                << std::endl;
      std::cout << "  framerate " << s.frame_rate.numerator << "/" << s.frame_rate.denominator
                << std::endl;
    }
    std::cout << std::endl;
  }
}

void CameraClient::WatchMuteStateHandler(bool software_muted, bool hardware_muted) {
  mute_state_handler_(software_muted | hardware_muted);
  device_->WatchMuteState(fit::bind_member<&CameraClient::WatchMuteStateHandler>(this));
}

void CameraClient::ConnectToStream(uint32_t config_index, uint32_t stream_index) {
  ZX_ASSERT(configurations_.size() > config_index);
  ZX_ASSERT(configurations_[config_index].streams.size() > stream_index);
  auto image_format = configurations_[config_index].streams[stream_index].image_format;

  auto v1_natural_image_format = fidl::HLCPPToNatural(fidl::Clone(image_format));
  auto v2_natural_image_format_result = sysmem::V2CopyFromV1ImageFormat(v1_natural_image_format);
  ZX_ASSERT(v2_natural_image_format_result.is_ok());
  auto v2_image_format = fidl::NaturalToHLCPP(std::move(v2_natural_image_format_result.value()));

  auto frame_rate = configurations_[config_index].streams[stream_index].frame_rate;

  // Connect to specific stream
  StreamInfo new_stream_info;
  stream_infos_.emplace(stream_index, std::move(new_stream_info));
  auto& stream = stream_infos_[stream_index].stream;
  auto stream_request = stream.NewRequest();

  // Allocate buffer collection
  fuchsia::sysmem2::BufferCollectionTokenHandle token_orig;
  allocator_->AllocateSharedCollection(
      std::move(fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest{}.set_token_request(
          token_orig.NewRequest())));
  stream->SetBufferCollection(
      fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>(token_orig.TakeChannel()));
  stream->WatchBufferCollection(
      [this, v2_image_format = std::move(v2_image_format), stream_index, frame_rate,
       &stream](fuchsia::sysmem::BufferCollectionTokenHandle token_back) mutable {
        if (add_collection_handler_) {
          auto& stream_info = stream_infos_[stream_index];
          stream_info.add_collection_handler_returned_value = add_collection_handler_(
              fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken>(
                  token_back.TakeChannel()),
              std::move(v2_image_format), frame_rate);
        } else {
          token_back.BindSync()->Close();
        }
        // Kick start the stream
        stream->GetNextFrame([this](fuchsia::camera3::FrameInfo frame_info) {
          OnNextFrame(stream_index_, std::move(frame_info));
        });
      });

  device_->ConnectToStream(stream_index, std::move(stream_request));
}

void CameraClient::OnNextFrame(uint32_t stream_index, fuchsia::camera3::FrameInfo frame_info) {
  if (show_buffer_handler_) {
    auto& stream_info = stream_infos_[stream_index];
    show_buffer_handler_(stream_info.add_collection_handler_returned_value, frame_info.buffer_index,
                         std::move(frame_info.release_fence));
  } else {
    frame_info.release_fence.reset();
  }
  auto& stream = stream_infos_[stream_index].stream;
  stream->GetNextFrame([this, stream_index](fuchsia::camera3::FrameInfo frame_info) {
    OnNextFrame(stream_index, std::move(frame_info));
  });
}
