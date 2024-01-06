// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_LIB_CODEC_IMPL_INCLUDE_LIB_MEDIA_CODEC_IMPL_LOG_H_
#define SRC_MEDIA_LIB_CODEC_IMPL_INCLUDE_LIB_MEDIA_CODEC_IMPL_LOG_H_

#include <lib/ddk/debug.h>
#include <lib/syslog/global.h>
#include <lib/syslog/logger.h>

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
#define LOG(severity, format, ...)                            \
  do {                                                        \
    static_assert(true || DDK_LOG_##severity);                \
    static_assert(true || FX_LOG_##severity);                 \
    if (DDK_LOG_##severity > DDK_LOG_INFO) {                  \
      fprintf(stderr, "[codec_impl] " format, ##__VA_ARGS__); \
    }                                                         \
  } while (0)

namespace codec_impl {
namespace internal {

constexpr std::string_view BaseName(std::string_view path) {
  size_t pos = path.find_last_of('/') + 1;
  return (pos < path.size()) ? path.substr(pos) : path;
}

}  // namespace internal
}  // namespace codec_impl

#endif  // SRC_MEDIA_LIB_CODEC_IMPL_INCLUDE_LIB_MEDIA_CODEC_IMPL_LOG_H_
