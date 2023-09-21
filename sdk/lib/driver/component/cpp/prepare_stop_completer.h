// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_
#define LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_

#include <zircon/availability.h>

#if __Fuchsia_API_level__ >= 13

#if __Fuchsia_API_level__ >= 15
#include <lib/driver/component/cpp/start_completer.h>
#else
#include <lib/driver/symbols/symbols.h>
#include <lib/zx/result.h>
#endif

namespace fdf {

#if __Fuchsia_API_level__ >= 15
// This is the completer for the PrepareStop operation in |DriverBase|.
class PrepareStopCompleter : public Completer {
 public:
  using Completer::Completer;
  using Completer::operator();
};
#else
// This class wraps the completion of the PrepareStop driver lifecycle hook.
// The completer must be called before this class is destroyed. This is a move-only type.
class PrepareStopCompleter {
 public:
  explicit PrepareStopCompleter(PrepareStopCompleteCallback* complete, void* cookie)
      : complete_(complete), cookie_(cookie) {}

  PrepareStopCompleter(PrepareStopCompleter&& other) noexcept;

  PrepareStopCompleter(const PrepareStopCompleter&) = delete;
  PrepareStopCompleter& operator=(const PrepareStopCompleter&) = delete;

  ~PrepareStopCompleter();

  // Complete the PrepareStop async operation. Safe to call from any thread.
  void operator()(zx::result<> result);

 private:
  PrepareStopCompleteCallback* complete_;
  void* cookie_;
};
#endif

}  // namespace fdf

#endif

#endif  // LIB_DRIVER_COMPONENT_CPP_PREPARE_STOP_COMPLETER_H_
