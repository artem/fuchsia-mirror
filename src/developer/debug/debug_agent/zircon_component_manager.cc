// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/zircon_component_manager.h"

#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.sys2/cpp/fidl.h>
#include <fidl/fuchsia.test.manager/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <unistd.h>
#include <zircon/processargs.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <utility>

#include "src/developer/debug/debug_agent/debug_agent.h"
#include "src/developer/debug/debug_agent/debugged_process.h"
#include "src/developer/debug/debug_agent/filter.h"
#include "src/developer/debug/debug_agent/stdio_handles.h"
#include "src/developer/debug/ipc/records.h"
#include "src/developer/debug/shared/logging/file_line_function.h"
#include "src/developer/debug/shared/logging/logging.h"
#include "src/developer/debug/shared/message_loop.h"
#include "src/developer/debug/shared/status.h"
#include "src/lib/diagnostics/accessor2logger/log_message.h"
#include "src/lib/fxl/memory/ref_counted.h"
#include "src/lib/fxl/memory/ref_ptr.h"

namespace debug_agent {

namespace {

// Maximum time we wait for reading "elf/job_id" in the runtime directory.
constexpr uint64_t kMaxWaitMsForJobId = 1000;

// Helper to simplify request pipelining.
template <typename Protocol>
fidl::ServerEnd<Protocol> CreateEndpointsAndBind(fidl::Client<Protocol>& client) {
  auto [client_end, server_end] = *fidl::CreateEndpoints<Protocol>();
  client.Bind(std::move(client_end), async_get_default_dispatcher());
  return std::move(server_end);
}

// Read the content of "elf/job_id" in the runtime directory of an ELF component.
//
// |callback| will be issued with ZX_KOID_INVALID if there's any error.
// |moniker| is only used for error logging.
void ReadElfJobId(fidl::Client<fuchsia_io::Directory> runtime_dir, const std::string& moniker,
                  fit::callback<void(zx_koid_t)> cb) {
  fidl::Client<fuchsia_io::File> job_id_file;
  auto open_res = runtime_dir->Open(
      {fuchsia_io::OpenFlags::kRightReadable,
       {},
       "elf/job_id",
       fidl::ServerEnd<fuchsia_io::Node>(CreateEndpointsAndBind(job_id_file).TakeChannel())});
  if (!open_res.is_ok()) {
    LOGS(Error) << "Failed to open elf/job_id for " << moniker;
    return cb(ZX_KOID_INVALID);
  }
  job_id_file->Read(fuchsia_io::kMaxTransferSize)
      .Then([cb = cb.share(), moniker](fidl::Result<fuchsia_io::File::Read>& res) mutable {
        if (!cb) {
          return;
        }
        if (!res.is_ok()) {
          LOGS(Error) << "Failed to read elf/job_id for " << moniker << ": " << res.error_value();
          return cb(ZX_KOID_INVALID);
        }
        std::string job_id_str(reinterpret_cast<const char*>(res->data().data()),
                               res->data().size());
        // We use std::strtoull here because std::stoull is not exception-safe.
        char* end;
        zx_koid_t job_id = std::strtoull(job_id_str.c_str(), &end, 10);
        if (end != job_id_str.c_str() + job_id_str.size()) {
          LOGS(Error) << "Invalid elf/job_id for " << moniker << ": " << job_id_str;
          return cb(ZX_KOID_INVALID);
        }
        cb(job_id);
      });
  debug::MessageLoop::Current()->PostTimer(
      FROM_HERE, kMaxWaitMsForJobId,
      [cb = std::move(cb), file = std::move(job_id_file), moniker]() mutable {
        if (cb) {
          LOGS(Warn) << "Timeout reading elf/job_id for " << moniker;
          cb(ZX_KOID_INVALID);
        }
      });
}

std::string SeverityToString(int32_t severity) {
  switch (severity) {
    case fuchsia_logging::LOG_TRACE:
      return "TRACE";
    case fuchsia_logging::LOG_DEBUG:
      return "DEBUG";
    case fuchsia_logging::LOG_INFO:
      return "INFO";
    case fuchsia_logging::LOG_WARNING:
      return "WARNING";
    case fuchsia_logging::LOG_ERROR:
      return "ERROR";
    case fuchsia_logging::LOG_FATAL:
      return "FATAL";
  }
  return "INVALID";
}

void SendLogs(DebugAgent* debug_agent, std::vector<fuchsia::diagnostics::FormattedContent> batch) {
  debug_ipc::NotifyIO notify;
  notify.timestamp = GetNowTimestamp();
  notify.process_koid = 0;
  notify.type = debug_ipc::NotifyIO::Type::kStderr;

  for (auto& content : batch) {
    auto res =
        diagnostics::accessor2logger::ConvertFormattedContentToLogMessages(std::move(content));
    if (res.is_error()) {
      LOGS(Warn) << "Failed to parse log: " << res.error();
    } else {
      for (auto& msg : res.value()) {
        if (msg.is_error()) {
          LOGS(Warn) << "Failed to parse log: " << msg.error();
        } else {
          notify.data += SeverityToString(msg.value().severity) + ": ";
          notify.data.insert(notify.data.end(), msg.value().msg.begin(), msg.value().msg.end());
          notify.data.push_back('\n');
        }
      }
    }
  }

  debug_agent->SendNotification(notify);
}

}  // namespace

void ZirconComponentManager::GetNextComponentEvent() {
  event_stream_client_->GetNext().Then([this](auto& result) {
    if (result.is_error()) {
      LOGS(Error) << "Failed to GetNextComponentEvent: " << result.error_value();
    } else {
      for (auto& event : result->events()) {
        OnComponentEvent(std::move(event));
      }
      GetNextComponentEvent();
    }
  });
}

ZirconComponentManager::ZirconComponentManager(SystemInterface* system_interface)
    : ComponentManager(system_interface), weak_factory_(this) {
  // 1. Subscribe to "debug_started" and "stopped" events.
  auto event_stream_res = component::Connect<fuchsia_component::EventStream>();
  if (!event_stream_res.is_ok()) {
    LOGS(Error) << "Failed to connect to fuchsia.component.EventStream: "
                << event_stream_res.status_string();
  } else {
    fidl::SyncClient client(std::move(*event_stream_res));
    auto res = client->WaitForReady();
    if (!res.is_ok()) {
      LOGS(Error) << "Failed to WaitForReady: " << res.error_value();
    } else {
      event_stream_client_.Bind(client.TakeClientEnd(), async_get_default_dispatcher());
      GetNextComponentEvent();
    }
  }

  // 2. List existing components via fuchsia.sys2.RealmQuery.
  auto realm_query_res =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (!realm_query_res.is_ok()) {
    LOGS(Error) << "Failed to connect to fuchsia.sys2.RealmQuery.root: "
                << realm_query_res.status_string();
    return;
  }
  fidl::SyncClient realm_query(std::move(*realm_query_res));

  auto all_instances_res = realm_query->GetAllInstances();
  if (!all_instances_res.is_ok()) {
    LOGS(Error) << "Failed to GetAllInstances: " << all_instances_res.error_value();
    return;
  }

  fidl::SyncClient instance_it(std::move(all_instances_res->iterator()));
  auto deferred_ready =
      std::make_shared<fit::deferred_callback>([weak_this = weak_factory_.GetWeakPtr()] {
        if (weak_this && weak_this->ready_callback_)
          weak_this->ready_callback_();
      });
  while (true) {
    auto instances_res = instance_it->Next();
    if (!instances_res.is_ok() || instances_res->infos().empty()) {
      break;
    }
    for (auto& instance : instances_res->infos()) {
      if (!instance.moniker() || instance.moniker()->empty() || !instance.url() ||
          instance.url()->empty()) {
        continue;
      }
      if (!instance.resolved_info() || !instance.resolved_info()->execution_info()) {
        // The component is not running.
        continue;
      }
      fidl::Client<fuchsia_io::Directory> runtime_dir;
      auto open_res = realm_query->Open(
          {*instance.moniker(),
           fuchsia_sys2::OpenDirType::kRuntimeDir,
           fuchsia_io::OpenFlags::kRightReadable,
           {},
           ".",
           fidl::ServerEnd<fuchsia_io::Node>(CreateEndpointsAndBind(runtime_dir).TakeChannel())});
      if (!open_res.is_ok()) {
        continue;
      }
      // Remove the "." at the beginning of the moniker. It's safe because moniker is not empty.
      std::string moniker = instance.moniker()->substr(1);
      ReadElfJobId(std::move(runtime_dir), moniker,
                   [weak_this = weak_factory_.GetWeakPtr(), moniker, url = *instance.url(),
                    deferred_ready](zx_koid_t job_id) {
                     if (weak_this && job_id != ZX_KOID_INVALID) {
                       weak_this->running_component_info_[job_id] = {.moniker = moniker,
                                                                     .url = url};
                     }
                   });
    }
  }
}

void ZirconComponentManager::SetReadyCallback(fit::callback<void()> callback) {
  if (ready_callback_) {
    ready_callback_ = std::move(callback);
  } else {
    debug::MessageLoop::Current()->PostTask(FROM_HERE,
                                            [cb = std::move(callback)]() mutable { cb(); });
  }
}

void ZirconComponentManager::OnComponentEvent(fuchsia_component::Event event) {
  if (!event.payload() || !event.header() || !event.header()->event_type() ||
      !event.header()->component_url() || !event.header()->moniker() ||
      event.header()->moniker()->empty()) {
    return;
  }
  const auto& moniker = *event.header()->moniker();
  const auto& url = *event.header()->component_url();
  switch (*event.header()->event_type()) {
    case fuchsia_component::EventType::kDebugStarted:
      if (debug_agent_) {
        debug_agent_->OnComponentStarted(moniker, url);
      }
      if (event.payload()->debug_started() && event.payload()->debug_started()->runtime_dir()) {
        auto& runtime_dir = *event.payload()->debug_started()->runtime_dir();
        auto& break_on_start = *event.payload()->debug_started()->break_on_start();
        ReadElfJobId({std::move(runtime_dir), async_get_default_dispatcher()}, moniker,
                     [weak_this = weak_factory_.GetWeakPtr(), moniker, url,
                      break_on_start = std::move(break_on_start)](zx_koid_t job_id) mutable {
                       if (weak_this && job_id != ZX_KOID_INVALID) {
                         weak_this->running_component_info_[job_id] = {.moniker = moniker,
                                                                       .url = url};
                         DEBUG_LOG(Process) << "Component started job_id=" << job_id
                                            << " moniker=" << moniker << " url=" << url;
                       }
                       // Explicitly reset break_on_start to indicate the component manager that
                       // processes can be spawned.
                       break_on_start.reset();
                     });
      }
      break;
    case fuchsia_component::EventType::kStopped: {
      if (debug_agent_) {
        debug_agent_->OnComponentExited(moniker, *event.header()->component_url());
      }
      for (auto it = running_component_info_.begin(); it != running_component_info_.end(); it++) {
        if (it->second.moniker == moniker) {
          DEBUG_LOG(Process) << "Component stopped job_id=" << it->first
                             << " moniker=" << it->second.moniker << " url=" << it->second.url;
          running_component_info_.erase(it);
          expected_v2_components_.erase(moniker);
          break;
        }
      }
      break;
    }
    default:
      FX_NOTREACHED();
  }
}

std::optional<debug_ipc::ComponentInfo> ZirconComponentManager::FindComponentInfo(
    zx_koid_t job_koid) const {
  if (auto it = running_component_info_.find(job_koid); it != running_component_info_.end())
    return it->second;
  return std::nullopt;
}

// We need a class to help to launch a test because the lifecycle of GetEvents callbacks
// are undetermined.
class ZirconComponentManager::TestLauncher : public fxl::RefCountedThreadSafe<TestLauncher> {
 public:
  // This function can only be called once.
  debug::Status Launch(std::string url, std::optional<std::string> realm,
                       std::vector<std::string> case_filters,
                       ZirconComponentManager* component_manager, DebugAgent* debug_agent) {
    if (realm) {
      return debug::Status("Realm parameter is not supported yet");
    }
    test_url_ = std::move(url);
    component_manager_ = component_manager->GetWeakPtr();
    debug_agent_ = debug_agent ? debug_agent->GetWeakPtr() : nullptr;

    if (component_manager->running_tests_info_.count(test_url_))
      return debug::Status("Test " + test_url_ + " is already launched");

    auto run_builder_res = component::Connect<fuchsia_test_manager::RunBuilder>();
    if (!run_builder_res.is_ok()) {
      return debug::ZxStatus(run_builder_res.error_value());
    }
    fidl::SyncClient run_builder(std::move(*run_builder_res));

    DEBUG_LOG(Process) << "Launching test url=" << test_url_;

    fuchsia_test_manager::RunOptions run_options;
    run_options.case_filters_to_run() = std::move(case_filters);
    run_options.arguments() = {"--gtest_break_on_failure"};  // does no harm to rust tests.
    auto add_suite_res = run_builder->AddSuite(
        {test_url_, std::move(run_options), CreateEndpointsAndBind(suite_controller_)});
    if (!add_suite_res.is_ok()) {
      return debug::ZxStatus(add_suite_res.error_value().status(),
                             add_suite_res.error_value().FormatDescription());
    }
    auto build_res = run_builder->Build(CreateEndpointsAndBind(run_controller_));
    if (!build_res.is_ok()) {
      return debug::ZxStatus(build_res.error_value().status(),
                             build_res.error_value().FormatDescription());
    }
    run_controller_->GetEvents().Then(
        [self = fxl::RefPtr<TestLauncher>(this)](auto& res) { self->OnRunEvents(std::move(res)); });
    suite_controller_->GetEvents().Then([self = fxl::RefPtr<TestLauncher>(this)](auto& res) {
      self->OnSuiteEvents(std::move(res));
    });
    component_manager->running_tests_info_[test_url_] = {};
    return debug::Status();
  }

