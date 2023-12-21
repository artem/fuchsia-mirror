// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_SUSPEND_TASK_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_SUSPEND_TASK_H_

#include "src/devices/bin/driver_manager/v1/task.h"

class SuspendTask final : public Task {
 public:
  static fbl::RefPtr<SuspendTask> Create(fbl::RefPtr<Device> device, uint32_t flags,
                                         Completion completion = nullptr);

  // Don/t invoke this, use Create
  SuspendTask(fbl::RefPtr<Device> device, uint32_t flags, Completion completion);

  uint32_t suspend_flags() { return flags_; }

  ~SuspendTask() final;

  const Device& device() const { return *device_; }

  fbl::String TaskDescription() const final;

 private:
  void Run() final;

  // The device being suspended
  fbl::RefPtr<Device> device_;
  // The target suspend flags
  uint32_t flags_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_SUSPEND_TASK_H_
