// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/fidl/cpp/clone.h"

namespace fidl {

zx_status_t Clone(const zx::channel& value, zx::channel* result) {
  if (!value) {
    result->reset();
    return ZX_OK;
  }
  return ZX_ERR_ACCESS_DENIED;
}

zx_status_t Clone(const StringPtr& value, StringPtr* result) {
  if (!value) {
    *result = StringPtr();
  } else {
    result->reset(*value);
  }
  return ZX_OK;
}

}  // namespace fidl
