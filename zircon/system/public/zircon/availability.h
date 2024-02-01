// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_AVAILABILITY_H_
#define ZIRCON_AVAILABILITY_H_

// The value of __Fuchsia_API_level__ when the target API level is HEAD.
// While Fuchsia API levels are unsigned 64-bit integers, Clang only supports
// 32-bit version segments, so we use the special value of `UINT32_MAX` to
// represent builds targeting HEAD/LEGACY.
// Note that while the FIDL definition of `HEAD` is one less than the largest
// unsigned 64-bit value (that is, the equivalent of `UINT64_MAX - 1`), this is
// the largest possible unsigned 32-bit value (`UINT32_MAX`).
// TODO(https://fxbug.dev/321269965): Resolve this FIDL-Clang discrepancy.
#define FUCHSIA_HEAD 4294967295

#if defined(__Fuchsia_API_level__) && defined(__clang__)

// An API that was added to the platform.
//
// Annotates the API level at which the API was added to the platform. Use
// ZX_DEPRECATED_SINCE if the API is later deprecated.
//
// Example:
//
//   void fdio_spawn(...) ZX_AVAILABLE_SINCE(4);
//
#define ZX_AVAILABLE_SINCE(level_added) \
  __attribute__((availability(fuchsia, strict, introduced = level_added)))

// An API that was added the platform and later deprecated.
//
// Annotates the API level at which the API added the platform and the API
// level at which the API was deprecated.
//
// Deprecated API can still be called by clients. The deprecation annotation
// is a warning that the API is likely to be removed in the future. APIs should
// be deprecated for at least one API level before being removed.
//
// Use the `msg` parameter to explain why the API was deprecated and what
// clients should do instead of using the API.
//
// Example:
//
//   void fdio_fork(...) ZX_DEPRECATED_SINCE(1, 4,
//       "Root cause of security vulnerabilities due to implicit handle "
//       "transfer. Use fdio_spawn instead.");
//
#define ZX_DEPRECATED_SINCE(level_added, level_deprecated, msg)          \
  __attribute__((availability(fuchsia, strict, introduced = level_added, \
                              deprecated = level_deprecated, message = msg)))

// An API that was added to the platform and later removed.
//
// Annotates the API level at which the API added the platform, the API
// level at which the API was deprecated, and the API level at which the API
// was removed.
//
// Clients can no longer call APIs if they are compiled to target an API
// level at, or beyond, the level at which the API was removed. APIs should be
// deprecated for at least one API level before being removed.
//
// Example:
//
//   void fdio_fork(...) ZX_REMOVED_SINCE(1, 4, 8,
//       "Root cause of security vulnerabilities due to implicit handle "
//       "transfer. Use fdio_spawn instead.");
//
#define ZX_REMOVED_SINCE(level_added, level_deprecated, level_removed, msg)             \
  __attribute__((availability(fuchsia, strict, introduced = level_added,                \
                              deprecated = level_deprecated, obsoleted = level_removed, \
                              message = msg)))

#else  // __Fuchsia_API_level__

#define ZX_AVAILABLE_SINCE(level_added)
#define ZX_DEPRECATED_SINCE(level_added, level_deprecated, msg)
#define ZX_REMOVED_SINCE(level_added, level_deprecated, level_removed, msg)

#endif  // __Fuchsia_API_level__

#endif  // ZIRCON_AVAILABILITY_H_
