// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#include <dirent.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <gtest/gtest.h>
#include <linux/capability.h>

#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/string_number_conversions.h"

namespace test_helper {

::testing::AssertionResult ForkHelper::WaitForChildrenInternal(int death_signum) {
  ::testing::AssertionResult result = ::testing::AssertionSuccess();
  while (wait_for_all_children_ || !child_pids_.empty()) {
    int wstatus;
    pid_t pid;
    if ((pid = wait(&wstatus)) == -1) {
      if (errno == EINTR) {
        continue;
      }
      if (errno == ECHILD) {
        // No more children, reaping is done.
        return result;
      }
      // Another error is unexpected.
      result = ::testing::AssertionFailure()
               << "wait error: " << strerror(errno) << "(" << errno << ")";
    }
    bool check_result = wait_for_all_children_;
    if (!check_result) {
      auto it = std::find(child_pids_.begin(), child_pids_.end(), pid);
      if (it != child_pids_.end()) {
        child_pids_.erase(it);
        check_result = true;
      }
    }

    if (check_result) {
      if (death_signum == 0) {
        if (!WIFEXITED(wstatus) || WEXITSTATUS(wstatus) != 0) {
          result = ::testing::AssertionFailure()
                   << "wait_status: WIFEXITED(wstatus) = " << WIFEXITED(wstatus)
                   << ", WEXITSTATUS(wstatus) = " << WEXITSTATUS(wstatus)
                   << ", WTERMSIG(wstatus) = " << WTERMSIG(wstatus);
        }
      } else {
        if (!WIFSIGNALED(wstatus) || WTERMSIG(wstatus) != death_signum) {
          result = ::testing::AssertionFailure()
                   << "wait_status: WIFSIGNALED(wstatus) = " << WIFSIGNALED(wstatus)
                   << ", WEXITSTATUS(wstatus) = " << WEXITSTATUS(wstatus)
                   << ", WTERMSIG(wstatus) = " << WTERMSIG(wstatus);
        }
      }
    }
  }
  return result;
}

ForkHelper::ForkHelper() : wait_for_all_children_(true), death_signum_(0) {
  // Ensure that all children will ends up being parented to the process that
  // created the helper.
  prctl(PR_SET_CHILD_SUBREAPER, 1);
}

ForkHelper::~ForkHelper() {
  // Wait for all remaining children, and ensure none failed.
  EXPECT_TRUE(WaitForChildrenInternal(death_signum_)) << ": at least a child had a failure";
}

void ForkHelper::OnlyWaitForForkedChildren() { wait_for_all_children_ = false; }

void ForkHelper::ExpectSignal(int signum) { death_signum_ = signum; }

testing::AssertionResult ForkHelper::WaitForChildren() {
  return WaitForChildrenInternal(death_signum_);
}

pid_t ForkHelper::RunInForkedProcess(std::function<void()> action) {
  pid_t pid = SAFE_SYSCALL(fork());
  if (pid != 0) {
    child_pids_.push_back(pid);
    return pid;
  }
  action();
  _exit(testing::Test::HasFailure());
}

CloneHelper::CloneHelper() {
  // Stack setup
  this->_childStack = (uint8_t *)mmap(NULL, CloneHelper::_childStackSize, PROT_WRITE | PROT_READ,
                                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  assert(errno == 0);
  assert(this->_childStack != (uint8_t *)(-1));
  this->_childStackBegin = this->_childStack + CloneHelper::_childStackSize;
}

CloneHelper::~CloneHelper() { munmap(this->_childStack, CloneHelper::_childStackSize); }

int CloneHelper::runInClonedChild(unsigned int cloneFlags, int (*childFunction)(void *)) {
  int childPid = clone(childFunction, this->_childStackBegin, cloneFlags, NULL);
  assert(errno == 0);
  assert(childPid != -1);
  return childPid;
}

int CloneHelper::sleep_1sec(void *) {
  struct timespec res;
  res.tv_sec = 1;
  res.tv_nsec = 0;
  clock_nanosleep(CLOCK_MONOTONIC, 0, &res, &res);
  return 0;
}

int CloneHelper::doNothing(void *) { return 0; }

void SignalMaskHelper::blockSignal(int signal) {
  sigemptyset(&this->_sigset);
  sigaddset(&this->_sigset, signal);
  sigprocmask(SIG_BLOCK, &this->_sigset, &this->_sigmaskCopy);
}

void SignalMaskHelper::waitForSignal(int signal) {
  int sig;
  int result = sigwait(&this->_sigset, &sig);
  ASSERT_EQ(result, 0);
  ASSERT_EQ(sig, signal);
}

void SignalMaskHelper::restoreSigmask() { sigprocmask(SIG_SETMASK, &this->_sigmaskCopy, NULL); }

ScopedTempFD::ScopedTempFD() : name_("/tmp/proc_test_file_XXXXXX") {
  char *mut_name = const_cast<char *>(name_.c_str());
  fd_ = ScopedFD(mkstemp(mut_name));
}

ScopedTempDir::ScopedTempDir() {
  path_ = get_tmp_path() + "/testdirXXXXXX";
  if (!mkdtemp(path_.data())) {
    path_.clear();
  }
}

ScopedTempDir::~ScopedTempDir() {
  if (!path_.empty()) {
    RecursiveUnmountAndRemove(path_);
  }
}

void waitForChildSucceeds(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                          int (*parentRunFunction)(void *)) {
  CloneHelper cloneHelper;
  int expectedWaitPid = cloneHelper.runInClonedChild(cloneFlags, childRunFunction);

  parentRunFunction(NULL);

  int expectedWaitStatus = 0;
  int expectedErrno = 0;
  int actualWaitStatus;
  int actualWaitPid = waitpid(expectedWaitPid, &actualWaitStatus, waitFlag);
  EXPECT_EQ(actualWaitPid, expectedWaitPid);
  EXPECT_EQ(actualWaitStatus, expectedWaitStatus);
  EXPECT_EQ(errno, expectedErrno);
}

void waitForChildFails(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                       int (*parentRunFunction)(void *)) {
  CloneHelper cloneHelper;
  int pid = cloneHelper.runInClonedChild(cloneFlags, childRunFunction);

  parentRunFunction(NULL);

  int expectedWaitPid = -1;
  int actualWaitPid = waitpid(pid, NULL, waitFlag);
  EXPECT_EQ(actualWaitPid, expectedWaitPid);
  EXPECT_EQ(errno, ECHILD);
  errno = 0;
}

std::string get_tmp_path() {
  static std::string tmp_path = [] {
    const char *tmp = getenv("TEST_TMPDIR");
    if (tmp == nullptr)
      tmp = "/tmp";
    return tmp;
  }();
  return tmp_path;
}

std::optional<MemoryMapping> find_memory_mapping(std::function<bool(const MemoryMapping &)> match,
                                                 std::string_view maps) {
  std::vector<std::string_view> lines =
      fxl::SplitString(maps, "\n", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  // format:
  // start-end perms offset device inode path
  for (auto line : lines) {
    std::vector<std::string_view> parts =
        fxl::SplitString(line, " ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    if (parts.size() < 5) {
      return std::nullopt;
    }
    std::vector<std::string_view> addrs =
        fxl::SplitString(parts[0], "-", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    if (addrs.size() != 2) {
      return std::nullopt;
    }

    uintptr_t start;
    uintptr_t end;

    if (!fxl::StringToNumberWithError(addrs[0], &start, fxl::Base::k16) ||
        !fxl::StringToNumberWithError(addrs[1], &end, fxl::Base::k16)) {
      return std::nullopt;
    }

    size_t offset;
    size_t inode;
    if (!fxl::StringToNumberWithError(parts[2], &offset, fxl::Base::k16) ||
        !fxl::StringToNumberWithError(parts[4], &inode, fxl::Base::k10)) {
      return std::nullopt;
    }

    std::string pathname;
    if (parts.size() > 5) {
      // The pathname always starts at pos 73.
      pathname = line.substr(73);
    }

    MemoryMapping mapping = {
        start, end, std::string(parts[1]), offset, std::string(parts[3]), inode, pathname,
    };

    if (match(mapping)) {
      return mapping;
    }
  }
  return std::nullopt;
}

std::optional<MemoryMapping> find_memory_mapping(uintptr_t addr, std::string_view maps) {
  return find_memory_mapping(
      [addr](const MemoryMapping &mapping) { return mapping.start <= addr && addr < mapping.end; },
      maps);
}

bool HasCapability(uint32_t cap) {
  struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
  struct __user_cap_data_struct caps[_LINUX_CAPABILITY_U32S_3] = {};
  syscall(__NR_capget, &header, &caps);

  return (caps[CAP_TO_INDEX(cap)].effective & CAP_TO_MASK(cap)) != 0;
}

bool HasSysAdmin() { return HasCapability(CAP_SYS_ADMIN); }

bool IsStarnix() {
  struct utsname buf;
  return uname(&buf) == 0 && strstr(buf.release, "starnix") != nullptr;
}

void RecursiveUnmountAndRemove(const std::string &path) {
  if (HasSysAdmin()) {
    // Repeatedly call umount to handle shadowed mounts properly.
    do {
      errno = 0;
      ASSERT_THAT(umount(path.c_str()), AnyOf(SyscallSucceeds(), SyscallFailsWithErrno(EINVAL)))
          << path;
    } while (errno != EINVAL);
  }

  int dir_fd = open(path.c_str(), O_DIRECTORY | O_NOFOLLOW);
  if (dir_fd >= 0) {
    DIR *dir = fdopendir(dir_fd);
    EXPECT_NE(dir, nullptr) << "fdopendir: " << std::strerror(errno);
    while (struct dirent *entry = readdir(dir)) {
      std::string name(entry->d_name);
      if (name == "." || name == "..")
        continue;
      std::string subpath = std::string(path) + "/" + name;
      if (entry->d_type == DT_DIR) {
        RecursiveUnmountAndRemove(subpath);
      } else {
        EXPECT_THAT(unlink(subpath.c_str()), SyscallSucceeds()) << subpath;
      }
    }
    EXPECT_EQ(closedir(dir), 0) << "closedir: " << std::strerror(errno);
  }

  EXPECT_THAT(rmdir(path.c_str()), SyscallSucceeds());
}

int MemFdCreate(const char *name, unsigned int flags) {
  return static_cast<int>(syscall(SYS_memfd_create, name, flags));
}

// Attempts to read a byte from the given memory address.
// Returns whether the read succeeded or not.
bool TryRead(uintptr_t addr) {
  test_helper::ScopedFD mem_fd(MemFdCreate("try_read", O_WRONLY));
  EXPECT_TRUE(mem_fd.is_valid());

  return write(mem_fd.get(), reinterpret_cast<void *>(addr), 1) == 1;
}

// Attempts to write a zero byte to the given memory address.
// Returns whether the write succeeded or not.
bool TryWrite(uintptr_t addr) {
  test_helper::ScopedFD zero_fd(open("/dev/zero", O_RDONLY));
  EXPECT_TRUE(zero_fd.is_valid());

  return read(zero_fd.get(), reinterpret_cast<void *>(addr), 1) == 1;
}

}  // namespace test_helper
