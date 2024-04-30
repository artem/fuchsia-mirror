// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Code generated by "measure-tape"; DO NOT EDIT.
//
// See tools/fidl/measure-tape/README.md

// clang-format off
#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_MEASURE_TAPE_HLCPP_MEASURE_TAPE_FOR_READ_BY_TYPE_RESULT_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_MEASURE_TAPE_HLCPP_MEASURE_TAPE_FOR_READ_BY_TYPE_RESULT_H_

#include <fuchsia/bluetooth/gatt2/cpp/fidl.h>


namespace measure_tape {
namespace fuchsia {
namespace bluetooth {
namespace gatt2 {

struct Size {
  explicit Size(int64_t num_bytes, int64_t num_handles)
    : num_bytes(num_bytes), num_handles(num_handles) {}

  const int64_t num_bytes;
  const int64_t num_handles;
};


// Helper function to measure ::fuchsia::bluetooth::gatt2::ReadByTypeResult.
//
// In most cases, the size returned is a precise size. Otherwise, the size
// returned is a safe upper-bound.
Size Measure(const ::fuchsia::bluetooth::gatt2::ReadByTypeResult& value);



}  // gatt2
}  // bluetooth
}  // fuchsia
}  // measure_tape

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_MEASURE_TAPE_HLCPP_MEASURE_TAPE_FOR_READ_BY_TYPE_RESULT_H_

