// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_ZIRCON_H_
#define LIB_LD_ZIRCON_H_

#include <lib/elfldltl/vmar-loader.h>
#include <lib/stdcompat/functional.h>
#include <lib/zx/channel.h>
#include <lib/zx/debuglog.h>
#include <lib/zx/socket.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include <string_view>

#include "startup-diagnostics.h"
#include "startup-load.h"

namespace ld {

using StartupModule = StartupLoadModule<elfldltl::LocalVmarLoader>;

// This collects the data from the bootstrap channel.
struct StartupData {
  int Log(std::string_view str);

  auto LogClosure() { return cpp20::bind_front(&StartupData::Log, this); }

  zx::vmo GetLibraryVmo(Diagnostics& diag, std::string_view name);

  zx::debuglog debuglog;
  zx::socket log_socket;

  zx::vmar vmar;       // VMAR for allocation and module-loading.
  zx::vmar self_vmar;  // VMAR for the dynamic linker load image.

  zx::vmo executable_vmo;

  zx::channel ldsvc;

  bool ld_debug = false;
};

StartupData ReadBootstrap(zx::unowned_channel bootstrap);

}  // namespace ld

#endif  // LIB_LD_ZIRCON_H_
