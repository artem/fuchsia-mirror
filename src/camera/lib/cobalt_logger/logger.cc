// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/camera/lib/cobalt_logger/logger.h"

#include <lib/async/cpp/task.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <string>

#include "src/cobalt/bin/utils/error_utils.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace camera::cobalt {
namespace {

using async::PostDelayedTask;
using ::cobalt::ErrorToString;
using fuchsia::metrics::MetricEventLoggerFactory;
using fxl::StringPrintf;

// Keep this under 1750 so we can't overflow the cobalt channel.
constexpr uint32_t kMaxPendingEvents = 1700u;

uint64_t CurrentTimeUSecs(const std::unique_ptr<timekeeper::Clock>& clock) {
  return zx::nsec(clock->Now().get()).to_usecs();
}

inline bool HasStreamProperty(fuchsia::camera2::CameraStreamType stream_type,
                              fuchsia::camera2::CameraStreamType target_type) {
  return (stream_type & target_type) == target_type;
}

}  // namespace

Logger::Logger(async_dispatcher_t* dispatcher, std::shared_ptr<sys::ServiceDirectory> services,
               std::unique_ptr<timekeeper::Clock> clock)
    : dispatcher_(dispatcher),
      services_(services),
      clock_(std::move(clock)),
      logger_reconnection_backoff_(/*initial_delay=*/zx::msec(100), /*retry_factor=*/2u,
                                   /*max_delay=*/zx::hour(1)) {
  logger_.set_error_handler([this](zx_status_t status) {
    FX_PLOGS(WARNING, status) << "Lost connection with MetricEventLogger";
    RetryConnectingToLogger();
  });

  auto logger_request = logger_.NewRequest();
  ConnectToLogger(std::move(logger_request));
}

void Logger::Shutdown() {
  shut_down_ = true;

  pending_events_.clear();
  timer_starts_usecs_.clear();

  reconnect_task_.Cancel();

  logger_factory_.Unbind();
  logger_.Unbind();
}

void Logger::ConnectToLogger(
    ::fidl::InterfaceRequest<fuchsia::metrics::MetricEventLogger> logger_request) {
  // Connect to the LoggerFactory.
  logger_factory_ = services_->Connect<fuchsia::metrics::MetricEventLoggerFactory>();

  logger_factory_.set_error_handler([](zx_status_t status) {
    FX_PLOGS(WARNING, status) << "Lost connection with MetricEventLoggerFactory";
  });

  fuchsia::metrics::ProjectSpec project;
  project.set_customer_id(kCustomerId);
  project.set_project_id(kProjectId);

  logger_factory_->CreateMetricEventLogger(
      std::move(project), std::move(logger_request),
      [this](fuchsia::metrics::MetricEventLoggerFactory_CreateMetricEventLogger_Result result) {
        // We don't need a long standing connection to the LoggerFactory so we unbind after
        // setting up the Logger.
        logger_factory_.Unbind();

        if (result.is_response()) {
          logger_reconnection_backoff_.Reset();
        } else if (result.err() == fuchsia::metrics::Error::SHUT_DOWN) {
          FX_LOGS(INFO) << "Stopping sending Cobalt events";
          logger_.Unbind();
        } else {
          FX_LOGS(WARNING) << "Failed to set up Cobalt: " << ErrorToString(result.err());
          logger_.Unbind();
          RetryConnectingToLogger();
        }
      });
}

void Logger::RetryConnectingToLogger() {
  if (logger_) {
    return;
  }

  // Bind |logger_| and immediately send the events that were not acknowledged by the server on the
  // previous connection.
  auto logger_request = logger_.NewRequest();
  SendAllPendingEvents();

  reconnect_task_.Reset([this, request = std::move(logger_request)]() mutable {
    ConnectToLogger(std::move(request));
  });

  PostDelayedTask(
      dispatcher_, [reconnect = reconnect_task_.callback()]() { reconnect(); },
      logger_reconnection_backoff_.GetNext());
}