  ~TestLauncher() {
    DEBUG_LOG(Process) << "Test finished url=" << test_url_;
    if (debug_agent_) {
      debug_agent_->OnTestComponentExited(test_url_);
    }
  }

 private:
  // Stdout and stderr are in case_artifact. Logs are in suite_artifact. Others are ignored.
  // NOTE: custom.component_moniker in suite_artifact is NOT the moniker of the test!
  void OnSuiteEvents(fidl::Result<fuchsia_test_manager::SuiteController::GetEvents> result) {
    if (!component_manager_ || !result.is_ok() || result->events().empty()) {
      (void)suite_controller_.UnbindMaybeGetEndpoint();  // Otherwise run_controller won't return.
      if (result.is_error())
        LOGS(Warn) << "Failed to launch test: " << result.error_value();
      if (component_manager_)
        component_manager_->running_tests_info_.erase(test_url_);
      return;
    }
    for (auto& event : result->events()) {
      FX_CHECK(event.payload());
      if (event.payload()->case_found()) {
        auto& test_info = component_manager_->running_tests_info_[test_url_];
        // Test cases should come in order.
        FX_CHECK(test_info.case_names.size() == event.payload()->case_found()->identifier());
        test_info.case_names.push_back(event.payload()->case_found()->test_case_name());
        if (event.payload()->case_found()->test_case_name().find_first_of('.') !=
            std::string::npos) {
          test_info.ignored_process = 1;
        }
      } else if (event.payload()->case_artifact()) {
        if (auto proc = GetDebuggedProcess(event.payload()->case_artifact()->identifier())) {
          auto& artifact = event.payload()->case_artifact()->artifact();
          if (artifact.stdout_()) {
            proc->SetStdout(std::move(artifact.stdout_().value()));
          } else if (artifact.stderr_()) {
            proc->SetStderr(std::move(artifact.stderr_().value()));
          }
        } else {
          // This usually happens when the process has terminated, e.g.
          //   - Rust test runner prints an extra message after the test finishes.
          //   - The process is killed by the debugger.
          //
          // Don't print anything because it's very common.
        }
      } else if (event.payload()->suite_artifact()) {
        auto& artifact = event.payload()->suite_artifact()->artifact();
        if (artifact.log()) {
          FX_CHECK(artifact.log()->batch());
          log_listener_.Bind(artifact.log()->batch().value().TakeChannel());
          log_listener_->GetNext(
              [self = fxl::RefPtr<TestLauncher>(this)](auto res) { self->OnLog(std::move(res)); });
        }
      }
    }
    suite_controller_->GetEvents().Then([self = fxl::RefPtr<TestLauncher>(this)](auto& res) {
      self->OnSuiteEvents(std::move(res));
    });
  }

