// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <sys/resource.h>
#include <unistd.h>

#include <filesystem>
#include <fstream>
#include <iostream>
#include <thread>

using namespace std::chrono_literals;

void set_priority_or_panic(int new_nice) {
  if (setpriority(PRIO_PROCESS, 0, new_nice)) {
    std::cout << "failed to update nice: " << std::strerror(errno) << "\n";
    abort();
  }
}

void spawn_and_join_thread_with_nice(int child_nice) {
  std::thread child([child_nice]() { set_priority_or_panic(child_nice); });
  child.join();
}

int main(int argc, const char** argv) {
  std::filesystem::path parent_fence_path(argv[1]);
  std::filesystem::path child_fence_path(argv[2]);

  set_priority_or_panic(10);
  spawn_and_join_thread_with_nice(12);

  // TODO(b/297961833) test SCHED_RESET_ON_FORK
  pid_t child = fork();
  if (child > 0) {
    // parent process waits for child process to finish
    while (true) {
      if (std::filesystem::exists(child_fence_path)) {
        break;
      }
      std::this_thread::sleep_for(1s);
    }
    std::ofstream parent_fence(parent_fence_path);
    parent_fence << "done!";
  } else {
    // child process emits some scheduler calls and writes to its fence when done
    set_priority_or_panic(14);
    spawn_and_join_thread_with_nice(16);
    std::ofstream child_fence(child_fence_path);
    child_fence << "done!";
  }
}
