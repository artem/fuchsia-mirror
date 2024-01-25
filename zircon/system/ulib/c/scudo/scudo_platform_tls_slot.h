// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_SCUDO_SCUDO_PLATFORM_TLS_SLOT_H_
#define ZIRCON_SYSTEM_ULIB_C_SCUDO_SCUDO_PLATFORM_TLS_SLOT_H_

#include "threads_impl.h"

// SCUDO_HAS_PLATFORM_TLS_SLOT tells the Scudo sources to include this file
// and call this function instead of using a `thread_local` variable of its
// own.
//
// TODO(https://fxbug.dev/42142757): Our current combined libc/dynamic linker
// implementation does not allow libc itself to have any `thread_local`
// variables of its own.  In future, a different dynamic linker implementation
// will likely remove this restriction and having scudo use a (hidden
// visibility) `thread_local` variable will work fine.

static inline uintptr_t *getPlatformAllocatorTlsSlot() { return &__pthread_self()->scudo_tsd; }

#endif  // ZIRCON_SYSTEM_ULIB_C_SCUDO_SCUDO_PLATFORM_TLS_SLOT_H_
