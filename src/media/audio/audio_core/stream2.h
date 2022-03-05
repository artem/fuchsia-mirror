// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_STREAM_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_STREAM_H_

#include <fuchsia/media/cpp/fidl.h>
#include <lib/fpromise/result.h>
#include <lib/zx/time.h>

#include <optional>

#include <fbl/static_vector.h>

#include "src/media/audio/audio_core/packet.h"
#include "src/media/audio/audio_core/stage_metrics.h"
#include "src/media/audio/audio_core/stream_usage.h"
#include "src/media/audio/audio_core/verbose_log.h"
#include "src/media/audio/lib/clock/audio_clock.h"
#include "src/media/audio/lib/format/format.h"
#include "src/media/audio/lib/timeline/timeline_function.h"

// TODO(fxbug.dev/50669): namespace is temporary until we move this file to stream.h
namespace media::audio::stream2 {

// TODO(fxbug.dev/50669): remove after replacing stream.h
using ::media::audio::AudioClock;
using ::media::audio::Fixed;
using ::media::audio::Format;
using ::media::audio::StageMetrics;
using ::media::audio::StreamUsageMask;

// A stream represents a single stage in an audio processing graph.
class BaseStream {
 public:
  static constexpr bool kLogPresentationDelay = false;

  BaseStream(std::string_view name, Format format) : name_(name), format_(format) {}
  virtual ~BaseStream() = default;

  // A name provided for debugging.
  std::string_view name() const { return name_; }

  // Format of data generated by this stream.
  // TODO(fxbug.dev/58740): make sure this is accurate in all implementations.
  const Format& format() const { return format_; }

  // A snapshot of a `TimelineFunction` with an associated `generation`. If `generation` is equal
  // between two subsequent calls to `ref_time_to_fract_presentation_frame`, then the
  // `timeline_function` is guaranteed to be unchanged.
  struct TimelineFunctionSnapshot {
    TimelineFunction timeline_function;
    uint32_t generation;
  };

  // This function translates from a timestamp to the corresponding fixed-point frame number that
  // will be presented at that time. The timestamp is relative to the stream's reference clock.
  virtual TimelineFunctionSnapshot ref_time_to_frac_presentation_frame() const = 0;
  virtual AudioClock& reference_clock() = 0;

  // Common shorthands to convert between PTS and frame numbers.
  Fixed FracPresentationFrameAtRefTime(zx::time ref_time) const {
    return Fixed::FromRaw(
        ref_time_to_frac_presentation_frame().timeline_function.Apply(ref_time.get()));
  }
  zx::time RefTimeAtFracPresentationFrame(Fixed frame) const {
    return zx::time(
        ref_time_to_frac_presentation_frame().timeline_function.ApplyInverse(frame.raw_value()));
  }

  // The presentation delay is defined to be the absolute difference between a frame's
  // presentation timestamp and the frame's safe read/write timestamp. This is always a
  // positive number. Ideally this should be the exact delay, if known, and otherwise a
  // true upper-bound of the delay, however in practice it is sometimes a best-effort
  // estimate that can be either low or high.
  //
  // For render pipelines, this represents the delay between reading a frame with
  // ReadLock and actually rendering the frame at an output device. This is also known as
  // the "min lead time".
  //
  // For capture pipelines, this represents the delay between capturing a frame at
  // an input device and reading that frame with ReadLock.
  zx::duration GetPresentationDelay() const { return presentation_delay_.load(); }

  // Presentation delays are propagated from destination streams to source streams. The
  // delay passed to the source stream is typically external_delay + intrinsic_delay.
  // The default implementation is sufficient for pipeline stages that do not introduce
  // extra delay.
  virtual void SetPresentationDelay(zx::duration external_delay) {
    presentation_delay_.store(external_delay);
  }

 private:
  std::string name_;
  Format format_;
  std::atomic<zx::duration> presentation_delay_{zx::duration(0)};
};

// A read-only stream of audio data.
// ReadableStreams should be created and held as shared_ptr<>s.
class ReadableStream : public BaseStream, public std::enable_shared_from_this<ReadableStream> {
 public:
  class Buffer {
   public:
    ~Buffer() {
      if (dtor_) {
        dtor_(consumed_);
      }
    }

    Buffer(Buffer&& rhs) = default;
    Buffer& operator=(Buffer&& rhs) = default;

    Buffer(const Buffer& rhs) = delete;
    Buffer& operator=(const Buffer& rhs) = delete;

    Fixed start() const { return start_; }
    Fixed end() const { return end_; }
    int64_t length() const { return length_; }
    void* payload() const { return payload_; }

