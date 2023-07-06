// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_DRIVER_INSPECTOR_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_DRIVER_INSPECTOR_H_

#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/stdcompat/span.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <deque>
#include <memory>
#include <mutex>

namespace wlan::iwlwifi {

// Options object for the DriverInspector.
struct DriverInspectorOptions final {
  // Name of the root node of this DriverInspector.
  const char* root_name = "driver_inspector";

  // Total size of the Inspect VMO to allocate.  The total usable size will be slightly smaller than
  // this, due to Inspect file format overhead.
  size_t vmo_size = 8 * 1024 * 1024;

  // Size reserved in the VMO for core dumps.
  size_t core_dump_capacity = 4 * 1024 * 1024;
};

// DriverInspector manages the Inspect tree hierarchy for a driver.
// Thread-safety: this class is thread-safe.
class DriverInspector {
 public:
  explicit DriverInspector(async_dispatcher* dispatcher, component::OutgoingDirectory& out,
                           DriverInspectorOptions options = {});
  ~DriverInspector();

  // Publish a core dump under `core_dump_name`.  Old core dumps may be removed to make room.
  zx_status_t PublishCoreDump(const char* core_dump_name, cpp20::span<const char> core_dump);

  // Get the root of this drivers' Inspect tree hierarchy.
  ::inspect::Node& GetRoot();
  const ::inspect::Node& GetRoot() const;

  // Get a read-only copy of this Inspect tree's backing VMO.
  ::zx::vmo DuplicateVmo() const;

  inspect::Inspector* inspector() { return inspector_.get(); }

 private:
  struct CoreDumpEntry {
    ::inspect::ByteVectorProperty dump_ = {};
    size_t dump_size_ = 0;
  };

  std::unique_ptr<::inspect::Inspector> inspector_;
  // Another wrapper of ::inspect::Inspector, alternative of set_inspect_vmo().
  std::unique_ptr<inspect::ComponentInspector> component_inspector_;
  ::inspect::Node root_node_;

  std::mutex core_dump_mutex_;
  const size_t core_dump_capacity_ = 0;
  std::deque<CoreDumpEntry> core_dumps_ __TA_GUARDED(core_dump_mutex_);
};

}  // namespace wlan::iwlwifi

// This subclass-as-an-alias exists purely to be compatible with C code that uses the
// `driver_inspector` type as a struct pointer.
struct driver_inspector final : public wlan::iwlwifi::DriverInspector {};

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_DRIVER_INSPECTOR_H_
