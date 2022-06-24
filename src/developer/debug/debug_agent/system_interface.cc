// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/system_interface.h"

#include "src/developer/debug/debug_agent/component_manager.h"

namespace debug_agent {

namespace {

using debug_ipc::ProcessTreeRecord;

ProcessTreeRecord GetProcessTreeFrom(const JobHandle& job,
                                     const ComponentManager& component_manager) {
  ProcessTreeRecord result;
  result.type = ProcessTreeRecord::Type::kJob;
  result.koid = job.GetKoid();
  result.name = job.GetName();
  if (auto component_info = component_manager.FindComponentInfo(job.GetKoid())) {
    result.component_url = component_info->url;
    result.component_moniker = component_info->moniker;
  }

  for (const auto& child_process : job.GetChildProcesses()) {
    ProcessTreeRecord& proc_record = result.children.emplace_back();
    proc_record.type = ProcessTreeRecord::Type::kProcess;
    proc_record.koid = child_process->GetKoid();
    proc_record.name = child_process->GetName();
  }

  for (const auto& child_job : job.GetChildJobs())
    result.children.push_back(GetProcessTreeFrom(*child_job, component_manager));

  return result;
}

}  // namespace

ProcessTreeRecord SystemInterface::GetProcessTree() {
  const ComponentManager& component_manager = GetComponentManager();
  if (std::unique_ptr<JobHandle> root_job = GetRootJob())
    return GetProcessTreeFrom(*root_job, component_manager);
  return ProcessTreeRecord();
}

std::unique_ptr<JobHandle> SystemInterface::GetJob(zx_koid_t job_koid) const {
  if (std::unique_ptr<JobHandle> root_job = GetRootJob())
    return root_job->FindJob(job_koid);
  return nullptr;
}

std::unique_ptr<ProcessHandle> SystemInterface::GetProcess(zx_koid_t process_koid) const {
  if (std::unique_ptr<JobHandle> root_job = GetRootJob())
    return root_job->FindProcess(process_koid);
  return nullptr;
}

}  // namespace debug_agent