    // Call this to indicate that frames [start(), start()+frames) have been consumed.
    // If not called before the Buffer is discarded, we assume the entire buffer has been consumed.
    void set_frames_consumed(int64_t frames) {
      FX_CHECK(frames <= length_) << ffl::String::DecRational << frames << " > " << length_;
      consumed_ = frames;
    }

    // Returns the set of usages that have contributed to this buffer.
    StreamUsageMask usage_mask() const { return usage_mask_; }

    // Returns the total gain that has been applied to the source stream. A source stream may be the
    // result of mixing many other sources, so in practice this value is the maximum accumulated
    // gain applied to any input to this source stream.
    //
    // For example, if this source stream is the result of mixing Stream A with gain -12dB applied,
    // and Stream B with gain +2dB applied, then total_applied_gain_db() would return +2.0.
    //
    // This is entirely unrelated to actual stream signal content and should only be used as a rough
    // indicator of the amplitude of the source stream.
    float total_applied_gain_db() const { return total_applied_gain_db_; }

   private:
    friend class ReadableStream;
    using DestructorT = fit::callback<void(int64_t frames_consumed)>;

    Buffer(Fixed start_frame, int64_t length_in_frames, void* payload, bool cache_this_buffer,
           StreamUsageMask usage_mask, float total_applied_gain_db, DestructorT dtor)
        : dtor_(std::move(dtor)),
          payload_(payload),
          start_(start_frame),
          end_(start_frame + Fixed(length_in_frames)),
          length_(length_in_frames),
          consumed_(length_in_frames),
          cache_this_buffer_(cache_this_buffer),
          usage_mask_(usage_mask),
          total_applied_gain_db_(total_applied_gain_db) {}

    DestructorT dtor_;
    void* payload_;
    Fixed start_;
    Fixed end_;
    int64_t length_;
    int64_t consumed_;
    bool cache_this_buffer_;
    StreamUsageMask usage_mask_;
    float total_applied_gain_db_;
  };

  // ReadLockContext provides a container for state that can be carried through a
  // sequence of ReadLock calls.
  class ReadLockContext {
   private:
    // Capacity of per_stage_metrics_.
    static constexpr size_t kMaxStages = 16;

   public:
    // Adds the given metrics. Internally we maintain one StageMetrics object per stage.
    // If this method is called multiple times with the same stage name, the metrics are
    // accumulated.
    void AddStageMetrics(const StageMetrics& new_stage) {
      for (auto& old_stage : per_stage_metrics_) {
        if (std::string_view(old_stage.name) == std::string_view(new_stage.name)) {
          old_stage += new_stage;
          return;
        }
      }
      // Add a new stage, or silently drop if we've exceeded the maximum.
      if (per_stage_metrics_.size() < kMaxStages) {
        per_stage_metrics_.push_back(new_stage);
      }
    }

    // Returns all metrics accumulated via AddMetrics.
    using StageMetricsVector = fbl::static_vector<StageMetrics, kMaxStages>;
    const StageMetricsVector& per_stage_metrics() { return per_stage_metrics_; }

   private:
    StageMetricsVector per_stage_metrics_;
  };

