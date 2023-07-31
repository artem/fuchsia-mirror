// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "targets.h"

#include <lib/fit/function.h>
#include <lib/stdcompat/span.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/thread.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/system/ulib/elf-search/include/elf-search.h>
#include <zircon/types.h>

#include <cstddef>
#include <unordered_map>
#include <utility>
#include <vector>

#include <src/lib/unwinder/module.h>

zx::result<> profiler::JobTarget::ForEachProcess(
    const fit::function<zx::result<>(cpp20::span<const zx_koid_t> job_path,
                                     const ProcessTarget& target)>& f) const {
  std::vector<zx_koid_t> job_path{ancestry.begin(), ancestry.end()};
  job_path.push_back(job_id);
  for (const auto& [_, process] : processes) {
    zx::result<> res = f(job_path, process);
    if (res.is_error()) {
      return res;
    }
  }
  for (const auto& [_, job] : child_jobs) {
    zx::result<> res = job.ForEachProcess(f);
    if (res.is_error()) {
      return res;
    }
  }
  return zx::ok();
}

zx::result<> profiler::JobTarget::ForEachJob(
    const fit::function<zx::result<>(const JobTarget& target)>& f) const {
  zx::result res = f(*this);
  if (res.is_error()) {
    return res;
  }

  for (const auto& [_, job] : child_jobs) {
    zx::result res = job.ForEachJob(f);
    if (res.is_error()) {
      return res;
    }
  }
  return zx::ok();
}