  // See the comment above |running_tests_info_| for the logic.
  DebuggedProcess* GetDebuggedProcess(uint32_t test_identifier) {
    auto& test_info = component_manager_->running_tests_info_[test_url_];
    auto& pids = test_info.pids;
    auto proc_idx = test_identifier + test_info.ignored_process;
    if (proc_idx < pids.size() && debug_agent_) {
      return debug_agent_->GetDebuggedProcess(pids[proc_idx]);
    }
    return nullptr;
  }

  // Unused but required by the test framework.
  void OnRunEvents(fidl::Result<fuchsia_test_manager::RunController::GetEvents> events) {
    if (!events->events().empty()) {
      FX_LOGS_FIRST_N(WARNING, 1) << "Not implemented yet";
      run_controller_->GetEvents().Then([self = fxl::RefPtr<TestLauncher>(this)](auto& res) {
        self->OnRunEvents(std::move(res));
      });
    } else {
      (void)run_controller_.UnbindMaybeGetEndpoint();
    }
  }

  // Handle logs.
  void OnLog(fuchsia::diagnostics::BatchIterator_GetNext_Result result) {
    if (result.is_response() && !result.response().batch.empty()) {
      if (debug_agent_) {
        SendLogs(debug_agent_.get(), std::move(result.response().batch));
      }
      log_listener_->GetNext(
          [self = fxl::RefPtr<TestLauncher>(this)](auto res) { self->OnLog(std::move(res)); });
    } else {
      if (result.is_err())
        LOGS(Error) << "Failed to read log";
      log_listener_.Unbind();  // Otherwise archivist won't terminate.
    }
  }