  // ReadableStream is implemented by audio pipeline stages that consume zero or more source
  // streams and produce a destination stream. ReadLock acquires a read lock on the destination
  // stream. The parameters `dest_frame` and `frame_count` represent a range of frames on the
  // destination stream's frame timeline.
  //
  // THE RETURNED BUFFER
  //
  //   If no data is available for the requested frame range, ReadLock returns std::nullopt.
  //   Otherwise, ReadLock returns a buffer representing all or part of the requested range.
  //   If `start()` on the returned buffer is greater than `dest_frame`, then the stream
  //   has no data for those initial frames and it may be treated as silence. Conversely, if `end()`
  //   on the returned buffer is less than `dest_frame + frame_count`, this indicates the full
  //   frame range is not available on a single contiguous buffer. Clients should call `ReadLock`
  //   again, with `dest_frame` set to the `end()` of the previous buffer, to query if the stream
  //   has more frames.
  //
  //   The buffer must contain an integral number of frames and must satisfy the following
  //   conditions:
  //
  //     - buffer.start() > dest_frame - Fixed(1)
  //     - buffer.end() <= dest_frame + Fixed(frame_count)
  //     - buffer.length() <= frame_count
  //
  //   The buffer's `start()` is the position of the left edge of the first frame in the buffer.
  //   For example, given `ReadLock(Fixed(10), 5)`, if the stream's frames happen to be aligned
  //   on positions 9.1, 10.1, 11.1, etc., then ReadLock should return a buffer with `start() = 9.1`
  //   and `length() = 5`.
  //
  //   The stream will remain locked until thebuffer is destructed.
  //
  // THE PASSAGE OF TIME
  //
  //   Each ReadableStream maintains a current frame position (aka time). This position must always
  //   move forward. Position advances to X after a call to `Trim(X)`. Position is also advanced by
  //   ReadLock, which calls Trim as follows:
  //
  //     - If ReadLock returns std::nullopt, we advance time to
  //       `Trim(dest_frame + frame_count)`.
  //
  //     - When a buffer is unlocked, we advance time to
  //       `Trim(buffer.start + buffer.frames_consumed)`.
  //
  //   Put differently, time advances when ReadLock is called, when a buffer is consumed, and on
  //   explicit calls to Trim. Time does not go backwards: each call to ReadLock must have
  //   `dest_frame` >= the last trimmed frame.
  //
  // RESET
  //
  //   Changing a stream's ref_time_to_frac_presentation_frame TimelineFunction discards all history
  //   recorded by prior calls to ReadLock and Trim. This allows time to go backwards as a one-time
  //   event after a Play or Pause command seeks backwards in a stream. Note that when time advances
  //   normally past X, then is reset to X or earlier (by Play), all internal caches are discarded
  //   and there's no requirement for a source to reproduce the same audio that was played at X the
  //   first time.
  std::optional<Buffer> ReadLock(ReadLockContext& ctx, Fixed dest_frame, int64_t frame_count);

  // Trims the stream by releasing any frames before the given frame. This is a declaration
  // that the caller will not attempt to ReadLock any frame before dest_frame. If the stream has
  // allocated buffers for frames before dest_frame, it can free those buffers now.
  // Must not be called while the stream is locked.
  void Trim(Fixed dest_frame);

  // Reports the highest frame passed to Trim, or std::nullopt if Trim has not been called or
  // if the stream has been reset since the last Trim.
  std::optional<Fixed> LastTrimmedFrame() {
    DetectTimelineUpdate();
    return next_dest_frame_;
  }

 protected:
  // `name` should uniquely identify the stream. Used for debugging.
  ReadableStream(std::string name, Format format);

  // Child classes must provide stream-specific implementations of ReadLockImpl and TrimImpl.
  // These are called by ReadLock and Trim, respectively.
  // ReadLock and Trim add some default behavior, including logging, tracing, caching of
  // partially-consumed buffers, and validation of pre and post conditions.
  virtual std::optional<Buffer> ReadLockImpl(ReadLockContext& ctx, Fixed dest_frame,
                                             int64_t frame_count) = 0;
  virtual void TrimImpl(Fixed dest_frame) = 0;

  // ReadLockImpl should use this to create a cached buffer. If the buffer is not fully
  // consumed after one ReadLock, the next ReadLock call will return the same buffer
  // without asking ReadLockImpl to recreate the same data. ReadableStream will hold
  // onto this buffer until the buffer is fully consumed or trimmed away.
  //
  // This is useful for pipeline stages that compute buffers dynamically, such as mixers
  // and effects. The std::optional return type is for convenience (so that `MakeCachedBuffer(...)`
  // can be returned directly from ReadLockImpl) -- the returned value is never std::nullopt.
  //
  // REQUIRED:
  // - The buffer's `start()` must obey the buffer constraints described by ReadLock,
  //   however the buffer's `length()` can be arbitrarily large. This is useful for pipeline
  //   stages that generate data in fixed-sized blocks: they may cache the entire block for
  //   future ReadLock calls.
  // - The payload must remain valid until the buffer is fully consumed, i.e. until the
  //   stream is Trim'd past the end of the buffer.
  std::optional<Buffer> MakeCachedBuffer(Fixed start_frame, int64_t frame_count, void* payload,
                                         StreamUsageMask usage_mask, float total_applied_gain_db);

  // ReadLockImpl should use this to create an uncached buffer. If the buffer is not fully
  // consumed after one ReadLock, the next ReadLock call will ask ReadLockImpl to recreate
  // the buffer.
  //
  // This is useful for streams that don't need caching or that want precise control over
  // buffer lifetimes. Examples include ring buffers and packet queues. The std::optional
  // return type is for convenience -- the returned value is never std::nullopt.
  //
  // REQUIRED:
  // - The buffer's `start()` and `length()` must obey the buffer constraints described
  //   by ReadLock (above).
  // - The payload must remain valid until the buffer is unlocked.
  std::optional<Buffer> MakeUncachedBuffer(Fixed start_frame, int64_t frame_count, void* payload,
                                           StreamUsageMask usage_mask, float total_applied_gain_db);

