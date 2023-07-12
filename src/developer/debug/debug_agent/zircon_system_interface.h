// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_ZIRCON_SYSTEM_INTERFACE_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_ZIRCON_SYSTEM_INTERFACE_H_

#include <memory>

#include "src/developer/debug/debug_agent/system_interface.h"
#include "src/developer/debug/debug_agent/zircon_component_manager.h"
#include "src/developer/debug/debug_agent/zircon_job_handle.h"
#include "src/developer/debug/debug_agent/zircon_limbo_provider.h"

namespace debug_agent {

class BinaryLauncher;

class ZirconSystemInterface final : public SystemInterface {
 public:
  explicit ZirconSystemInterface();

  // SystemInterface implementation:
  uint32_t GetNumCpus() const override;
  uint64_t GetPhysicalMemory() const override;
  std::unique_ptr<JobHandle> GetRootJob() const override;
  std::unique_ptr<BinaryLauncher> GetLauncher() const override;
  ComponentManager& GetComponentManager() override;
  LimboProvider& GetLimboProvider() override { return limbo_provider_; }
  std::string GetSystemVersion() override;

  ZirconComponentManager& zircon_component_manager() { return component_manager_; }

 private:
  fidl::ClientEnd<fuchsia_io::Directory> svc_dir_;
  std::unique_ptr<ZirconJobHandle> root_job_;  // May be null.
  ZirconComponentManager component_manager_;
  ZirconLimboProvider limbo_provider_;
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_ZIRCON_SYSTEM_INTERFACE_H_
