// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_METRICS_BUFFER_LOG_H_
#define SRC_LIB_METRICS_BUFFER_LOG_H_

#include <lib/ddk/debug.h>

#include <string_view>

#define VLOG_ENABLED 0

#if (VLOG_ENABLED)
#define VLOGF(format, ...) LOGF(format, ##__VA_ARGS__)
#else
#define VLOGF(...) \
  do {             \
  } while (0)
#endif

#define LOGF(format, ...)             \
  do {                                \
    LOG(INFO, format, ##__VA_ARGS__); \
  } while (0)

// Temporary solution for logging in driver and non-driver contexts by logging to stderr.
// TODO(b/299990391): Replace with syslog logging interface that accommodates both driver and
// non-driver contexts, when available.
#define LOG(severity, format, ...)                             \
  do {                                                         \
    static_assert(true || DDK_LOG_##severity);                 \
    if (DDK_LOG_##severity >= DDK_LOG_INFO) {                  \
      fprintf(stderr, "metrics_buffer" format, ##__VA_ARGS__); \
    }                                                          \
  } while (0)

#endif  // SRC_LIB_METRICS_BUFFER_LOG_H_
