// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/lib/perfmon/config.h"

#include <lib/syslog/cpp/macros.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/performance/lib/perfmon/config_impl.h"

namespace perfmon {
std::string Config::StatusToString(Status status) {
  switch (status) {
    case Status::OK:
      return "OK";
    case Status::MAX_EVENTS:
      return "MAX_EVENTS";
    case Status::INVALID_ARGS:
      return "INVALID_ARGS";
  }
}

void Config::Reset() { events_.clear(); }

Config::Status Config::AddEvent(EventId event, EventRate rate, uint32_t flags) {
  if (events_.size() == kMaxNumEvents) {
    return Status::MAX_EVENTS;
  }
  if (rate == 0 && (flags & Config::kNonZeroRateOnlyFlags) != 0) {
    return Status::INVALID_ARGS;
  }
  events_.emplace(EventConfig{event, rate, flags});
  return Status::OK;
}

size_t Config::GetEventCount() const { return events_.size(); }

CollectionMode Config::GetMode() const {
  for (const auto& event : events_) {
    // If any event is doing sampling, then we're in "sample mode".
    if (event.rate != 0) {
      return CollectionMode::kSample;
    }
  }
  return CollectionMode::kTally;
}

void Config::IterateOverEvents(const IterateFunc& func) const {
  for (const auto& event : events_) {
    func(event);
  }
}

static std::string EventConfigToString(Config::EventConfig event) {
  return fxl::StringPrintf("event 0x%x, rate %u, flags 0x%x", event.event, event.rate, event.flags);
}

std::string Config::ToString() {
  std::string result;
  for (const auto& event : events_) {
    if (!result.empty()) {
      result += "; ";
    }
    result += EventConfigToString(event);
  }
  return result;
}

static void ToFidlEvent(const Config::EventConfig& event, size_t index,
                        fuchsia_perfmon_cpu::Config* out_config) {
  out_config->events()[index].event() = event.event;
  out_config->events()[index].rate() = event.rate;
  if (event.flags & Config::kFlagOs) {
    out_config->events()[index].flags() |= fuchsia_perfmon_cpu::EventConfigFlags::kCollectOs;
  }
  if (event.flags & Config::kFlagUser) {
    out_config->events()[index].flags() |= fuchsia_perfmon_cpu::EventConfigFlags::kCollectUser;
  }
  if (event.flags & Config::kFlagPc) {
    out_config->events()[index].flags() |= fuchsia_perfmon_cpu::EventConfigFlags::kCollectPc;
  }
  if (event.flags & Config::kFlagTimebase) {
    out_config->events()[index].flags() |= fuchsia_perfmon_cpu::EventConfigFlags::kIsTimebase;
  }
  if (event.flags & Config::kFlagLastBranch) {
    out_config->events()[index].flags() |=
        fuchsia_perfmon_cpu::EventConfigFlags::kCollectLastBranch;
  }
}

namespace internal {

void PerfmonToFidlConfig(const Config& config, fuchsia_perfmon_cpu::Config* out_config) {
  *out_config = {};
  FX_DCHECK(config.GetEventCount() <= kMaxNumEvents);

  size_t i = 0;
  config.IterateOverEvents([&](const Config::EventConfig& event) {
    ToFidlEvent(event, i, out_config);
    ++i;
  });
}

}  // namespace internal

}  // namespace perfmon
