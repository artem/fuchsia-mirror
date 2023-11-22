// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_SYS_BIN_PSUTILS_RESOURCES_H_
#define SRC_SYS_BIN_PSUTILS_RESOURCES_H_

#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Returns a new handle to the info resource, which the caller
// is responsible for closing.
// See docs/objects/resource.md
zx_status_t get_info_resource(zx_handle_t* info_resource);

__END_CDECLS

#endif  // SRC_SYS_BIN_PSUTILS_RESOURCES_H_
