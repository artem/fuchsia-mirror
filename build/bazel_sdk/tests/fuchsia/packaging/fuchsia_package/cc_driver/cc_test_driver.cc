// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "cc_test_driver.h"

#include <lib/driver/component/cpp/driver_export.h>

namespace cc_test_driver {

zx::result<> CCTestDriver::Start() { return zx::ok(); }

}  // namespace cc_test_driver

FUCHSIA_DRIVER_EXPORT(cc_test_driver::CCTestDriver);
