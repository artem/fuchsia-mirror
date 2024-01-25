// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_MIXER_MIX_PRODUCER_STAGE_H_
#define SRC_MEDIA_AUDIO_SERVICES_MIXER_MIX_PRODUCER_STAGE_H_

#include <lib/zx/time.h>

#include <functional>
#include <optional>
#include <string>
#include <variant>

#include "src/media/audio/lib/format2/fixed.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/mixer/common/atomic_optional.h"
#include "src/media/audio/services/mixer/mix/mix_job_context.h"
#include "src/media/audio/services/mixer/mix/packet_view.h"
#include "src/media/audio/services/mixer/mix/pipeline_stage.h"
#include "src/media/audio/services/mixer/mix/ptr_decls.h"
#include "src/media/audio/services/mixer/mix/start_stop_control.h"

namespace media_audio {

// A producer has zero source streams and a single destination stream.
//
// ## Starting, stopping, and timelines
//
// Producers can be started and stopped. To simplify the implementation, a producer's Start and Stop
// state are not exposed to downstream PipelineStages. Each ProducerStage uses two frame timelines:
//
// * An *internal* frame timeline. This is defined relative to the Producer's media timeline, as
//   described in ../docs/timelines.md. FIDL commands use the media timeline directly, then get
//   translated to internal commands (Start, Stop) which use internal frames. The internal stage
//   uses this frame timeline.
//
// * A *downstream* frame timeline. This is the same frame timeline used by our downstream
//   PipelineStage. Public methods (Read, Advance, presentation_time_to_frac_frame) use the
//   downstream frame timeline. Then, within AdvanceSourcesImpl and ReadImpl, we translate
//   downstream frames to internal frames on-the-fly.
//
// This design makes it simpler to implement Start and Stop with frame accuracy. For example,
// suppose we receive Start command that is scheduled to happen in the middle of a mix job. To
// implement this accurately, the translation from downstream to internal frames needs to use one
// function for all frames before the Start and a second function for all frames after the Start.
// It's best to do this translation internally rather than force it on our downstream PipelineStage.
// See additional discussion in ../docs/timelines.md.
//
// The translation between downstream frame and presentation time is stored in
// `presentation_time_to_frac_frame()`.
//
// The translation between internal frame and presentation time is stored internally and not
// exposed.
//
// ## Data production
//
// This class does not directly produce any data. Instead, it is a wrapper around an internal
// PipelineStage which produces data, typically a packet queue or ring buffer. This design allows us
// to separate concerns and reuse code: ProducerStage handles Start and Stop commands and translates
// between "downstream" and "internal" frame time, as described above, while the internal
// PipelineStage runs on internal frame time and is responsible for actually producing data.
class ProducerStage : public PipelineStage {
 public:
  using StartCommand = StartStopControl::StartCommand;
  using StopCommand = StartStopControl::StopCommand;
  using PendingStartStopCommand = AtomicOptional<StartStopControl::Command>;

  struct Args {
    // Name of this stage.
    std::string_view name;

    // Format of this stage's destination stream.
    Format format;

    // Reference clock of this stage's output stream.
    UnreadableClock reference_clock;

    // Ticks of media time per nanoseconds of reference time.
    TimelineRate media_ticks_per_ns;

    // Slot to hold a pending start/stop command.
    std::shared_ptr<PendingStartStopCommand> pending_start_stop_command;

    // Internal stage which actually produces the data. This must be specified and must have the
    // same format and reference clock as this ProducerStage.
    PipelineStagePtr internal_source;
  };

  explicit ProducerStage(Args args);

  // Implements `PipelineStage`.
  void AddSource(PipelineStagePtr source, AddSourceOptions options) final {
    UNREACHABLE << "ProducerStage should not have a source";
  }
  void RemoveSource(PipelineStagePtr source) final {
    UNREACHABLE << "ProducerStage should not have a source";
  }
  void UpdatePresentationTimeToFracFrame(std::optional<TimelineFunction> f) final;

 protected:
  // Implements `PipelineStage`.
  void AdvanceSelfImpl(Fixed frame) final {}
  void AdvanceSourcesImpl(MixJobContext& ctx, Fixed frame) final;
  std::optional<Packet> ReadImpl(MixJobContext& ctx, Fixed start_frame, int64_t frame_count) final;

 private:
  struct CommandSummary {
    StartStopControl::CommandType type;
    // When this command took effect, in three different units.
    zx::time presentation_time;
    Fixed internal_frame;
    Fixed downstream_frame;
  };

  std::optional<CommandSummary> NextCommand(const MixJobContext& ctx);
  void PopPendingCommand();
  void AdvanceStartStopControlTo(const MixJobContext& ctx, zx::time presentation_time);
  void RecomputeInternalFrameOffset();
  std::optional<Fixed> PresentationTimeToDownstreamFrame(zx::time t);
  std::optional<zx::time> DownstreamFrameToPresentationTime(Fixed downstream_frame);

  // TODO(https://fxbug.dev/42065903): Need to call `internal_source_->set_thread(thread())` when
  // `PipelineStage::set_thread` is called for this stage to keep both threads in sync.
  const PipelineStagePtr internal_source_;  // uses internal frame time
  const std::shared_ptr<PendingStartStopCommand> pending_start_stop_command_;
  StartStopControl start_stop_control_;

  // The translation between internal frame and presentation time.
  std::optional<TimelineFunction> presentation_time_to_internal_frac_frame_;

  // Given a downstream frame, we can compute an internal frame using the formula
  // `f_internal = f_downstream + internal_frame_offset_`. This is `std::nullopt` iff either the
  // downstream or internal frame timeline is stopped.
  std::optional<Fixed> internal_frame_offset_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_MIXER_MIX_PRODUCER_STAGE_H_
