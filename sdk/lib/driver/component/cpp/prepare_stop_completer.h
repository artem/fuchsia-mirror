// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_
#define LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_

#include <zircon/availability.h>

#if __Fuchsia_API_level__ >= 15
#include <lib/driver/component/cpp/start_completer.h>

namespace fdf {

// This is the completer for the PrepareStop operation in |DriverBase|.
class PrepareStopCompleter : public Completer {
 public:
  using Completer::Completer;
  using Completer::operator();
};

}  // namespace fdf

#endif

#endif  // LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_
