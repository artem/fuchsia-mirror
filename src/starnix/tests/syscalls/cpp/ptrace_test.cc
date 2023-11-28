// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <sys/ptrace.h>
#include <time.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

constexpr int kOriginalSigno = SIGUSR1;
constexpr int kInjectedSigno = SIGUSR2;
constexpr int kInjectedErrno = EIO;

TEST(PtraceTest, SetSigInfo) {
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([] {
    struct sigaction sa = {};
    sa.sa_sigaction = +[](int sig, siginfo_t* info, void* ucontext) {
      if (sig != kInjectedSigno) {
        _exit(1);
      }
      if (info->si_errno != kInjectedErrno) {
        _exit(2);
      }
      _exit(0);
    };

    sa.sa_flags = SA_SIGINFO | SA_RESTART;
    EXPECT_EQ(sigemptyset(&sa.sa_mask), 0);
    sigaction(kInjectedSigno, &sa, nullptr);
    sigaction(kOriginalSigno, &sa, nullptr);

    EXPECT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(kOriginalSigno);
    _exit(3);
  });

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == kOriginalSigno) << " status " << status;

  siginfo_t siginfo = {};
  ASSERT_EQ(ptrace(PTRACE_GETSIGINFO, child_pid, 0, &siginfo), 0)
      << "ptrace failed with error " << strerror(errno);
  EXPECT_EQ(kOriginalSigno, siginfo.si_signo);
  EXPECT_EQ(SI_TKILL, siginfo.si_code);

  // Replace the signal with kInjectedSigno, and check that the child exits
  // with kInjectedSigno, indicating that signal injection was successful.
  siginfo.si_signo = kInjectedSigno;
  siginfo.si_errno = kInjectedErrno;
  ASSERT_EQ(ptrace(PTRACE_SETSIGINFO, child_pid, 0, &siginfo), 0);
  ASSERT_EQ(ptrace(PTRACE_DETACH, child_pid, 0, kInjectedSigno), 0);
}

#ifndef PTRACE_EVENT_STOP  // Not defined in every libc
#define PTRACE_EVENT_STOP 128
#endif

TEST(PtraceTest, InterruptAfterListen) {
  volatile int child_should_spin = 1;
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([&child_should_spin] {
    const struct timespec req = {.tv_sec = 0, .tv_nsec = 1000};
    while (child_should_spin) {
      nanosleep(&req, nullptr);
    }
    _exit(0);
  });

  // In parent process.
  ASSERT_NE(child_pid, 0);

  ASSERT_EQ(ptrace(PTRACE_SEIZE, child_pid, 0, 0), 0);
  int status;
  EXPECT_EQ(waitpid(child_pid, &status, WNOHANG), 0);

  // Stop the child with PTRACE_INTERRUPT.
  ASSERT_EQ(ptrace(PTRACE_INTERRUPT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_EQ(SIGTRAP | (PTRACE_EVENT_STOP << 8), status >> 8);

  ASSERT_EQ(ptrace(PTRACE_POKEDATA, child_pid, &child_should_spin, 0), 0) << strerror(errno);

  // Send SIGSTOP to the child, then resume it, allowing it to proceed to
  // signal-delivery-stop.
  ASSERT_EQ(kill(child_pid, SIGSTOP), 0);
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  // Move out of signal-delivery-stop and deliver the SIGSTOP.
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, SIGSTOP), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_EQ(SIGSTOP | (PTRACE_EVENT_STOP << 8), status >> 8);

  // Restart the child, but don't let it execute. Child continues to deliver
  // notifications of when it gets stop / continue signals.  This allows a
  // normal SIGCONT signal to be sent to a child to restart it, rather than
  // having the tracer restart it.  The tracer can then detect the SIGCONT.
  ASSERT_EQ(ptrace(PTRACE_LISTEN, child_pid, 0, 0), 0);

  // "If the tracee was already stopped by a signal and PTRACE_LISTEN was sent
  // to it, the tracee stops with PTRACE_EVENT_STOP and WSTOPSIG(status) returns
  // the stop signal."
  ASSERT_EQ(ptrace(PTRACE_INTERRUPT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_EQ(SIGSTOP | (PTRACE_EVENT_STOP << 8), status >> 8);

  // Allow the tracer to proceed normally.
  EXPECT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0) << strerror(errno);
}
