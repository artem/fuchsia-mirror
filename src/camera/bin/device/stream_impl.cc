// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/camera/bin/device/stream_impl.h"

#include <lib/async/cpp/task.h>
#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include "src/camera/bin/device/messages.h"
#include "src/camera/bin/device/size_util.h"
#include "src/camera/bin/device/util.h"
#include "src/lib/fsl/handles/object_info.h"

namespace camera {

StreamImpl::StreamImpl(async_dispatcher_t* dispatcher, MetricsReporter::StreamRecord& record,
                       const fuchsia::camera3::StreamProperties2& properties,
                       const fuchsia::camera2::hal::StreamConfig& legacy_config,
                       fidl::InterfaceRequest<fuchsia::camera3::Stream> request,
                       StreamRequestedCallback on_stream_requested,
                       BuffersRequestedCallback on_buffers_requested, fit::closure on_no_clients,
                       std::optional<std::string> description,
                       std::unique_ptr<MetricsReporter::FailureTestRecord> streaming_failure_record)
    : dispatcher_(dispatcher),
      record_(record),
      properties_(properties),
      legacy_config_(legacy_config),
      on_stream_requested_(std::move(on_stream_requested)),
      on_buffers_requested_(std::move(on_buffers_requested)),
      on_no_clients_(std::move(on_no_clients)),
      description_(description.value_or("<unknown>")),
      streaming_failure_tracker_{.record = std::move(streaming_failure_record)} {
  legacy_stream_.set_error_handler(fit::bind_member(this, &StreamImpl::OnLegacyStreamDisconnected));
  legacy_stream_.events().OnFrameAvailable = fit::bind_member(this, &StreamImpl::OnFrameAvailable);
  current_resolution_ = ConvertToSize(properties.image_format());
  OnNewRequest(std::move(request));
}

StreamImpl::~StreamImpl() { CheckStreamingFailure(); }

void StreamImpl::CloseAllClients(zx_status_t status) {
  while (clients_.size() > 1) {
    auto& [id, client] = *clients_.begin();
    client->CloseConnection(status);
  }

  if (clients_.size() == 1) {
    // After last client has been removed, on_no_clients_ will run and potentially delete 'this' so
    // handle last client on it's own and don't touch 'this' after.
    clients_.begin()->second->CloseConnection(status);
  }
}

void StreamImpl::SetMuteState(MuteState mute_state) {
  TRACE_DURATION("camera", "StreamImpl::SetMuteState");
  // Before updating the mute state, check to see if the current window has accumulated a failure.
  CheckStreamingFailure();
  mute_state_ = mute_state;
  // On either transition, invalidate existing frames and reset the failure tracker.
  for (auto& [id, client] : clients_) {
    client->ClearFrames();
  }
  streaming_failure_tracker_.Reset();
}

fpromise::scope& StreamImpl::Scope() { return scope_; }

void StreamImpl::OnNewRequest(fidl::InterfaceRequest<fuchsia::camera3::Stream> request) {
  TRACE_DURATION("camera", "StreamImpl::OnNewRequest");
  auto client = std::make_unique<Client>(*this, client_id_next_, std::move(request));
  client->ReceiveResolution(current_resolution_);
  client->ReceiveCropRegion(nullptr);
  clients_.emplace(client_id_next_++, std::move(client));
}

void StreamImpl::OnLegacyStreamDisconnected(zx_status_t status) {
  FX_PLOGS(ERROR, status) << description_ << ":Legacy Stream disconnected unexpectedly.";
  clients_.clear();
  on_no_clients_();
}

void StreamImpl::RemoveClient(uint64_t id) {
  TRACE_DURATION("camera", "StreamImpl::RemoveClient");
  clients_.erase(id);
  if (clients_.empty()) {
    on_no_clients_();
  }
}

void StreamImpl::OnFrameAvailable(fuchsia::camera2::FrameAvailableInfo info) {
  TRACE_DURATION("camera", "StreamImpl::OnFrameAvailable");
  if (info.metadata.has_timestamp()) {
    TRACE_FLOW_END("camera", "camera_stream_on_frame_available", info.metadata.timestamp());
  }
  record_.FrameReceived();
  ++streaming_failure_tracker_.frames_received_in_window;
  CheckStreamingFailure();

  if (info.frame_status != fuchsia::camera2::FrameStatus::OK) {
    FX_LOGS(WARNING) << description_
                     << ": Driver reported a bad frame. This will not be reported to clients.";
    auto reason = cobalt::FrameDropReason::kInvalidFrame;
    if (info.frame_status == fuchsia::camera2::FrameStatus::ERROR_BUFFER_FULL) {
      reason = cobalt::FrameDropReason::kNoMemory;
    }
    record_.FrameDropped(reason);
    legacy_stream_->AcknowledgeFrameError();
    return;
  }

  if (frame_waiters_.find(info.buffer_id) != frame_waiters_.end()) {
    FX_LOGS(WARNING) << description_
                     << ": Driver sent a frame that was already in use (ID = " << info.buffer_id
                     << "). This frame will not be sent to clients.";
    record_.FrameDropped(cobalt::FrameDropReason::kFrameIdInUse);
    legacy_stream_->ReleaseFrame(info.buffer_id);
    return;
  }

  if (!info.metadata.has_timestamp()) {
    FX_LOGS(WARNING)
        << description_
        << ": Driver sent a frame without a timestamp. This frame will not be sent to clients.";
    record_.FrameDropped(cobalt::FrameDropReason::kInvalidTimestamp);
    legacy_stream_->ReleaseFrame(info.buffer_id);
    return;
  }

  uint64_t capture_timestamp = 0;
  if (info.metadata.has_capture_timestamp()) {
    capture_timestamp = info.metadata.capture_timestamp();
  } else {
    FX_LOGS(INFO) << description_ << ": Driver sent a frame without a capture timestamp.";
  }

  // Discard any spurious frames received while muted.
  if (mute_state_.muted()) {
    record_.FrameDropped(cobalt::FrameDropReason::kMuted);
    legacy_stream_->ReleaseFrame(info.buffer_id);
    return;
  }

  // The frame is valid and camera is unmuted, so increment the frame counter.
  ++frame_counter_;

  // Discard the frame if there are too many frames outstanding.
  // TODO(https://fxbug.dev/42143447): Recycle LRU frames.
  if (frame_waiters_.size() == max_camping_buffers_) {
    record_.FrameDropped(cobalt::FrameDropReason::kTooManyFramesInFlight);
    legacy_stream_->ReleaseFrame(info.buffer_id);
    return;
  }

  // Construct the frame info and create a release fence per client.
  std::vector<zx::eventpair> fences;
  for (auto& [id, client] : clients_) {
    if (!client->Participant()) {
      continue;
    }
    zx::eventpair fence;
    zx::eventpair release_fence;
    ZX_ASSERT(zx::eventpair::create(0u, &fence, &release_fence) == ZX_OK);
    fences.push_back(std::move(fence));
    fuchsia::camera3::FrameInfo2 frame;
    frame.set_buffer_index(info.buffer_id);
    frame.set_frame_counter(frame_counter_);
    frame.set_timestamp(info.metadata.timestamp());
    frame.set_capture_timestamp(capture_timestamp);
    frame.set_release_fence(std::move(release_fence));
    client->AddFrame(std::move(frame));
  }

  // No participating clients exist. Release the frame immediately.
  if (fences.empty()) {
    record_.FrameDropped(cobalt::FrameDropReason::kNoClient);
    legacy_stream_->ReleaseFrame(info.buffer_id);
    return;
  }

  // Queue a waiter so that when the client end of the fence is released, the frame is released back
  // to the driver.
  ZX_ASSERT(frame_waiters_.size() <= max_camping_buffers_);
  frame_waiters_[info.buffer_id] =
      std::make_unique<FrameWaiter>(dispatcher_, std::move(fences), [this, index = info.buffer_id] {
        legacy_stream_->ReleaseFrame(index);
        frame_waiters_.erase(index);
      });
}

void StreamImpl::SetBufferCollection(
    uint64_t id, fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_handle) {
  TRACE_DURATION("camera", "StreamImpl::SetBufferCollection");
  auto it = clients_.find(id);
  if (it == clients_.end()) {
    FX_LOGS(ERROR) << description_ << ": Client " << id << " not found.";
    if (token_handle) {
      token_handle.BindSync()->Release();
    }
    ZX_DEBUG_ASSERT(false);
    return;
  }
  bool legacy_stream_needs_start = false;
  auto& client = it->second;
  client->Participant() = !!token_handle;

  if (token_handle) {
    token_handle.BindSync()->Release();
  }

  frame_waiters_.clear();

  if (!legacy_stream_) {
    // Connect to stream. Instead of trying to configure the stream to begin with the specified
    // format, the request is enqueued as the first command on the channel. This reduces complexity
    // in the controller.
    on_stream_requested_(legacy_stream_.NewRequest(), 0);
    legacy_stream_->SetImageFormat(legacy_stream_format_index_, [](auto) {});
    legacy_stream_needs_start = true;
  }

  legacy_stream_->GetBuffers(
      [this, legacy_stream_needs_start](fuchsia::sysmem::BufferCollectionTokenHandle token_handle) {
        // Duplicate and send each client a token.
        std::map<uint64_t, fuchsia::sysmem2::BufferCollectionTokenHandle> client_tokens;
        fuchsia::sysmem2::BufferCollectionTokenPtr token =
            fuchsia::sysmem2::BufferCollectionTokenHandle(token_handle.TakeChannel()).Bind();
        for (auto& client_i : clients_) {
          if (client_i.second->Participant()) {
            fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest dup_request;
            dup_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
            dup_request.set_token_request(client_tokens[client_i.first].NewRequest());
            token->Duplicate(std::move(dup_request));
          }
        }
        token->Sync([this, token = std::move(token), client_tokens = std::move(client_tokens),
                     legacy_stream_needs_start](fuchsia::sysmem2::Node_Sync_Result result) mutable {
          for (auto& [id, token] : client_tokens) {
            auto it = clients_.find(id);
            if (it == clients_.end()) {
              token.BindSync()->Release();
            } else {
              it->second->ReceiveBufferCollection(std::move(token));
            }
          }
          on_buffers_requested_(std::move(token), [this](uint32_t max_camping_buffers) {
            FX_LOGS(INFO) << description_ << ": max camping buffers = " << max_camping_buffers;
            max_camping_buffers_ = max_camping_buffers;
          });
          RestoreLegacyStreamState();
          if (legacy_stream_needs_start) {
            legacy_stream_->Start();
            streaming_failure_tracker_.Reset();
          }
        });
      });
}

void StreamImpl::SetResolution(uint64_t id, fuchsia::math::Size coded_size) {
  TRACE_DURATION("camera", "StreamImpl::SetResolution");
  auto it = clients_.find(id);
  if (it == clients_.end()) {
    FX_LOGS(ERROR) << description_ << ": Client " << id << " not found.";
    ZX_DEBUG_ASSERT(false);
    return;
  }
  auto& client = it->second;

  // Begin with the full resolution.
  auto best_size = ConvertToSize(properties_.image_format());
  if (coded_size.width > best_size.width || coded_size.height > best_size.height) {
    client->CloseConnection(ZX_ERR_INVALID_ARGS);
    return;
  }

  // Examine all supported resolutions, preferring those that cover the requested resolution but
  // have fewer pixels, breaking ties by picking the one with a smaller width.
  uint32_t best_index = 0;
  for (uint32_t i = 0; i < legacy_config_.image_formats.size(); ++i) {
    auto size = ConvertToSize(legacy_config_.image_formats[i]);
    bool contains_request = size.width >= coded_size.width && size.height >= coded_size.height;
    bool smaller_size = size.width * size.height < best_size.width * best_size.height;
    bool equal_size = size.width * size.height == best_size.width * best_size.height;
    bool smaller_width = size.width < best_size.width;
    if (contains_request && (smaller_size || (equal_size && smaller_width))) {
      best_size = size;
      best_index = i;
    }
  }

  // Save the selected image format, and set it on the stream if bound.
  legacy_stream_format_index_ = best_index;
  if (legacy_stream_) {
    legacy_stream_->SetImageFormat(legacy_stream_format_index_, [this](zx_status_t status) {
      if (status != ZX_OK) {
        FX_PLOGS(ERROR, status) << description_ << ": Unexpected response from driver.";
        while (!clients_.empty()) {
          auto it = clients_.begin();
          it->second->CloseConnection(ZX_ERR_INTERNAL);
          clients_.erase(it);
        }
        on_no_clients_();
        return;
      }
    });
  }
  current_resolution_ = best_size;

  // Inform clients of the resolution change.
  for (auto& [id, client] : clients_) {
    client->ReceiveResolution(best_size);
  }
}

void StreamImpl::SetCropRegion(uint64_t id, std::unique_ptr<fuchsia::math::RectF> region) {
  TRACE_DURATION("camera", "StreamImpl::SetCropRegion");
  if (legacy_stream_) {
    float x_min = 0.0f;
    float y_min = 0.0f;
    float x_max = 1.0f;
    float y_max = 1.0f;
    if (region) {
      x_min = region->x;
      y_min = region->y;
      x_max = x_min + region->width;
      y_max = y_min + region->height;
    }
    legacy_stream_->SetRegionOfInterest(x_min, y_min, x_max, y_max, [](zx_status_t status) {
      // TODO(https://fxbug.dev/42128040): Make this an error once RegionOfInterest support is known
      // at init time. FX_PLOGS(WARNING, status) << "Stream does not support crop region.";
    });
  }
  current_crop_region_ = std::move(region);

  // Inform clients of the resolution change.
  for (auto& [id, client] : clients_) {
    std::unique_ptr<fuchsia::math::RectF> region;
    if (current_crop_region_) {
      region = std::make_unique<fuchsia::math::RectF>(*current_crop_region_);
    }
    client->ReceiveCropRegion(std::move(region));
  }
}

void StreamImpl::RestoreLegacyStreamState() {
  TRACE_DURATION("camera", "StreamImpl::RestoreLegacyStreamState");
  // Note that image format does not need restoration as it is passed to the driver during creation.
  if (current_crop_region_) {
    legacy_stream_->SetRegionOfInterest(current_crop_region_->x, current_crop_region_->y,
                                        current_crop_region_->x + current_crop_region_->width,
                                        current_crop_region_->y + current_crop_region_->height,
                                        [](zx_status_t) {});
  }
}

void StreamImpl::CheckStreamingFailure() {
  TRACE_DURATION("camera", "StreamImpl::CheckStreamingFailure");

  // Minimum streaming duration for which the frame counter will be used to indicate failures.
  constexpr auto kMinimumWindow = zx::min(1);

  auto window_duration = zx::clock::get_monotonic() - streaming_failure_tracker_.window_start;
  if (mute_state_.muted() || window_duration < kMinimumWindow) {
    // If the camera is muted, or only recently began streaming, the corresponding frame count is
    // not a good indicator for streaming failures.
    return;
  }

  // Fraction of expected frames actually received, below which a window is considered a failure.
  // The precise value is not particularly important, as most successful windows will see near 100%
  // of frames received, while most failed windows will be near 0%.
  constexpr float kFailureThreshold = 0.5f;

  float expected_frames = static_cast<float>(window_duration.to_secs()) *
                          static_cast<float>(properties_.frame_rate().numerator) /
                          static_cast<float>(properties_.frame_rate().denominator);
  if (static_cast<float>(streaming_failure_tracker_.frames_received_in_window) / expected_frames <
      kFailureThreshold) {
    FX_LOGS(WARNING) << description_ << ": possible streaming failure - expected "
                     << expected_frames << " frames during the last " << window_duration.to_secs()
                     << " seconds but only received "
                     << streaming_failure_tracker_.frames_received_in_window << " from driver";
    if (streaming_failure_tracker_.record) {
      // Mark the record as a failure and immediately report it.
      streaming_failure_tracker_.record->SetFailureState(true);
      streaming_failure_tracker_.record = nullptr;
    }
  }

  // Begin a new window to avoid failures being lost in the noise of long-lived streams.
  streaming_failure_tracker_.Reset();
}

}  // namespace camera
