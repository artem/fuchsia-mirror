// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/camera/lib/fake_stream/fake_stream_impl.h"

#include <fuchsia/sysmem/cpp/fidl.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

namespace camera {

fpromise::result<std::unique_ptr<FakeStream>, zx_status_t> FakeStream::Create(
    fuchsia::camera3::StreamProperties properties,
    fit::function<void(fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken>)>
        on_set_buffer_collection) {
  auto result = FakeStreamImpl::Create(std::move(properties), std::move(on_set_buffer_collection));
  if (result.is_error()) {
    return fpromise::error(result.error());
  }
  return fpromise::ok(result.take_value());
}

FakeStreamImpl::FakeStreamImpl() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

FakeStreamImpl::~FakeStreamImpl() {
  async::PostTask(loop_.dispatcher(), fit::bind_member(this, &FakeStreamImpl::OnDestruction));
  loop_.Quit();
  loop_.JoinThreads();
}

static zx_status_t Validate(const fuchsia::camera3::StreamProperties& properties) {
  zx_status_t status = ZX_OK;
  if (properties.image_format.coded_width == 0 || properties.image_format.coded_height == 0 ||
      properties.image_format.bytes_per_row == 0) {
    status = ZX_ERR_INVALID_ARGS;
    FX_PLOGS(DEBUG, status) << "Invalid image format dimensions or stride.";
  }
  if (properties.image_format.pixel_format.type == fuchsia::sysmem::PixelFormatType::INVALID) {
    status = ZX_ERR_INVALID_ARGS;
    FX_PLOGS(DEBUG, status) << "Invalid pixel format type.";
  }
  if (properties.frame_rate.numerator == 0 || properties.frame_rate.denominator == 0) {
    status = ZX_ERR_INVALID_ARGS;
    FX_PLOGS(DEBUG, status) << "Invalid frame rate.";
  }
  return status;
}

fpromise::result<std::unique_ptr<FakeStreamImpl>, zx_status_t> FakeStreamImpl::Create(
    fuchsia::camera3::StreamProperties properties,
    fit::function<void(fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken>)>
        on_set_buffer_collection) {
  auto stream = std::make_unique<FakeStreamImpl>();

  zx_status_t status = Validate(properties);
  if (status != ZX_OK) {
    FX_PLOGS(DEBUG, status) << "StreamProperties failed validation.";
    return fpromise::error(status);
  }

  stream->properties_ = std::move(properties);

  stream->on_set_buffer_collection_ = std::move(on_set_buffer_collection);

  ZX_ASSERT(stream->loop_.StartThread("Fake Stream Loop") == ZX_OK);

  return fpromise::ok(std::move(stream));
}

fidl::InterfaceRequestHandler<fuchsia::camera3::Stream> FakeStreamImpl::GetHandler() {
  return fit::bind_member(this, &FakeStreamImpl::OnNewRequest);
}

void FakeStreamImpl::AddFrame(fuchsia::camera3::FrameInfo info) {
  async::PostTask(loop_.dispatcher(), [this, info = std::move(info)]() mutable {
    if (frame_request_) {
      frame_request_(std::move(info));
      frame_request_ = nullptr;
      return;
    }
    frames_.push(std::move(info));
  });
}

void FakeStreamImpl::OnNewRequest(fidl::InterfaceRequest<fuchsia::camera3::Stream> request) {
  if (bindings_.size() > 0) {
    request.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }

  bindings_.AddBinding(this, std::move(request), loop_.dispatcher());
}

void FakeStreamImpl::OnDestruction() { bindings_.CloseAll(ZX_ERR_IO_NOT_PRESENT); }

void FakeStreamImpl::SetCropRegion(std::unique_ptr<fuchsia::math::RectF> region) {
  bindings_.CloseAll(ZX_ERR_NOT_SUPPORTED);
}

void FakeStreamImpl::WatchCropRegion(WatchCropRegionCallback callback) {
  bindings_.CloseAll(ZX_ERR_NOT_SUPPORTED);
}

void FakeStreamImpl::SetResolution(fuchsia::math::Size coded_size) {
  bindings_.CloseAll(ZX_ERR_NOT_SUPPORTED);
}

void FakeStreamImpl::WatchResolution(WatchResolutionCallback callback) {
  bindings_.CloseAll(ZX_ERR_NOT_SUPPORTED);
}

void FakeStreamImpl::SetBufferCollection(
    fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_param) {
  fuchsia::sysmem2::BufferCollectionTokenHandle token(token_param.TakeChannel());

  if (!token) {
    on_set_buffer_collection_(nullptr);
    return;
  }

  auto token_protocol = token.BindSync();
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> client_token;
  fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest dup_request;
  dup_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
  dup_request.set_token_request(client_token.NewRequest());
  token_protocol->Duplicate(std::move(dup_request));
  fuchsia::sysmem2::Node_Sync_Result sync_result;
  token_protocol->Sync(&sync_result);

  if (token_request_) {
    token_request_(fuchsia::sysmem::BufferCollectionTokenHandle(client_token.TakeChannel()));
    token_request_ = nullptr;
  } else {
    token_ = std::move(client_token);
  }

  on_set_buffer_collection_(std::move(token_protocol));
}

void FakeStreamImpl::WatchBufferCollection(WatchBufferCollectionCallback callback) {
  if (token_request_) {
    bindings_.CloseAll(ZX_ERR_BAD_STATE);
  }

  if (token_) {
    callback(fuchsia::sysmem::BufferCollectionTokenHandle(std::move(token_).TakeChannel()));
    token_ = nullptr;
    return;
  }

  token_request_ = std::move(callback);
}

void FakeStreamImpl::WatchOrientation(WatchOrientationCallback callback) {
  callback(fuchsia::camera3::Orientation::UP);
}

void FakeStreamImpl::GetNextFrame(GetNextFrameCallback callback) {
  if (frame_request_) {
    bindings_.CloseAll(ZX_ERR_BAD_STATE);
    return;
  }

  if (frames_.empty()) {
    frame_request_ = std::move(callback);
    return;
  }

  callback(std::move(frames_.front()));
  frames_.pop();
}

void FakeStreamImpl::Rebind(fidl::InterfaceRequest<fuchsia::camera3::Stream> request) {
  request.Close(ZX_ERR_NOT_SUPPORTED);
  bindings_.CloseAll(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace camera