void Logger::LogEvent(Event event) {
  FX_CHECK(!shut_down_);
  if (pending_events_.size() >= kMaxPendingEvents) {
    // Drop event
    dropped_events_since_last_report_++;
    auto now = zx::clock::get_monotonic();
    if (now < droped_events_next_report_permitted_) {
      // Avoid logging to because not enough time has passed since the last report.
      return;
    }
    FX_LOGS(WARNING) << StringPrintf(
        "Dropped %lu Cobalt events (event: %s) - too many pending events (%lu)",
        dropped_events_since_last_report_, event.ToString().c_str(), pending_events_.size());
    dropped_events_since_last_report_ = 0;
    droped_events_next_report_permitted_ = now + zx::sec(1);
    return;
  }

  const uint64_t event_id = next_event_id_++;
  pending_events_.emplace(event_id, std::move(event));
  SendEvent(event_id);
}

uint64_t Logger::StartTimer() {
  FX_CHECK(!shut_down_);

  const uint64_t timer_id = next_event_id_++;
  timer_starts_usecs_.insert(std::make_pair(timer_id, CurrentTimeUSecs(clock_)));
  return timer_id;
}

void Logger::SendEvent(uint64_t event_id) {
  if (!logger_) {
    return;
  }

  if (pending_events_.find(event_id) == pending_events_.end()) {
    return;
  }
  Event& event = pending_events_.at(event_id);

  auto callback = [this, event_id,
                   &event](fuchsia::metrics::MetricEventLogger_LogOccurrence_Result result) {
    if (result.is_err()) {
      FX_LOGS(ERROR) << StringPrintf("Cobalt logging error: status %s, event %s",
                                     ErrorToString(result.err()).c_str(), event.ToString().c_str());
    }

    // We don't retry events that have been acknowledged by the server, regardless of the return
    // status.
    pending_events_.erase(event_id);
  };

  switch (event.GetType()) {
    case EventType::kOccurrence:
      logger_->LogOccurrence(event.GetMetricId(), 1u, event.GetDimensions(), std::move(callback));
      break;

    default:
      FX_LOGS(WARNING) << "Ignores an event with unknown type.";
      break;
  }
}

void Logger::SendAllPendingEvents() {
  for (const auto& [event_id, _] : pending_events_) {
    SendEvent(event_id);
  }
}

uint64_t Logger::GetTimerDurationUSecs(uint64_t timer_id) const {
  FX_CHECK(timer_starts_usecs_.find(timer_id) != timer_starts_usecs_.end());

  return CurrentTimeUSecs(clock_) - timer_starts_usecs_.at(timer_id);
}

StreamType Logger::ConvertStreamType(fuchsia::camera2::CameraStreamType type) {
  if (HasStreamProperty(type, fuchsia::camera2::CameraStreamType::VIDEO_CONFERENCE)) {
    if (HasStreamProperty(type, fuchsia::camera2::CameraStreamType::EXTENDED_FOV)) {
      if (HasStreamProperty(type, fuchsia::camera2::CameraStreamType::FULL_RESOLUTION)) {
        return cobalt::StreamType::kStream5;
      } else {
        return cobalt::StreamType::kStream6;
      }
    } else {
      if (HasStreamProperty(type, fuchsia::camera2::CameraStreamType::FULL_RESOLUTION)) {
        return cobalt::StreamType::kStream3;
      } else {
        return cobalt::StreamType::kStream4;
      }
    }
  } else {
    if (HasStreamProperty(type, fuchsia::camera2::CameraStreamType::MONITORING)) {
      return cobalt::StreamType::kStream2;
    } else if (HasStreamProperty(type, fuchsia::camera2::CameraStreamType::FULL_RESOLUTION)) {
      return cobalt::StreamType::kStream0;
    } else {
      return cobalt::StreamType::kStream1;
    }
  }
}

}  // namespace camera::cobalt
