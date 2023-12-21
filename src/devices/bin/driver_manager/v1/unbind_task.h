// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_UNBIND_TASK_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_UNBIND_TASK_H_

#include "src/devices/bin/driver_manager/v1/task.h"

// This is not nested so we can forward declare it in device.h.
struct UnbindTaskOpts {
  // Whether to call the unbind hook.
  bool do_unbind;
  // Whether to immediately post this task to the async dispatcher.
  bool post_on_create;
  // Whether the driver_host (i.e. not the devcoordinator) called |ScheduleRemove| on the device.
  bool driver_host_requested;
};

// This is used for sending |Unbind| requests, and scheduling additional unbind and remove
// tasks.
// For compatibility with the current device lifecycle model, unbind is not invoked
// on the device that |ScheduleRemove| was called on.
class UnbindTask final : public Task {
 public:
  static fbl::RefPtr<UnbindTask> Create(fbl::RefPtr<Device> device, UnbindTaskOpts opts,
                                        Completion completion = nullptr);

  // Don't invoke this, use Create
  UnbindTask(fbl::RefPtr<Device> device, UnbindTaskOpts opts, Completion completion);

  ~UnbindTask() final;

  void set_do_unbind(bool do_unbind) { do_unbind_ = do_unbind; }

  bool driver_host_requested() const { return driver_host_requested_; }

  fbl::String TaskDescription() const final;

 private:
  void ScheduleUnbindChildren();
  void Run() final;

  // The device being unbound.
  fbl::RefPtr<Device> device_;
  // If true, |Unbind| will be sent to the driver_host.
  bool do_unbind_;
  // True if this task is for the device that had |ScheduleRemove| called on it by a driver_host,
  // false otherwise.
  bool driver_host_requested_;
};

// This is used for sending |CompleteRemoval| requests.
class RemoveTask final : public Task {
 public:
  static fbl::RefPtr<RemoveTask> Create(fbl::RefPtr<Device> device,
                                        Completion completion = nullptr);

  // Don't invoke this, use Create
  RemoveTask(fbl::RefPtr<Device> device, Completion completion);

  ~RemoveTask() final;

  fbl::String TaskDescription() const final;

 protected:
  friend class UnbindTask;

 private:
  void Run() final;

  // The device being removed.
  fbl::RefPtr<Device> device_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_UNBIND_TASK_H_
