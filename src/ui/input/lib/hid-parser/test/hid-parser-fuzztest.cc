// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/hid-parser/parser.h>
#include <stddef.h>

// fuzz_target.cc
extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  hid::DeviceDescriptor* dd = nullptr;
  auto result = hid::ParseReportDescriptor(data, size, &dd);
  if (result != hid::ParseResult::kParseOk) {
    return 0;
  }
  auto cleanup = fit::defer([dd]() { hid::FreeDeviceDescriptor(dd); });
  return 0;
}