zx::result<> profiler::JobTarget::AddJob(cpp20::span<const zx_koid_t> ancestry, JobTarget&& job) {
  if (ancestry.empty()) {
    zx_koid_t job_id = job.job_id;
    auto [_, emplaced] = child_jobs.try_emplace(job_id, std::move(job));
    return zx::make_result(emplaced ? ZX_OK : ZX_ERR_ALREADY_EXISTS);
  }
  auto it = child_jobs.find(ancestry[0]);
  if (it == child_jobs.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return it->second.AddJob(ancestry.subspan(1), std::move(job));
}

zx::result<> profiler::JobTarget::AddProcess(cpp20::span<const zx_koid_t> job_path,
                                             ProcessTarget&& process) {
  if (job_path.empty()) {
    zx_koid_t pid = process.pid;
    auto [_, emplaced] = processes.try_emplace(pid, std::move(process));
    return zx::make_result(emplaced ? ZX_OK : ZX_ERR_ALREADY_EXISTS);
  }
  auto next_child = child_jobs.find(job_path[0]);
  if (next_child == child_jobs.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return next_child->second.AddProcess(job_path.subspan(1), std::move(process));
}

zx::result<> profiler::JobTarget::AddThread(cpp20::span<const zx_koid_t> job_path, zx_koid_t pid,
                                            ThreadTarget&& thread) {
  if (job_path.empty()) {
    auto process = processes.find(pid);
    if (process == processes.end()) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    zx_koid_t tid = thread.tid;
    auto [_, emplaced] = process->second.threads.try_emplace(tid, std::move(thread));
    return zx::make_result(emplaced ? ZX_OK : ZX_ERR_ALREADY_EXISTS);
  }

  auto next_child = child_jobs.find(job_path[0]);
  if (next_child == child_jobs.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return next_child->second.AddThread(job_path.subspan(1), pid, std::move(thread));
}

zx::result<> profiler::JobTarget::RemoveThread(cpp20::span<const zx_koid_t> job_path, zx_koid_t pid,
                                               zx_koid_t tid) {
  if (job_path.empty()) {
    auto process = processes.find(pid);
    if (process == processes.end()) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    size_t num_removed = process->second.threads.erase(tid);
    return zx::make_result(num_removed == 1 ? ZX_OK : ZX_ERR_NOT_FOUND);
  }

  auto next_child = child_jobs.find(job_path[0]);
  if (next_child == child_jobs.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return next_child->second.RemoveThread(job_path.subspan(1), pid, tid);
}

zx::result<std::vector<zx_koid_t>> GetChildrenTids(const zx::process& process) {
  size_t num_threads;
  zx_status_t status = process.get_info(ZX_INFO_PROCESS_THREADS, nullptr, 0, nullptr, &num_threads);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to get process thread info (#threads)";
    return zx::error(status);
  }
  if (num_threads < 1) {
    FX_LOGS(ERROR) << "failed to get any threads associated with the process";
    return zx::error(ZX_ERR_BAD_STATE);
  }

  zx_koid_t threads[num_threads];
  size_t records_read;
  status = process.get_info(ZX_INFO_PROCESS_THREADS, threads, num_threads * sizeof(threads[0]),
                            &records_read, nullptr);

  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to get process thread info";
    return zx::error(status);
  }

  if (records_read != num_threads) {
    FX_LOGS(ERROR) << "records_read != num_threads";
    return zx::error(ZX_ERR_BAD_STATE);
  }

  std::vector<zx_koid_t> children{threads, threads + num_threads};
  return zx::ok(children);
}

zx::result<profiler::ProcessTarget> profiler::MakeProcessTarget(zx::process process) {
  zx::result<std::vector<zx_koid_t>> children = GetChildrenTids(process);
  if (children.is_error()) {
    return children.take_error();
  }
  zx_info_handle_basic_t handle_info;
  zx_status_t res =
      process.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info), nullptr, nullptr);
  if (res != ZX_OK) {
    return zx::error(res);
  }
  std::unordered_map<zx_koid_t, profiler::ThreadTarget> threads;
  for (auto child_tid : *children) {
    zx::thread child_thread;
    zx_status_t res = process.get_child(child_tid, ZX_DEFAULT_THREAD_RIGHTS, &child_thread);
    if (res != ZX_OK) {
      FX_PLOGS(ERROR, res) << "Failed to get handle for child (tid: " << child_tid << ")";
      continue;
    }
    threads.try_emplace(child_tid, profiler::ThreadTarget{std::move(child_thread), child_tid});
  }
  profiler::ProcessTarget process_target{std::move(process), handle_info.koid, std::move(threads)};

  elf_search::ForEachModule(*zx::unowned_process{process_target.handle},
                            [&process_target](const elf_search::ModuleInfo& info) {
                              process_target.unwinder_data->modules.emplace_back(
                                  info.vaddr, &process_target.unwinder_data->memory,
                                  unwinder::Module::AddressMode::kProcess);
                            });
  return zx::ok(std::move(process_target));
}

zx::result<profiler::JobTarget> profiler::MakeJobTarget(zx::job job,
                                                        cpp20::span<const zx_koid_t> ancestry) {
  size_t num_child_jobs;
  zx_status_t status = job.get_info(ZX_INFO_JOB_CHILDREN, nullptr, 0, nullptr, &num_child_jobs);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to query number of job children";
    return zx::error(status);
  }

  zx_info_handle_basic_t info;
  zx_status_t info_res = job.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (info_res != ZX_OK) {
    FX_PLOGS(ERROR, info_res) << "failed to make process_target";
    return zx::error(status);
  }
  zx_koid_t job_id = info.koid;

  // Provide each of this job's children their ancestry, which is this job's ancestry, prepended to
  // this job's job id.
  std::vector<zx_koid_t> child_job_ancestry{ancestry.begin(), ancestry.end()};
  child_job_ancestry.push_back(job_id);

  std::unordered_map<zx_koid_t, profiler::JobTarget> child_job_targets;
  if (num_child_jobs > 0) {
    zx_koid_t child_jobs[num_child_jobs];
    status = job.get_info(ZX_INFO_JOB_CHILDREN, child_jobs, sizeof(child_jobs), nullptr, nullptr);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "failed to get job children";
      return zx::error(status);
    }

    for (zx_koid_t child_koid : child_jobs) {
      zx::job child_job;
      zx_status_t res = job.get_child(child_koid, ZX_DEFAULT_JOB_RIGHTS, &child_job);
      if (status != ZX_OK) {
        FX_PLOGS(ERROR, res) << "failed to get job: " << child_koid;
        return zx::error(status);
      }
      zx::result<profiler::JobTarget> child_job_target =
          MakeJobTarget(std::move(child_job), child_job_ancestry);
      if (child_job_target.is_error()) {
        FX_PLOGS(ERROR, child_job_target.status_value()) << "failed to make job_target";
        return zx::error(status);
      }
      child_job_target->ancestry = child_job_ancestry;
      child_job_targets.try_emplace(child_koid, std::move(*child_job_target));
    }
  }

  size_t num_processes;
  status = job.get_info(ZX_INFO_JOB_PROCESSES, nullptr, 0, nullptr, &num_processes);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to query number of job processes";
    return zx::error(status);
  }
  std::unordered_map<zx_koid_t, profiler::ProcessTarget> process_targets;
  if (num_processes > 0) {
    zx_koid_t processes[num_processes];
    status = job.get_info(ZX_INFO_JOB_PROCESSES, processes, sizeof(processes), nullptr, nullptr);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "failed to get job processes";
      return zx::error(status);
    }

    for (zx_koid_t process_koid : processes) {
      zx::process process;
      zx_status_t res = job.get_child(process_koid, ZX_DEFAULT_PROCESS_RIGHTS, &process);
      if (status != ZX_OK) {
        FX_PLOGS(ERROR, res) << "failed to get process: " << process_koid;
        return zx::error(status);
      }
      zx::result<profiler::ProcessTarget> process_target =
          profiler::MakeProcessTarget(std::move(process));

      if (process_target.is_error()) {
        FX_PLOGS(ERROR, process_target.status_value()) << "failed to make process_target";
        return zx::error(status);
      }
      process_targets.try_emplace(process_koid, std::move(*process_target));
    }
  }
  return zx::ok(profiler::JobTarget{std::move(job), job_id, std::move(process_targets),
                                    std::move(child_job_targets), ancestry});
}

