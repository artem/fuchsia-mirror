// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_SHARED_PLATFORM_H_
#define SRC_DEVELOPER_DEBUG_SHARED_PLATFORM_H_

#include <stdint.h>

namespace debug {

// Only append to this list, the values are encoded in the IPC protocol which has stability
// guarantees.
enum class Platform : uint32_t { kUnknown = 0, kFuchsia, kLinux, kMac };

// Returns the platform of the currently executing code. Note: for the zxdb frontend, this will tell
// you the platform that the zxdb frontend is running on, not the platform being debugged.
Platform CurrentSystemPlatform();

const char* PlatformToString(Platform p);

}  // namespace debug

#endif  // SRC_DEVELOPER_DEBUG_SHARED_PLATFORM_H_
