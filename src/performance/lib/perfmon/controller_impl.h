// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_LIB_PERFMON_CONTROLLER_IMPL_H_
#define SRC_PERFORMANCE_LIB_PERFMON_CONTROLLER_IMPL_H_

#include <fidl/fuchsia.perfmon.cpu/cpp/fidl.h>

#include "src/lib/fxl/memory/weak_ptr.h"
#include "src/performance/lib/perfmon/controller.h"

namespace perfmon {
namespace internal {

class ControllerImpl final : public Controller {
 public:
  ControllerImpl(fidl::SyncClient<fuchsia_perfmon_cpu::Controller> controller_ptr,
                 uint32_t num_traces, uint32_t buffer_size_in_pages, const Config& config);
  ~ControllerImpl() override;

  bool Start() override;
  // It is ok to call this while stopped.
  void Stop() override;

  bool started() const override { return started_; }

  uint32_t num_traces() const override { return num_traces_; }

  const Config& config() const override { return config_; }

  bool GetBufferHandle(const std::string& name, uint32_t trace_num, zx::vmo* out_vmo) override;

  std::unique_ptr<Reader> GetReader() override;

 private:
  bool Stage();
  void Terminate();
  void Reset();

  fidl::SyncClient<fuchsia_perfmon_cpu::Controller> controller_ptr_;
  // The number of traces we will collect (== #cpus for now).
  uint32_t num_traces_;
  // This is the actual buffer size we use, in pages.
  const uint32_t buffer_size_in_pages_;
  const Config config_;

  // Set to true by |Start()|, false by |Stop()|.
  bool started_ = false;

  fxl::WeakPtrFactory<Controller> weak_ptr_factory_;
};

}  // namespace internal
}  // namespace perfmon

#endif  // SRC_PERFORMANCE_LIB_PERFMON_CONTROLLER_IMPL_H_
