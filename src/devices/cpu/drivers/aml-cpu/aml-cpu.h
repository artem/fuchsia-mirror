// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CPU_DRIVERS_AML_CPU_AML_CPU_H_
#define SRC_DEVICES_CPU_DRIVERS_AML_CPU_AML_CPU_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.cpu.ctrl/cpp/wire.h>
#include <fidl/fuchsia.hardware.power/cpp/wire.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/inspect/cpp/inspector.h>

#include <mutex>
#include <vector>

#include <soc/aml-common/aml-cpu-metadata.h>

namespace amlogic_cpu {

// Fragments are provided to this driver in groups of 4. Fragments are provided as follows:
// [4 fragments for cluster 0]
// [4 fragments for cluster 1]
// [...]
// [4 fragments for cluster n]
// Each fragment is a combination of the fixed string + id.
constexpr size_t kFragmentsPerPfDomain = 4;
constexpr size_t kFragmentsPerPfDomainA5 = 2;
constexpr size_t kFragmentsPerPfDomainA1 = 1;

constexpr zx_off_t kCpuVersionOffset = 0x220;
constexpr zx_off_t kCpuVersionOffsetA5 = 0x300;
constexpr zx_off_t kCpuVersionOffsetA1 = 0x220;

struct AmlCpuConfiguration {
  pdev_device_info_t info;
  uint32_t metadata_type;
  size_t fragments_per_pf_domain;
  uint32_t cpu_version_packed;

  bool has_div16_clients;
  bool has_power_client;
};

class AmlCpu : public fidl::WireServer<fuchsia_hardware_cpu_ctrl::Device> {
 public:
  explicit AmlCpu(const std::vector<operating_point_t>& operating_points,
                  const perf_domain_t& perf_domain)
      : current_operating_point_(
            static_cast<uint32_t>(operating_points.size() -
                                  1))  // Assume the core is running at the slowest clock to begin.
        ,
        operating_points_(operating_points),
        perf_domain_(perf_domain) {}

  zx_status_t Init(fidl::ClientEnd<fuchsia_hardware_clock::Clock> plldiv16,
                   fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpudiv16,
                   fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpuscaler,
                   fidl::ClientEnd<fuchsia_hardware_power::Device> pwr);

  zx_status_t SetCurrentOperatingPointInternal(uint32_t requested_opp, uint32_t* out_opp);

  // Set CpuInfo in inspect.
  void SetCpuInfo(uint32_t cpu_version_packed);

  uint32_t GetCurrentOperatingPoint() {
    std::scoped_lock lock(lock_);
    return current_operating_point_;
  }

  uint32_t GetOperatingPointCount() const {
    return static_cast<uint32_t>(operating_points_.size());
  }

  const std::vector<operating_point_t>& GetOperatingPoints() { return operating_points_; }

  uint32_t GetCoreCount() const { return perf_domain_.core_count; }
  PerfDomainId GetDomainId() const { return perf_domain_.id; }
  const char* GetName() const { return perf_domain_.name; }

  // Fidl server interface implementation.
  void GetOperatingPointInfo(GetOperatingPointInfoRequestView request,
                             GetOperatingPointInfoCompleter::Sync& completer) override;
  void SetCurrentOperatingPoint(SetCurrentOperatingPointRequestView request,
                                SetCurrentOperatingPointCompleter::Sync& completer) override;
  void GetCurrentOperatingPoint(GetCurrentOperatingPointCompleter::Sync& completer) override;
  void GetOperatingPointCount(GetOperatingPointCountCompleter::Sync& completer) override;
  void GetNumLogicalCores(GetNumLogicalCoresCompleter::Sync& completer) override;
  void GetLogicalCoreId(GetLogicalCoreIdRequestView request,
                        GetLogicalCoreIdCompleter::Sync& completer) override;

  inspect::Inspector inspector_;
  inspect::Node cpu_info_ = inspector_.GetRoot().CreateChild("cpu_info_service");

 private:
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> plldiv16_;
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> cpudiv16_;
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> cpuscaler_;

  // This is from an optional fragment.
  fidl::WireSyncClient<fuchsia_hardware_power::Device> pwr_;

  std::mutex lock_;
  uint32_t current_operating_point_ __TA_GUARDED(lock_);
  const std::vector<operating_point_t> operating_points_;

  perf_domain_t perf_domain_;
};

std::vector<operating_point_t> PerformanceDomainOpPoints(const perf_domain_t& perf_domain,
                                                         std::vector<operating_point>& op_points);
zx_status_t GetPopularVoltageTable(const zx::resource& smc_resource, uint32_t* metadata_type);
zx::result<AmlCpuConfiguration> LoadConfiguration(ddk::PDevFidl& pdev);

}  // namespace amlogic_cpu

#endif  // SRC_DEVICES_CPU_DRIVERS_AML_CPU_AML_CPU_H_