  fxl::WeakPtr<DebugAgent> debug_agent_;
  fxl::WeakPtr<ZirconComponentManager> component_manager_;
  std::string test_url_;
  fidl::Client<fuchsia_test_manager::RunController> run_controller_;
  fidl::Client<fuchsia_test_manager::SuiteController> suite_controller_;
  fuchsia::diagnostics::BatchIteratorPtr log_listener_;  // accessor2logger is still using hlcpp.
};

debug::Status ZirconComponentManager::LaunchTest(std::string url, std::optional<std::string> realm,
                                                 std::vector<std::string> case_filters) {
  return fxl::MakeRefCounted<TestLauncher>()->Launch(std::move(url), std::move(realm),
                                                     std::move(case_filters), this, debug_agent_);
}

debug::Status ZirconComponentManager::LaunchComponent(std::string url) {
  if (cpp20::ends_with(std::string_view{url}, ".cmx")) {
    return debug::Status("V1 components are no longer supported.");
  }

  constexpr char kParentMoniker[] = "core";
  constexpr char kCollection[] = "ffx-laboratory";

  // url: fuchsia-pkg://fuchsia.com/crasher#meta/cpp_crasher.cm
  size_t name_start = url.find_last_of('/') + 1;
  // name: cpp_crasher
  std::string name = url.substr(name_start, url.find_last_of('.') - name_start);
  // moniker: core/ffx-laboratory:cpp_crasher
  std::string moniker = std::string(kParentMoniker) + "/" + kCollection + ":" + name;

  if (expected_v2_components_.count(moniker)) {
    return debug::Status(url + " is already launched");
  }

  auto connect_res = component::Connect<fuchsia_sys2::LifecycleController>(
      "/svc/fuchsia.sys2.LifecycleController.root");
  if (!connect_res.is_ok())
    return debug::ZxStatus(connect_res.error_value());
  fidl::SyncClient lifecycle_controller(std::move(*connect_res));

  DEBUG_LOG(Process) << "Launching component url=" << url << " moniker=" << moniker;

  auto create_child = [&]() {
    fuchsia_component_decl::Child child_decl;
    child_decl.name() = name;
    child_decl.url() = url;
    child_decl.startup() = fuchsia_component_decl::StartupMode::kLazy;
    return lifecycle_controller->CreateInstance(
        {kParentMoniker, {kCollection}, std::move(child_decl), {}});
  };
  auto create_res = create_child();
  if (create_res.is_error() && create_res.error_value().is_domain_error() &&
      create_res.error_value().domain_error() ==
          fuchsia_sys2::CreateError::kInstanceAlreadyExists) {
    auto destroy_res = lifecycle_controller->DestroyInstance({kParentMoniker, {name, kCollection}});
    if (destroy_res.is_error()) {
      return debug::Status("Failed to destroy component " + moniker + ": " +
                           destroy_res.error_value().FormatDescription());
    }
    create_res = create_child();
  }
  if (create_res.is_error()) {
    return debug::Status("Failed to create the component: " +
                         create_res.error_value().FormatDescription());
  }

  // LifecycleController::Start accepts relative monikers.
  fidl::Client<fuchsia_component::Binder> binder_client_end;
  auto start_res =
      lifecycle_controller->StartInstance({moniker, CreateEndpointsAndBind(binder_client_end)});
  if (start_res.is_error()) {
    return debug::Status("Failed to start the component: " +
                         start_res.error_value().FormatDescription());
  }

  expected_v2_components_.insert(moniker);
  return debug::Status();
}

bool ZirconComponentManager::OnProcessStart(const ProcessHandle& process, StdioHandles* out_stdio,
                                            std::string* process_name_override) {
  if (auto component = ComponentManager::FindComponentInfo(process)) {
    if (expected_v2_components_.count(component->moniker)) {
      // It'll be erased in the stopped event.
      return true;
    }
    if (auto it = running_tests_info_.find(component->url); it != running_tests_info_.end()) {
      size_t idx = it->second.pids.size();
      it->second.pids.push_back(process.GetKoid());
      if (idx < it->second.ignored_process) {
        return false;
      }
      idx -= it->second.ignored_process;
      if (idx < it->second.case_names.size()) {
        *process_name_override = it->second.case_names[idx];
      }
      return true;
    }
  }
  return false;
}

}  // namespace debug_agent
