// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{create_bytes_file_with_handler, StaticDirectoryBuilder},
    task::CurrentTask,
    types::file_mode::mode,
};

use std::sync::Arc;

/// This directory contains various files and subdirectories that provide information about
/// the running kernel.
pub fn sysfs_kernel_directory(current_task: &CurrentTask, dir: &mut StaticDirectoryBuilder<'_>) {
    let kernel = current_task.kernel();
    dir.subdir(current_task, b"kernel", 0o755, |dir| {
        dir.subdir(current_task, b"tracing", 0o755, |_| ());
        dir.subdir(current_task, b"wakeup_reasons", 0o755, |dir| {
            let read_only_file_mode = mode!(IFREG, 0o444);
            dir.entry(
                current_task,
                b"last_resume_reason",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    kernel.power_manager.suspend_stats().last_resume_reason.unwrap_or_default()
                }),
                read_only_file_mode,
            );
            dir.entry(
                current_task,
                b"last_suspend_time",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    // TODO(b/303507442): It contains two numbers (in seconds) separated by space.
                    // First number is the time spent in suspend and resume processes.
                    // Second number is the time spent in sleep state.
                    let suspend_time = kernel
                        .power_manager
                        .suspend_stats()
                        .last_suspend_time
                        .into_seconds()
                        .to_string();
                    format!("{} {}", suspend_time, suspend_time)
                }),
                read_only_file_mode,
            );
        });
    });
}