zx::result<profiler::JobTarget> profiler::MakeJobTarget(zx::job job) {
  return MakeJobTarget(std::move(job), cpp20::span<const zx_koid_t>{});
}

zx::result<> profiler::TargetTree::AddJob(JobTarget&& job) {
  return AddJob(cpp20::span<const zx_koid_t>{}, std::move(job));
}

zx::result<> profiler::TargetTree::AddJob(cpp20::span<const zx_koid_t> ancestry, JobTarget&& job) {
  if (ancestry.empty()) {
    zx_koid_t job_id = job.job_id;
    auto [it, emplaced] = jobs_.try_emplace(job_id, std::move(job));
    return zx::make_result(emplaced ? ZX_OK : ZX_ERR_ALREADY_EXISTS);
  }

  zx_koid_t next_child_koid = ancestry[0];
  auto it = jobs_.find(next_child_koid);
  return it == jobs_.end() ? zx::error(ZX_ERR_NOT_FOUND)
                           : it->second.AddJob(ancestry.subspan(1), std::move(job));
}

zx::result<> profiler::TargetTree::AddProcess(ProcessTarget&& process) {
  return AddProcess(cpp20::span<const zx_koid_t>{}, std::move(process));
}

zx::result<> profiler::TargetTree::AddProcess(cpp20::span<const zx_koid_t> job_path,
                                              ProcessTarget&& process) {
  if (job_path.empty()) {
    zx_koid_t pid = process.pid;
    auto [it, emplaced] = processes_.try_emplace(pid, std::move(process));
    return zx::make_result(emplaced ? ZX_OK : ZX_ERR_ALREADY_EXISTS);
  }

  zx_koid_t next_child_koid = job_path[0];
  auto it = jobs_.find(next_child_koid);
  return it == jobs_.end() ? zx::error(ZX_ERR_NOT_FOUND)
                           : it->second.AddProcess(job_path.subspan(1), std::move(process));
}

zx::result<> profiler::TargetTree::AddThread(zx_koid_t pid, ThreadTarget&& thread) {
  return AddThread(cpp20::span<const zx_koid_t>{}, pid, std::move(thread));
}

zx::result<> profiler::TargetTree::RemoveThread(zx_koid_t pid, zx_koid_t tid) {
  return RemoveThread(cpp20::span<const zx_koid_t>{}, pid, tid);
}

zx::result<> profiler::TargetTree::AddThread(cpp20::span<const zx_koid_t> job_path, zx_koid_t pid,
                                             ThreadTarget&& thread) {
  if (job_path.empty()) {
    auto it = processes_.find(pid);
    if (it == processes_.end()) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    zx_koid_t tid = thread.tid;
    auto [_, emplaced] = it->second.threads.try_emplace(tid, std::move(thread));
    return zx::make_result(emplaced ? ZX_OK : ZX_ERR_ALREADY_EXISTS);
  }

  zx_koid_t next_child_koid = job_path[0];
  auto it = jobs_.find(next_child_koid);
  return it == jobs_.end() ? zx::error(ZX_ERR_NOT_FOUND)
                           : it->second.AddThread(job_path.subspan(1), pid, std::move(thread));
}

zx::result<> profiler::TargetTree::RemoveThread(cpp20::span<const zx_koid_t> job_path,
                                                zx_koid_t pid, zx_koid_t tid) {
  if (job_path.empty()) {
    auto it = processes_.find(pid);
    if (it == processes_.end()) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    size_t num_erased = it->second.threads.erase(tid);
    return zx::make_result(num_erased == 1 ? ZX_OK : ZX_ERR_NOT_FOUND);
  }

  zx_koid_t next_child_koid = job_path[0];
  auto it = jobs_.find(next_child_koid);
  return it == jobs_.end() ? zx::error(ZX_ERR_NOT_FOUND)
                           : it->second.RemoveThread(job_path.subspan(1), pid, tid);
}

void profiler::TargetTree::Clear() {
  jobs_.clear();
  processes_.clear();
}

zx::result<> profiler::TargetTree::ForEachJob(
    const fit::function<zx::result<>(const JobTarget& target)>& f) {
  for (const auto& [_, job] : jobs_) {
    zx::result<> res = job.ForEachJob(f);
    if (res.is_error()) {
      return res;
    }
  }
  return zx::ok();
}

zx::result<> profiler::TargetTree::ForEachProcess(
    const fit::function<zx::result<>(cpp20::span<const zx_koid_t> job_path,
                                     const ProcessTarget& target)>& f) {
  for (const auto& [_, process] : processes_) {
    zx::result res = f(cpp20::span<const zx_koid_t>{}, process);
    if (res.is_error()) {
      return res;
    }
  }
  for (const auto& [_, job] : jobs_) {
    zx::result<> res = job.ForEachProcess(f);
    if (res.is_error()) {
      return res;
    }
  }
  return zx::ok();
}
