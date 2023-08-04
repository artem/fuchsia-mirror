// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_STRUCTURED_BACKEND_FUCHSIA_SYSLOG_H_
#define LIB_SYSLOG_STRUCTURED_BACKEND_FUCHSIA_SYSLOG_H_

#include <assert.h>
#include <stdint.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

// REVIEWERS: DO NOT +2 any changes to this header
// file unless this number is incremented with each change.
#define FUCHSIA_LOG_API_VERSION (0)

typedef int8_t FuchsiaLogSeverity;

// Default log levels.
#define FUCHSIA_LOG_TRACE ((FuchsiaLogSeverity)0x10)
#define FUCHSIA_LOG_DEBUG ((FuchsiaLogSeverity)0x20)
#define FUCHSIA_LOG_INFO ((FuchsiaLogSeverity)0x30)
#define FUCHSIA_LOG_WARNING ((FuchsiaLogSeverity)0x40)
#define FUCHSIA_LOG_ERROR ((FuchsiaLogSeverity)0x50)
#define FUCHSIA_LOG_FATAL ((FuchsiaLogSeverity)0x60)

#define FUCHSIA_LOG_NONE ((FuchsiaLogSeverity)0x7F)

#define FUCHSIA_LOG_SEVERITY_STEP_SIZE ((uint8_t)0x10)
#define FUCHSIA_LOG_VERBOSITY_STEP_SIZE ((uint8_t)0x1)

// Assert that log levels are in ascending order.
// Numeric comparison is generally used to determine whether to log.
static_assert(FUCHSIA_LOG_TRACE < FUCHSIA_LOG_DEBUG, "");
static_assert(FUCHSIA_LOG_DEBUG < FUCHSIA_LOG_INFO, "");
static_assert(FUCHSIA_LOG_INFO < FUCHSIA_LOG_WARNING, "");
static_assert(FUCHSIA_LOG_WARNING < FUCHSIA_LOG_ERROR, "");
static_assert(FUCHSIA_LOG_ERROR < FUCHSIA_LOG_FATAL, "");
static_assert(FUCHSIA_LOG_FATAL < FUCHSIA_LOG_NONE, "");

// Max size of log buffer
#define FUCHSIA_SYSLOG_BUFFER_SIZE ((1 << 15) / 8)

// Additional storage for internal log state.
#define FUCHSIA_SYSLOG_STATE_SIZE (15)

__END_CDECLS

#endif  // LIB_SYSLOG_STRUCTURED_BACKEND_FUCHSIA_SYSLOG_H_
