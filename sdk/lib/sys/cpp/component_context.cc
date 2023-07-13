// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fdio/directory.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/outgoing_directory.h>
#include <lib/zx/channel.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

namespace sys {

ComponentContext::ComponentContext(std::shared_ptr<ServiceDirectory> svc,
                                   async_dispatcher_t* dispatcher)
    : svc_(std::move(svc)), outgoing_(std::make_shared<OutgoingDirectory>()) {}

ComponentContext::ComponentContext(std::shared_ptr<ServiceDirectory> svc,
                                   fidl::InterfaceRequest<fuchsia::io::Directory> directory_request,
                                   async_dispatcher_t* dispatcher)
    : ComponentContext(svc, dispatcher) {
  outgoing_->Serve(std::move(directory_request), dispatcher);
}

ComponentContext::~ComponentContext() = default;

std::unique_ptr<ComponentContext> ComponentContext::Create() {
  return std::make_unique<ComponentContext>(ServiceDirectory::CreateFromNamespace());
}

std::unique_ptr<ComponentContext> ComponentContext::CreateAndServeOutgoingDirectory() {
  auto component_context = ComponentContext::Create();
  component_context->outgoing()->ServeFromStartupInfo();
  return component_context;
}

}  // namespace sys
