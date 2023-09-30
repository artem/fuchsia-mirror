// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_REGISTRY_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_REGISTRY_H_

#include <lib/driver/devicetree/manager/visitor.h>

#include <memory>

namespace fdf_devicetree {
class VisitorRegistry : public fdf_devicetree::Visitor {
 public:
  zx::result<> RegisterVisitor(std::unique_ptr<Visitor> visitor) {
    visitors_.push_back(std::move(visitor));
    return zx::ok();
  }

  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
    zx::result<> final_status = zx::ok();
    for (auto& visitor : visitors_) {
      auto status = visitor->Visit(node, decoder);
      if (status.is_error()) {
        final_status = zx::error(ZX_ERR_INTERNAL);
      }
    }
    return final_status;
  }

 private:
  std::vector<std::unique_ptr<Visitor>> visitors_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_REGISTRY_H_
