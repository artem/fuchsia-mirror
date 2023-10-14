// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_REGISTRATION_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_REGISTRATION_H_

#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/driver/logging/cpp/logger.h>

#include <memory>

// The |VisitorRegistration| is the ABI for visitors to expose themselves to
// the Devicetree Visitor Framework. The visitor is loaded in as a shared
// library (also referred to as a DSO), and the global symbol
// `__devicetree_visitor_registration__` is used by the Devicetree Visitor
// Framework to locate this VisitorRegistration in the visitor library. The
// framework will use this to instantiate a visitor object to parse the
// devicetree.
struct VisitorRegistration {
  // The version is used in case we evolve the initialization signature
  // for any reason.
  uint64_t version;

  struct v1 {
    // Return a unique pointer to the visitor object or empty object in case of
    // error.
    // TODO(fxbug.dev/134108) : Temporarily using C++ ABI. Move to C ABI once
    // visitor interface is stabilized.
    std::unique_ptr<fdf_devicetree::Visitor> (*create_visitor)(fdf::Logger* logger);
  } v1;
};

#define VISITOR_REGISTRATION_VERSION_1 1

#define VISITOR_REGISTRATION_VERSION_MAX VISITOR_REGISTRATION_VERSION_1

#define DEVICETREE_VISITOR_REGISTRATION_V1(create_visitor) \
  { .version = VISITOR_REGISTRATION_VERSION_1, .v1 = {create_visitor}, }

#define EXPORT_DEVICETREE_VISITOR_REGISTRATION_V1(create_visitor)                   \
  extern "C" const VisitorRegistration __devicetree_visitor_registration__ __EXPORT \
  DEVICETREE_VISITOR_REGISTRATION_V1(create_visitor)

#define REGISTER_DEVICETREE_VISITOR(visitor_class)                          \
  EXPORT_DEVICETREE_VISITOR_REGISTRATION_V1(                                \
      [](fdf::Logger* logger) -> std::unique_ptr<fdf_devicetree::Visitor> { \
        fdf::Logger::SetGlobalInstance(logger);                             \
        return std::make_unique<visitor_class>();                           \
      })

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_REGISTRATION_H_