  // ReadLockImpl should use this when forwarding a Buffer from an upstream source.
  // This may be used by no-op pipeline stages. It is necessary to call ForwardBuffer,
  // rather than simply returning a buffer from an upstream source, so that this stream
  // will be Trim'd when the buffer is unlocked:
  //
  //    // Good: calls this->Trim() and src->Trim() when unlocked
  //    return ForwardBuffer(src->ReadLock(ctx, dest_frame, frame_count));
  //
  //    // Bad: calls src->Trim() when unlocked, but not this->Trim()
  //    return src->ReadLock(ctx, dest_frame, frame_count);
  //
  // If `start_frame` is specified, the returned buffer's starting frame is set to the
  // given value. The length is kept unchanged. This is useful when doing SampleAndHold
  // on a source stream and also when the source and destination timelines have the
  // same rate but are offset by an arbitrary amount. SampleAndHold looks like:
  //
  //     auto buffer = src->ReadLock(ctx, frame, frame_count);
  //     auto start_frame = buffer->start().Ceiling();
  //     return ForwardBufferWithModifiedStart(std::move(buffer), start_frame);
  //
  // If start_frame is not specified, the buffers is forwarded unchanged.
  std::optional<Buffer> ForwardBuffer(std::optional<Buffer>&& buffer,
                                      std::optional<Fixed> start_frame = std::nullopt);

 private:
  void DetectTimelineUpdate();
  std::optional<ReadableStream::Buffer> ReadFromCachedBuffer(Fixed start_frame,
                                                             int64_t frame_count);

  std::optional<uint32_t> timeline_function_generation_;
  std::optional<Fixed> next_dest_frame_;  // marks the passage of time
  std::optional<Fixed> previous_buffer_end_;
  bool locked_ = false;

  // This is cached from the last call to ReadLockImpl.
  // We hold onto this until buffer until next_dest_frame_ >= cached_->end.
  std::optional<ReadableStream::Buffer> cached_;

  // For TRACE_DURATION, which requires C strings.
  const std::string name_for_read_lock_;
  const std::string name_for_trim_;
};

// A write-only stream of audio data.
class WritableStream : public BaseStream {
 public:
  class Buffer {
   public:
    using DestructorT = fit::callback<void()>;

    Buffer(int64_t start_frame, int64_t length_in_frames, void* payload, DestructorT dtor = nullptr)
        : dtor_(std::move(dtor)),
          payload_(payload),
          start_(start_frame),
          end_(start_frame + length_in_frames),
          length_(length_in_frames) {}

    ~Buffer() {
      if (dtor_) {
        dtor_();
      }
    }

    Buffer(Buffer&& rhs) = default;
    Buffer& operator=(Buffer&& rhs) = default;

    Buffer(const Buffer& rhs) = delete;
    Buffer& operator=(const Buffer& rhs) = delete;

    int64_t start() const { return start_; }
    int64_t end() const { return end_; }
    int64_t length() const { return length_; }
    void* payload() const { return payload_; }

   private:
    DestructorT dtor_;
    void* payload_;
    int64_t start_;
    int64_t end_;
    int64_t length_;
  };

  WritableStream(std::string name, Format format) : BaseStream(std::move(name), format) {}

  // WritableStream is implemented by audio sinks. WriteLock acquires a write lock on the
  // stream. The parameters `start_frame` and `frame_count` represent a range of frames on the
  // stream's frame timeline.
  //
  // If data cannot be written to that frame range, WriteLock returns std::nullopt.
  // Otherwise, WriteLock returns a buffer representing all or part of the requested range.
  // If `start()` on the returned buffer is greater than `start_frame`, then frames before
  // `start()` must not be written. Conversely, if `end()` on the returned buffer is less than
  // `start_frame + frame_count`, this does not indicate those frames can be omitted. Instead
  // it indicates the full frame range is not available on a single contiguous buffer. Clients
  // should call `WriteLock` again and provide the `end()` of the previous buffer as `start_frame`
  // to query if the stream has more frames.
  //
  // The returned buffer must not refer to frames outside of the range
  // [start_frame, start_frame + frame_count).
  //
  // Callers can write directly to the payload. The buffer will remain locked until it is
  // destructed. It is illegal to call WriteLock again until the lock has been released.
  virtual std::optional<Buffer> WriteLock(int64_t start_frame, int64_t frame_count) = 0;
};

}  // namespace media::audio::stream2

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_STREAM_H_
