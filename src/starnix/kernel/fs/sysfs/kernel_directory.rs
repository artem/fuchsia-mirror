// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{create_bytes_file_with_handler, StaticDirectoryBuilder, StubEmptyFile},
};
use starnix_logging::track_stub;
use starnix_uapi::file_mode::mode;
use std::sync::Arc;

/// This directory contains various files and subdirectories that provide information about
/// the running kernel.
pub fn sysfs_kernel_directory(current_task: &CurrentTask, dir: &mut StaticDirectoryBuilder<'_>) {
    let kernel = current_task.kernel();
    dir.subdir(current_task, "kernel", 0o755, |dir| {
        dir.subdir(current_task, "tracing", 0o755, |_| ());
        dir.subdir(current_task, "mm", 0o755, |dir| {
            dir.subdir(current_task, "transparent_hugepage", 0o755, |dir| {
                dir.entry(
                    current_task,
                    "enabled",
                    StubEmptyFile::new_node("/sys/kernel/mm/transparent_hugepage/enabled"),
                    mode!(IFREG, 0o644),
                );
            });
        });
        dir.subdir(current_task, "wakeup_reasons", 0o755, |dir| {
            let read_only_file_mode = mode!(IFREG, 0o444);
            dir.entry(
                current_task,
                "last_resume_reason",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    kernel.power_manager.suspend_stats().last_resume_reason.unwrap_or_default()
                }),
                read_only_file_mode,
            );
            dir.entry(
                current_task,
                "last_suspend_time",
                create_bytes_file_with_handler(Arc::downgrade(kernel), |kernel| {
                    // It contains two numbers (in seconds) separated by space.
                    // First number is the time spent in suspend and resume processes.
                    // Second number is the time spent in sleep state.
                    track_stub!(
                        TODO("https://fxbug.dev/303507442"),
                        "/sys/kernel/wakeup_reasons/last_suspend_time"
                    );
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
