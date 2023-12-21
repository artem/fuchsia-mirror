// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_RESUME_TASK_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_RESUME_TASK_H_

#include "src/devices/bin/driver_manager/v1/task.h"

class ResumeTask final : public Task {
 public:
  static fbl::RefPtr<ResumeTask> Create(fbl::RefPtr<Device> device, uint32_t target_system_state,
                                        Completion completion = nullptr);

  // Don't invoke this, use Create
  ResumeTask(fbl::RefPtr<Device> device, uint32_t target_system_state, Completion completion);

  uint32_t target_system_state() { return target_system_state_; }

  ~ResumeTask() final;

  const Device& device() const { return *device_; }

  fbl::String TaskDescription() const final;

 private:
  void Run() final;
  bool AddParentResumeTask();
  bool AddProxyResumeTask();

  // The device being resumeed
  fbl::RefPtr<Device> device_;
  // Target system resume state
  uint32_t target_system_state_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_RESUME_TASK_H_
