// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/bootup_tracker.h"

#include <lib/async/cpp/time.h>

#include <src/devices/lib/log/log.h>

namespace driver_manager {

namespace {

zx::duration kBootupTimeoutDuration = zx::sec(2);
zx::duration kLastUpdatedTimeoutDuration = zx::sec(10);

}  // namespace

void BootupTracker::Start() { UpdateTrackerAndResetTimer(); }

void BootupTracker::WaitForBootup(WaitForBootupCompleter::Sync& completer) {
  if (bootup_done_) {
    completer.Reply();
  } else {
    completers_.push_back(completer.ToAsync());
  }
}

void BootupTracker::NotifyNewStartRequest(std::string node_moniker, std::string driver_url) {
  if (outstanding_start_requests_.find(node_moniker) != outstanding_start_requests_.end()) {
    LOGF(WARNING, "Bootup tracker received conflicting start requests for node %s",
         node_moniker.c_str());
  }
  outstanding_start_requests_[node_moniker] = driver_url;
  UpdateTrackerAndResetTimer();
}

void BootupTracker::NotifyStartComplete(std::string node_moniker) {
  if (auto itr = outstanding_start_requests_.find(node_moniker);
      itr != outstanding_start_requests_.end()) {
    outstanding_start_requests_.erase(itr);
  } else {
    LOGF(ERROR, "Bootup tracker notified for an unknown start request for %s",
         node_moniker.c_str());
  }
  UpdateTrackerAndResetTimer();
}

void BootupTracker::NotifyBindingChanged() { UpdateTrackerAndResetTimer(); }

void BootupTracker::CheckBootupDone() {
  bool deadline_exceeded = IsUpdateDeadlineExceeded();
  if (!deadline_exceeded &&
      (!outstanding_start_requests_.empty() || bind_manager_->HasOngoingBind())) {
    ResetBootupTimer();
    return;
  }

  if (deadline_exceeded) {
    LOGF(WARNING, "Deadline exceeded in the bootup tracker with:");
    LOGF(WARNING, "    %u unfinished start requests:", outstanding_start_requests_.size());
    for (const auto& [moniker, url] : outstanding_start_requests_) {
      LOGF(WARNING, "         - %s - %s", moniker.c_str(), url.c_str());
    }
    if (bind_manager_->HasOngoingBind()) {
      LOGF(WARNING, "    a hanging bind process in the bind manager");
    }
  } else {
    LOGF(INFO, "Bootup completed.");
  }

  for (auto& completer : completers_) {
    completer.Reply();
  }
  completers_.clear();
  bootup_done_ = true;
}

void BootupTracker::UpdateTrackerAndResetTimer() {
  last_update_timestamp_ = async::Now(dispatcher_);
  ResetBootupTimer();
}

void BootupTracker::OnBootupTimeout() {
  bootup_timeout_ = true;
  CheckBootupDone();
}

bool BootupTracker::IsUpdateDeadlineExceeded() const {
  auto time_delta = async::Now(dispatcher_) - last_update_timestamp_;
  return time_delta >= kLastUpdatedTimeoutDuration;
}

void BootupTracker::ResetBootupTimer() {
  if (bootup_done_) {
    return;
  }
  if (bootup_timeout_task_.is_pending()) {
    bootup_timeout_task_.Cancel();
  }
  bootup_timeout_task_.PostDelayed(dispatcher_, kBootupTimeoutDuration);
}

void BootupTracker::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_driver_development::BootupWatcher> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  LOGF(ERROR, "Unknown method in BootupWatcher protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace driver_manager
