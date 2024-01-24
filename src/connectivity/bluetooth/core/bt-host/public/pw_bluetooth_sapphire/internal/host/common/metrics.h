// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_COMMON_METRICS_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_COMMON_METRICS_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/inspect.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/macros.h"

namespace bt {

// Represents a metric counter.
template <typename property_t>
class MetricCounter {
 public:
  MetricCounter() = default;
  virtual ~MetricCounter() = default;

  // Attach peer inspect node as a child node of |parent|.
  virtual void AttachInspect(inspect::Node& parent, const std::string& name);

  // Increment the metrics counter by |value|.
  void Add(int value = 1) { inspect_property_.Add(value); }

  // Decrement the metrics counter by |value|.
  void Subtract(int value = 1) { inspect_property_.Subtract(value); }

 private:
  // Node property
  property_t inspect_property_;

  // Update underlying inspect attributes.
  void UpdateInspect();

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(MetricCounter);
};

using IntMetricCounter = MetricCounter<inspect::IntProperty>;
using UintMetricCounter = MetricCounter<inspect::UintProperty>;

}  // namespace bt

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_COMMON_METRICS_H_
