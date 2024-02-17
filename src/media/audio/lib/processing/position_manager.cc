// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/lib/processing/position_manager.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include <algorithm>

#include <ffl/string.h>

#include "src/media/audio/lib/format2/fixed.h"
#include "src/media/audio/lib/processing/flags.h"
#include "src/media/audio/lib/processing/sampler.h"

namespace media_audio {

PositionManager::PositionManager(int32_t source_channel_count, int32_t dest_channel_count,
                                 int64_t frac_positive_length, int64_t frac_negative_length)
    : source_channel_count_(source_channel_count),
      dest_channel_count_(dest_channel_count),
      frac_positive_length_(frac_positive_length),
      frac_negative_length_(frac_negative_length) {
  FX_CHECK(frac_positive_length_ > 0);
  FX_CHECK(frac_negative_length_ > 0);
}

// static
void PositionManager::CheckPositions(int64_t dest_frame_count, int64_t* dest_offset_ptr,
                                     int64_t source_frame_count, int64_t frac_source_offset,
                                     int64_t frac_pos_filter_length, int64_t frac_step_size,
                                     uint64_t step_size_modulo, uint64_t step_size_denominator,
                                     uint64_t source_pos_modulo) {
  if constexpr (kCheckPositionsAndRatesOrDie) {
    PositionManager::CheckDestPositions(dest_frame_count, *dest_offset_ptr);
    PositionManager::CheckSourcePositions(source_frame_count, frac_source_offset,
                                          frac_pos_filter_length);
    PositionManager::CheckRateValues(frac_step_size, step_size_modulo, step_size_denominator,
                                     source_pos_modulo);
  }
}

// static
void PositionManager::CheckDestPositions(int64_t dest_frame_count, int64_t dest_offset) {
  if constexpr (kCheckPositionsAndRatesOrDie) {
    // Location of first destination frame cannot be negative.
    FX_CHECK(dest_offset >= 0) << "dest_offset (" << dest_offset << ") must be non-negative";

    // Location of first destination frame to produce must be within the provided buffer.
    FX_CHECK(dest_offset < dest_frame_count)
        << "dest_offset (" << dest_offset << ") must be less than dest_frame_count ("
        << dest_frame_count << ")";
  }
}

// static
void PositionManager::CheckSourcePositions(int64_t source_frame_count, int64_t frac_source_offset,
                                           int64_t frac_pos_filter_length) {
  if constexpr (kCheckPositionsAndRatesOrDie) {
    FX_CHECK(source_frame_count > 0) << "Source buffer must have at least one frame";
    FX_CHECK(frac_pos_filter_length > 0)
        << "Mixer lookahead frac_pos_filter_length (" << ffl::String::DecRational
        << Fixed::FromRaw(frac_pos_filter_length) << ") must be positive";

    // Source offset can be negative but only within bounds of `frac_pos_filter_length`.
    FX_CHECK(frac_source_offset + frac_pos_filter_length > 0)
        << "frac_source_offset (" << ffl::String::DecRational << Fixed::FromRaw(frac_source_offset)
        << ") must be greater than -pos_length (" << Fixed::FromRaw(-frac_pos_filter_length) << ")";

    // Source offset cannot exceed `source_frame_count`.
    FX_CHECK(((frac_source_offset - 1) >> Fixed::Format::FractionalBits) < source_frame_count)
        << "frac_source_offset: " << ffl::String::DecRational << Fixed::FromRaw(frac_source_offset)
        << ", source_frame_count: " << source_frame_count;
  }
}

// static
void PositionManager::CheckRateValues(int64_t frac_step_size, uint64_t step_size_modulo,
                                      uint64_t step_size_denominator, uint64_t source_pos_modulo) {
  if constexpr (kCheckPositionsAndRatesOrDie) {
    FX_CHECK(frac_step_size > 0) << "step_size must be positive; cannot be zero";

    FX_CHECK(step_size_denominator > 0) << "step_size_denominator cannot be zero";

    FX_CHECK(step_size_modulo < step_size_denominator)
        << "step_size_modulo (" << step_size_modulo << ") must be less than step_size_denominator ("
        << step_size_denominator << ")";

    FX_CHECK(source_pos_modulo < step_size_denominator)
        << "source_position_modulo (" << source_pos_modulo
        << ") must be less than step_size_denominator (" << step_size_denominator << ")";
  }
}

void PositionManager::Display() const {
  if constexpr (kEnableDisplayForPositionsAndRates) {
    FX_LOGS(INFO) << "Channels: source " << source_channel_count_ << ", dest "
                  << dest_channel_count_ << ".   Filter Length: pos " << ffl::String::DecRational
                  << Fixed::FromRaw(frac_positive_length_) << ", neg "
                  << Fixed::FromRaw(frac_negative_length_);

    FX_LOGS(INFO) << "Source:   len " << source_frame_count_ << ", to " << ffl::String::DecRational
                  << Fixed::FromRaw(frac_source_end_) << ". Dest: len " << dest_frame_count_;

    FX_LOGS(INFO) << "Rate:     frac_step_size " << ffl::String::DecRational
                  << Fixed(frac_step_size_) << ", rate_mod " << step_size_modulo_ << ", denom "
                  << step_size_denominator_;

    DisplayUpdate();
  }
}

void PositionManager::DisplayUpdate() const {
  if constexpr (kEnableDisplayForPositionsAndRates) {
    FX_LOGS(INFO) << "Position: frac_source_offset " << ffl::String::DecRational
                  << Fixed::FromRaw(frac_source_offset_) << ": dest_offset " << dest_offset_
                  << ", pos_mod " << source_pos_modulo_;
  }
}

void PositionManager::SetDestValues(float* dest_ptr, int64_t dest_frame_count,
                                    int64_t* dest_offset_ptr) {
  if constexpr (kTracePositionEvents) {
    TRACE_DURATION("audio", __func__, "dest_frame_count", dest_frame_count, "dest_offset",
                   *dest_offset_ptr);
  }
  if constexpr (kCheckPositionsAndRatesOrDie) {
    CheckDestPositions(dest_frame_count, *dest_offset_ptr);
  }

  dest_ptr_ = dest_ptr;
  dest_frame_count_ = dest_frame_count;
  dest_offset_ptr_ = dest_offset_ptr;
  dest_offset_ = *dest_offset_ptr_;
}

void PositionManager::SetSourceValues(const void* source_void_ptr, int64_t source_frame_count,
                                      Fixed* source_offset_ptr) {
  if constexpr (kTracePositionEvents) {
    TRACE_DURATION("audio", __func__, "source_frame_count", source_frame_count, "source_offset",
                   source_offset_ptr->Integral().Floor(), "source_offset.frac",
                   source_offset_ptr->Fraction().raw_value());
  }
  source_void_ptr_ = const_cast<void*>(source_void_ptr);
  source_frame_count_ = source_frame_count;
  source_offset_ptr_ = source_offset_ptr;
  frac_source_offset_ = source_offset_ptr_->raw_value();

  // `frac_source_end_` is the first subframe at which this call can no longer produce output.
  frac_source_end_ =
      (source_frame_count << Fixed::Format::FractionalBits) - frac_positive_length_ + 1;
}

void PositionManager::SetRateValues(int64_t frac_step_size, uint64_t step_size_modulo,
                                    uint64_t step_size_denominator, uint64_t source_pos_modulo) {
  if constexpr (kTracePositionEvents) {
    TRACE_DURATION("audio", __func__, "step_size",
                   Fixed::FromRaw(frac_step_size).Integral().Floor(), "step_size.frac",
                   Fixed::FromRaw(frac_step_size).Fraction().raw_value(), "step_size_modulo",
                   step_size_modulo, "step_size_denominator", step_size_denominator);
  }
  if constexpr (kCheckPositionsAndRatesOrDie) {
    CheckRateValues(frac_step_size, step_size_modulo, step_size_denominator, source_pos_modulo);
  }

  frac_step_size_ = frac_step_size;
  step_size_modulo_ = step_size_modulo;

  if (step_size_modulo_ > 0) {
    step_size_denominator_ = step_size_denominator;
    source_pos_modulo_ = source_pos_modulo;
  }
}

int64_t PositionManager::AdvanceToEnd() {
  if (!CanFrameBeMixed()) {
    return 0;
  }

  // Number of source steps available, if no rate modulo is in effect.
  const int64_t est_dest_frame_count_produced =
      (frac_source_end_ - frac_source_offset_ - 1) / frac_step_size_ + 1;
  const int64_t dest_frame_count_space_avail = dest_frame_count_ - dest_offset_;
  const int64_t avail = std::min(est_dest_frame_count_produced, dest_frame_count_space_avail);

  const auto prev_source_frame_consumed =
      (frac_source_offset_ + frac_positive_length_ - 1) >> Fixed::Format::FractionalBits;

  // Advance source and destination by `avail` steps.
  frac_source_offset_ += (avail * frac_step_size_);
  dest_offset_ += avail;

  if (step_size_modulo_ > 0) {
    // Compute the modulo after advancing, and increment `frac_source_offset_` accordingly.
    const uint64_t total_mod = source_pos_modulo_ + (avail * step_size_modulo_);
    frac_source_offset_ += total_mod / step_size_denominator_;
    source_pos_modulo_ = total_mod % step_size_denominator_;

    // Maintain an offset of previous source, for the last destination frame we would produce.
    int64_t prev_source_offset = frac_source_offset_ - frac_step_size_;
    if (source_pos_modulo_ < step_size_modulo_) {
      --prev_source_offset;
    }

    // If the rough estimate advanced position too far, roll position back until it is correct.
    // For the final destination frame we produce, `prev_source_offset` must be less than
    // `frac_source_end_`.
    while (prev_source_offset >= frac_source_end_) {
      if (source_pos_modulo_ < step_size_modulo_) {
        source_pos_modulo_ += step_size_denominator_;
      }
      source_pos_modulo_ -= step_size_modulo_;

      --dest_offset_;
      frac_source_offset_ = prev_source_offset;

      prev_source_offset = frac_source_offset_ - frac_step_size_;
      if (source_pos_modulo_ < step_size_modulo_) {
        --prev_source_offset;
      }
    }
  }

  const auto new_source_frame_consumed =
      (frac_source_offset_ + frac_positive_length_ - 1) >> Fixed::Format::FractionalBits;
  return new_source_frame_consumed - prev_source_frame_consumed;
}

void PositionManager::UpdateOffsets() {
  if constexpr (kTracePositionEvents) {
    TRACE_DURATION("audio", __func__, "source_offset",
                   Fixed::FromRaw(frac_source_offset_).Integral().Floor(), "source_offset.frac",
                   Fixed::FromRaw(frac_source_offset_).Fraction().raw_value(), "dest_offset",
                   dest_offset_, "source_pos_modulo", source_pos_modulo_);
  }
  *source_offset_ptr_ = Fixed::FromRaw(frac_source_offset_);
  *dest_offset_ptr_ = dest_offset_;
}

}  // namespace media_audio
