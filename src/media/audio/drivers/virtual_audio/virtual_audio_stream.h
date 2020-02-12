// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_STREAM_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_STREAM_H_

#include <fuchsia/virtualaudio/cpp/fidl.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/simple-audio-stream/simple-audio-stream.h>

#include <deque>

#include <audio-proto/audio-proto.h>
#include <fbl/ref_ptr.h>

namespace virtual_audio {

class VirtualAudioDeviceImpl;

class VirtualAudioStream : public audio::SimpleAudioStream {
 public:
  void EnqueuePlugChange(bool plugged) __TA_EXCLUDES(wakeup_queue_lock_);
  void EnqueueGainRequest(fuchsia::virtualaudio::Device::GetGainCallback gain_callback)
      __TA_EXCLUDES(wakeup_queue_lock_);
  void EnqueueFormatRequest(fuchsia::virtualaudio::Device::GetFormatCallback format_callback)
      __TA_EXCLUDES(wakeup_queue_lock_);
  void EnqueueBufferRequest(fuchsia::virtualaudio::Device::GetBufferCallback buffer_callback)
      __TA_EXCLUDES(wakeup_queue_lock_);
  void EnqueuePositionRequest(fuchsia::virtualaudio::Device::GetPositionCallback position_callback)
      __TA_EXCLUDES(wakeup_queue_lock_);
  void EnqueueNotificationOverride(uint32_t notifications_per_ring)
      __TA_EXCLUDES(wakeup_queue_lock_);

  static fbl::RefPtr<VirtualAudioStream> CreateStream(VirtualAudioDeviceImpl* owner,
                                                      zx_device_t* devnode, bool is_input);

  // Only set by DeviceImpl -- on dtor, Disable or Remove
  bool shutdown_by_parent_ = false;

 protected:
  friend class audio::SimpleAudioStream;
  friend class fbl::RefPtr<VirtualAudioStream>;

  VirtualAudioStream(VirtualAudioDeviceImpl* parent, zx_device_t* dev_node, bool is_input)
      : audio::SimpleAudioStream(dev_node, is_input), parent_(parent) {}

  zx_status_t Init() __TA_REQUIRES(domain_token()) override;

  zx_status_t ChangeFormat(const audio::audio_proto::StreamSetFmtReq& req)
      __TA_REQUIRES(domain_token()) override;
  zx_status_t SetGain(const audio::audio_proto::SetGainReq& req)
      __TA_REQUIRES(domain_token()) override;

  zx_status_t GetBuffer(const audio::audio_proto::RingBufGetBufferReq& req,
                        uint32_t* out_num_rb_frames, zx::vmo* out_buffer)
      __TA_REQUIRES(domain_token()) override;

  zx_status_t Start(uint64_t* out_start_time) __TA_REQUIRES(domain_token()) override;
  zx_status_t Stop() __TA_REQUIRES(domain_token()) override;

  void ShutdownHook() __TA_REQUIRES(domain_token()) override;
  // RingBufferShutdown() is unneeded: no hardware shutdown tasks needed...

  void ProcessRingNotification();
  void ProcessAltRingNotification() __TA_REQUIRES(domain_token());

  enum class PlugType { Plug, Unplug };

  void HandlePlugChanges() __TA_REQUIRES(domain_token()) __TA_EXCLUDES(wakeup_queue_lock_);
  void HandlePlugChange(PlugType plug_change) __TA_REQUIRES(domain_token());

  void HandleGainRequests() __TA_REQUIRES(domain_token()) __TA_EXCLUDES(wakeup_queue_lock_);
  void HandleFormatRequests() __TA_REQUIRES(domain_token()) __TA_EXCLUDES(wakeup_queue_lock_);
  void HandleBufferRequests() __TA_REQUIRES(domain_token()) __TA_EXCLUDES(wakeup_queue_lock_);
  void HandlePositionRequests() __TA_REQUIRES(domain_token()) __TA_EXCLUDES(wakeup_queue_lock_);
  void HandleSetNotifications() __TA_REQUIRES(domain_token()) __TA_EXCLUDES(wakeup_queue_lock_);

  // Accessed in GetBuffer, defended by token.
  fzl::VmoMapper ring_buffer_mapper_ __TA_GUARDED(domain_token());
  zx::vmo ring_buffer_vmo_ __TA_GUARDED(domain_token());
  uint32_t num_ring_buffer_frames_ __TA_GUARDED(domain_token()) = 0;

  uint32_t max_buffer_frames_ __TA_GUARDED(domain_token());
  uint32_t min_buffer_frames_ __TA_GUARDED(domain_token());
  uint32_t modulo_buffer_frames_ __TA_GUARDED(domain_token());

  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::ProcessRingNotification>
      notify_timer_ __TA_GUARDED(domain_token()){this};
  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::ProcessAltRingNotification>
      alt_notify_timer_ __TA_GUARDED(domain_token()){this};

  zx::time start_time_ __TA_GUARDED(domain_token());
  zx::duration notification_period_ __TA_GUARDED(domain_token()) = zx::duration(0);
  uint32_t notifications_per_ring_ __TA_GUARDED(domain_token()) = 0;
  zx::time target_notification_time_ __TA_GUARDED(domain_token()) = zx::time(0);

  bool using_alt_notifications_ __TA_GUARDED(domain_token());
  zx::duration alt_notification_period_ __TA_GUARDED(domain_token()) = zx::duration(0);
  uint32_t alt_notifications_per_ring_ __TA_GUARDED(domain_token()) = 0;
  zx::time target_alt_notification_time_ __TA_GUARDED(domain_token()) = zx::time(0);

  uint32_t bytes_per_sec_ __TA_GUARDED(domain_token()) = 0;
  uint32_t frame_rate_ __TA_GUARDED(domain_token()) = 0;
  audio_sample_format_t sample_format_ __TA_GUARDED(domain_token()) = 0;
  uint32_t num_channels_ __TA_GUARDED(domain_token()) = 0;

  VirtualAudioDeviceImpl* parent_ __TA_GUARDED(domain_token());

  fbl::Mutex wakeup_queue_lock_ __TA_ACQUIRED_AFTER(domain_token());

  // TODO(mpuryear): Refactor to a single queue of lambdas to dedupe this code.
  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::HandlePlugChanges>
      plug_change_wakeup_{this};
  std::deque<PlugType> plug_queue_ __TA_GUARDED(wakeup_queue_lock_);

  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::HandleGainRequests>
      gain_request_wakeup_{this};
  std::deque<fuchsia::virtualaudio::Device::GetGainCallback> gain_queue_
      __TA_GUARDED(wakeup_queue_lock_);

  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::HandleFormatRequests>
      format_request_wakeup_{this};
  std::deque<fuchsia::virtualaudio::Device::GetFormatCallback> format_queue_
      __TA_GUARDED(wakeup_queue_lock_);

  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::HandleBufferRequests>
      buffer_request_wakeup_{this};
  std::deque<fuchsia::virtualaudio::Device::GetBufferCallback> buffer_queue_
      __TA_GUARDED(wakeup_queue_lock_);

  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::HandlePositionRequests>
      position_request_wakeup_{this};
  std::deque<fuchsia::virtualaudio::Device::GetPositionCallback> position_queue_
      __TA_GUARDED(wakeup_queue_lock_);

  async::TaskClosureMethod<VirtualAudioStream, &VirtualAudioStream::HandleSetNotifications>
      set_notifications_wakeup_{this};
  std::deque<uint32_t> notifs_queue_ __TA_GUARDED(wakeup_queue_lock_);
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_STREAM_H_
