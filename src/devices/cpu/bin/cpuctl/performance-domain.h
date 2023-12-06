// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CPU_BIN_CPUCTL_PERFORMANCE_DOMAIN_H_
#define SRC_DEVICES_CPU_BIN_CPUCTL_PERFORMANCE_DOMAIN_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.cpu.ctrl/cpp/wire.h>

#include <tuple>
#include <utility>
#include <vector>

namespace cpuctrl = fuchsia_hardware_cpu_ctrl;

class CpuPerformanceDomain {
 public:
  static zx::result<CpuPerformanceDomain> CreateFromPath(const std::string& path);
  std::pair<zx_status_t, uint64_t> GetNumLogicalCores();
  std::tuple<zx_status_t, uint64_t, cpuctrl::wire::CpuPerformanceStateInfo>
  GetCurrentPerformanceState();
  const std::vector<cpuctrl::wire::CpuPerformanceStateInfo>& GetPerformanceStates();
  zx_status_t SetPerformanceState(uint32_t new_performance_state);

 protected:
  // Don't allow explicit construction.
  explicit CpuPerformanceDomain(fidl::ClientEnd<cpuctrl::Device> cpu_client)
      : cpu_client_(std::move(cpu_client)) {}

 private:
  fidl::WireSyncClient<cpuctrl::Device> cpu_client_;

  // Don't use this directly. Instead call GetPerformanceStates().
  std::vector<cpuctrl::wire::CpuPerformanceStateInfo> cached_pstates_;
};

#endif  // SRC_DEVICES_CPU_BIN_CPUCTL_PERFORMANCE_DOMAIN_H_
