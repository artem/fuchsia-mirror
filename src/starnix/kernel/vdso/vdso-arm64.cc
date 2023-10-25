// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/syscall.h>
#include <sys/time.h>
#include <zircon/compiler.h>

#include "vdso-common.h"
#include "vdso-platform.h"

int syscall(intptr_t syscall_number, intptr_t arg1, intptr_t arg2, intptr_t arg3) {
  register intptr_t x0 asm("x0") = arg1;
  register intptr_t x1 asm("x1") = arg2;
  register intptr_t x2 asm("x2") = arg3;
  register intptr_t number asm("x8") = syscall_number;

  __asm__ volatile("svc #0" : "=r"(x0) : "0"(x0), "r"(x1), "r"(x2), "r"(number) : "memory");
  return static_cast<int>(x0);
}

extern "C" __EXPORT __attribute__((naked)) void __kernel_rt_sigreturn() {
  __asm__ volatile("mov x8, %0" ::"I"(__NR_rt_sigreturn));
  __asm__ volatile("svc #0");
}

extern "C" __EXPORT int __kernel_clock_gettime(int clock_id, struct timespec* tp) {
  return clock_gettime_impl(clock_id, tp);
}

extern "C" __EXPORT int __kernel_clock_getres(int clock_id, struct timespec* tp) {
  return clock_getres_impl(clock_id, tp);
}

extern "C" __EXPORT int __kernel_gettimeofday(struct timeval* tv, struct timezone* tz) {
  int ret =
      syscall(__NR_gettimeofday, reinterpret_cast<intptr_t>(tv), reinterpret_cast<intptr_t>(tz), 0);

  return ret;
}
