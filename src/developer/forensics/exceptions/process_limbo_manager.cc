// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/developer/forensics/exceptions/process_limbo_manager.h"

#include <fuchsia/exception/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include "src/lib/fsl/handles/object_info.h"

namespace forensics {
namespace exceptions {

namespace {

using fuchsia::exception::MAX_EXCEPTIONS;
using fuchsia::exception::ProcessException;
using fuchsia::exception::ProcessExceptionInfo;
using fuchsia::exception::ProcessExceptionMetadata;

// Removes all stale weak pointers from the handler list.
void PruneStaleHandlers(std::vector<fxl::WeakPtr<ProcessLimboHandler>>* handlers) {
  // We only move active handlers to the new list.
  std::vector<fxl::WeakPtr<ProcessLimboHandler>> new_handlers;
  for (auto& handler : *handlers) {
    if (handler)
      new_handlers.push_back(std::move(handler));
  }

  *handlers = std::move(new_handlers);
}

// This verification was getting repeated for every call.
template <typename CallbackType>
bool VerifyState(const fxl::WeakPtr<ProcessLimboManager> limbo_manager, CallbackType* cb) {
  if (!limbo_manager) {
    (*cb)(fpromise::error(ZX_ERR_UNAVAILABLE));
    return false;
  }
  return true;
}

std::vector<std::string> CreateFilterVector(const std::set<std::string>& filter_set) {
  std::vector<std::string> filters;
  filters.reserve(filter_set.size());

  for (auto& filter : filter_set) {
    filters.emplace_back(filter);
  }

  return filters;
}

}  // namespace

ProcessLimboManager::ProcessLimboManager() : weak_factory_(this) {
  // Set the default function for getting process names.
  obtain_process_name_fn_ = [](zx_handle_t handle) -> std::string {
    return fsl::GetObjectName(handle);
  };
}

fxl::WeakPtr<ProcessLimboManager> ProcessLimboManager::GetWeakPtr() {
  return weak_factory_.GetWeakPtr();
}

void ProcessLimboManager::AddToLimbo(ProcessException process_exception) {
  // Check filters.
  auto process_name = obtain_process_name_fn_(process_exception.process().get());

  // Empty names will be stored within the limbo.
  if (!process_name.empty() && !filters_.empty()) {
    // Search for a partial match over the filters.
    bool filter_found = false;
    for (auto& filter : filters_) {
      if (process_name.find(filter) != std::string::npos) {
        filter_found = true;
        break;
      }
    }

    // If a matching filter was found, we will not store this process.
    if (filter_found)
      return;
  }

  limbo_[process_exception.info().process_koid] = std::move(process_exception);

  NotifyLimboChanged();
}

void ProcessLimboManager::NotifyLimboChanged() {
  // Notify the handlers of the new list of processes in limbo.
  PruneStaleHandlers(&handlers_);
  for (auto& handler : handlers_) {
    auto limbo_list = ListProcessesInLimbo();
    handler->LimboChanged(std::move(limbo_list));
  }
}

void ProcessLimboManager::AddHandler(fxl::WeakPtr<ProcessLimboHandler> handler) {
  handlers_.push_back(std::move(handler));
}

void ProcessLimboManager::AppendFiltersForTesting(const std::vector<std::string>& filters) {
  for (auto& filter : filters) {
    filters_.insert(filter);
  }
}

std::vector<ProcessExceptionMetadata> ProcessLimboManager::ListProcessesInLimbo() {
  std::vector<ProcessExceptionMetadata> exceptions;

  size_t max_size = limbo_.size() <= MAX_EXCEPTIONS ? limbo_.size() : MAX_EXCEPTIONS;
  exceptions.reserve(max_size);

  // The new rights of the handles we're going to duplicate.
  zx_rights_t rights =
      ZX_RIGHT_READ | ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_TRANSFER | ZX_RIGHT_DUPLICATE;
  for (const auto& [process_koid, limbo_exception] : limbo_) {
    ProcessExceptionMetadata metadata = {};

    zx::process process;
    if (auto res = limbo_exception.process().duplicate(rights, &process); res != ZX_OK) {
      FX_PLOGS(ERROR, res) << "Could not duplicate process handle.";
      continue;
    }

    zx::thread thread;
    if (auto res = limbo_exception.thread().duplicate(rights, &thread); res != ZX_OK) {
      FX_PLOGS(ERROR, res) << "Could not duplicate thread handle.";
      continue;
    }

    metadata.set_info(limbo_exception.info());
    metadata.set_process(std::move(process));
    metadata.set_thread(std::move(thread));

    exceptions.push_back(std::move(metadata));

    if (exceptions.size() >= MAX_EXCEPTIONS)
      break;
  }

  return exceptions;
}

bool ProcessLimboManager::SetActive(bool active) {
  // Ignore if no change.
  if (active == active_)
    return false;
  active_ = active;

  // If the limbo was disabled, free all the exceptions.
  if (!active_)
    limbo_.clear();

  // Notify the handlers of the new activa state.
  PruneStaleHandlers(&handlers_);
  for (auto& handler : handlers_) {
    handler->ActiveStateChanged(active);
  }

  return true;
}

// ProcessLimboHandler -----------------------------------------------------------------------------

ProcessLimboHandler::ProcessLimboHandler(fxl::WeakPtr<ProcessLimboManager> limbo_manager)
    : limbo_manager_(std::move(limbo_manager)), weak_factory_(this) {}

fxl::WeakPtr<ProcessLimboHandler> ProcessLimboHandler::GetWeakPtr() {
  return weak_factory_.GetWeakPtr();
}

void ProcessLimboHandler::SetActive(bool active, SetActiveCallback cb) {
  // Call the callback before so that the response of this call is sent before any hanging gets.
  cb();
  limbo_manager_->SetActive(active);
}

void ProcessLimboHandler::ActiveStateChanged(bool active) {
  if (!is_active_callback_) {
    // Reset the WatchActive state as the state is different from the last time the get was called.
    watch_active_dirty_bit_ = true;
  } else {
    is_active_callback_(active);
    is_active_callback_ = {};
    watch_active_dirty_bit_ = false;
  }

  // If there is a limbo call waiting, we tell them that it's canceled.
  if (!active) {
    if (watch_limbo_callback_) {
      watch_limbo_callback_(fpromise::error(ZX_ERR_CANCELED));
      watch_limbo_callback_ = {};
      watch_limbo_dirty_bit_ = false;
    } else {
      watch_limbo_dirty_bit_ = true;
    }
  }
}

void ProcessLimboHandler::LimboChanged(std::vector<ProcessExceptionMetadata> limbo_list) {
  if (!watch_limbo_callback_) {
    // Reset the hanging get state as the state is different from the first time the get was called.
    watch_limbo_dirty_bit_ = true;
    return;
  }

  watch_limbo_callback_(fpromise::ok(std::move(limbo_list)));
  watch_limbo_callback_ = {};
  watch_limbo_dirty_bit_ = false;
}

void ProcessLimboHandler::GetActive(WatchActiveCallback cb) {
  cb(limbo_manager_ ? limbo_manager_->active() : false);
}

void ProcessLimboHandler::WatchActive(WatchActiveCallback cb) {
  if (watch_active_dirty_bit_) {
    watch_active_dirty_bit_ = false;

    bool is_active = !!limbo_manager_ ? limbo_manager_->active() : false;
    cb(is_active);
    return;
  }

  // We store the latest callback for when the active state changes.
  is_active_callback_ = std::move(cb);
}

void ProcessLimboHandler::ListProcessesWaitingOnException(
    ListProcessesWaitingOnExceptionCallback cb) {
  if (!limbo_manager_) {
    cb(fpromise::error(ZX_ERR_BAD_STATE));
    return;
  }
  if (!limbo_manager_->active()) {
    cb(fpromise::error(ZX_ERR_UNAVAILABLE));
    return;
  }
  std::vector<ProcessExceptionInfo> exceptions;
  for (const auto& [process_koid, limbo_exception] : limbo_manager_->limbo()) {
    ProcessExceptionInfo info;
    info.set_info(limbo_exception.info());

    char name[ZX_MAX_NAME_LEN];
    if (auto res = limbo_exception.process().get_property(ZX_PROP_NAME, name, sizeof(name));
        res != ZX_OK) {
      FX_PLOGS(ERROR, res) << "Could not get process name.";
      continue;
    }
    info.set_process_name(name);

    if (auto res = limbo_exception.thread().get_property(ZX_PROP_NAME, name, sizeof(name));
        res != ZX_OK) {
      FX_PLOGS(ERROR, res) << "Could not get thread name.";
      continue;
    }
    info.set_thread_name(name);

    exceptions.push_back(std::move(info));

    if (exceptions.size() >= MAX_EXCEPTIONS)
      break;
  }
  cb(fpromise::ok(std::move(exceptions)));
}

void ProcessLimboHandler::WatchProcessesWaitingOnException(
    WatchProcessesWaitingOnExceptionCallback cb) {
  if (watch_limbo_dirty_bit_) {
    watch_limbo_dirty_bit_ = false;

    if (!limbo_manager_) {
      cb(fpromise::error(ZX_ERR_BAD_STATE));
      return;
    }

    if (!limbo_manager_->active()) {
      cb(fpromise::error(ZX_ERR_UNAVAILABLE));
      return;
    }

    auto processes = limbo_manager_->ListProcessesInLimbo();
    cb(fpromise::ok(std::move(processes)));
    return;
  }

  // Store the latest callback for when the processes enter the limbo.
  watch_limbo_callback_ = std::move(cb);
}

void ProcessLimboHandler::RetrieveException(zx_koid_t process_koid, RetrieveExceptionCallback cb) {
  if (!VerifyState(limbo_manager_, &cb))
    return;

  fuchsia::exception::ProcessLimbo_RetrieveException_Result result;

  auto& limbo = limbo_manager_->limbo_;

  auto it = limbo.find(process_koid);
  if (it == limbo.end()) {
    FX_LOGS(WARNING) << "Could not find process " << process_koid << " in limbo.";
    cb(fpromise::error(ZX_ERR_NOT_FOUND));
    return;
  }

  auto res = fpromise::ok(std::move(it->second));
  limbo.erase(it);
  cb(std::move(res));

  limbo_manager_->NotifyLimboChanged();
}

void ProcessLimboHandler::ReleaseProcess(zx_koid_t process_koid, ReleaseProcessCallback cb) {
  if (!VerifyState(limbo_manager_, &cb))
    return;

  auto& limbo = limbo_manager_->limbo_;

  auto it = limbo.find(process_koid);
  if (it == limbo.end()) {
    return cb(fpromise::error(ZX_ERR_NOT_FOUND));
  }

  limbo.erase(it);
  cb(fpromise::ok());

  limbo_manager_->NotifyLimboChanged();
}

void ProcessLimboHandler::GetFilters(GetFiltersCallback cb) {
  if (!limbo_manager_) {
    cb({});
    return;
  }

  cb(CreateFilterVector(limbo_manager_->filters_));
}

void ProcessLimboHandler::AppendFilters(std::vector<std::string> new_filters,
                                        AppendFiltersCallback cb) {
  if (!VerifyState(limbo_manager_, &cb))
    return;

  auto current_filters = limbo_manager_->filters_;

  for (auto& filter : new_filters) {
    current_filters.insert(filter);
    if (current_filters.size() >= ProcessLimboManager::kMaxFilters) {
      cb(fpromise::error(ZX_ERR_NO_RESOURCES));
      return;
    }
  }

  limbo_manager_->filters_ = std::move(current_filters);
  cb(fpromise::ok());
}

void ProcessLimboHandler::RemoveFilters(std::vector<std::string> filters,
                                        RemoveFiltersCallback cb) {
  if (!VerifyState(limbo_manager_, &cb))
    return;

  for (auto& filter : filters) {
    limbo_manager_->filters_.erase(filter);
  }

  cb(fpromise::ok());
}

}  // namespace exceptions
}  // namespace forensics
