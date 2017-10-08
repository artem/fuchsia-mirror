// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zx/channel.h>

#include <string>

#include "lib/fidl/cpp/bindings/interface_ptr.h"
#include "lib/fidl/cpp/bindings/interface_request.h"

namespace app {

// These helper functions help connect to environment services through the
// application's static environment. Multi-tenanted applications should connect
// via the appropriate ApplicationContext instance.

// These routines are safe to call from any thread.

// Connects to a service provided by the application's static environment,
// binding the service to a channel.
void ConnectToEnvironmentService(const std::string& interface_name,
                                 zx::channel channel);

// Connects to a service provided by the application's static environment,
// binding the service to an interface request.
template <typename Interface>
void ConnectToEnvironmentService(
    fidl::InterfaceRequest<Interface> request,
    const std::string& interface_name = Interface::Name_) {
  ConnectToEnvironmentService(interface_name, request.PassChannel());
}

// Connects to a service provided by the application's static environment,
// returning an interface pointer.
template <typename Interface>
fidl::InterfacePtr<Interface> ConnectToEnvironmentService(
    const std::string& interface_name = Interface::Name_) {
  fidl::InterfacePtr<Interface> interface_ptr;
  ConnectToEnvironmentService(interface_name,
                              interface_ptr.NewRequest().PassChannel());
  return interface_ptr;
}

namespace subtle {

// This returns creates a new channel connected to the application's static
// environment service provider.
zx::channel CreateStaticServiceRootHandle();

}  // namespace subtle

}  // namespace app
