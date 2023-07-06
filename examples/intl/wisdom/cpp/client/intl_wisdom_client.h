// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_INTL_WISDOM_CPP_CLIENT_INTL_WISDOM_CLIENT_H_
#define EXAMPLES_INTL_WISDOM_CPP_CLIENT_INTL_WISDOM_CLIENT_H_

#include <lib/sys/cpp/component_context.h>

#include "examples/intl/wisdom/cpp/client/icu_headers.h"
#include "fuchsia/examples/intl/wisdom/cpp/fidl.h"

namespace intl_wisdom {

// A client class for communicating with an |IntlWisdomServer|.
//
// Call |Start| to request that a server be started. Then call |SendRequest| to
// ask the server for a wisdom string.
class IntlWisdomClient {
 public:
  IntlWisdomClient(std::unique_ptr<sys::ComponentContext> startup_context);

  const fuchsia::examples::intl::wisdom::IntlWisdomServerPtr& server() const { return server_; }

  // Asks the startup context to connect to the wisdom server.
  void Start();
  // Sends a request for "wisdom" with given |timestamp| argument. The response,
  // if any, is provided via the |callback|.
  //
  // Params:
  //   timestamp: used for seeding the server's response
  //   time_zone: used in generating a |fuchsia::intl::Profile| for the request
  //   callback: async callback
  void SendRequest(
      zx::time timestamp, const icu::TimeZone& time_zone,
      fuchsia::examples::intl::wisdom::IntlWisdomServer::AskForWisdomCallback callback) const;

 private:
  IntlWisdomClient(const IntlWisdomClient&) = delete;
  IntlWisdomClient& operator=(const IntlWisdomClient&) = delete;

  std::unique_ptr<sys::ComponentContext> startup_context_;
  fuchsia::examples::intl::wisdom::IntlWisdomServerPtr server_;
};

}  // namespace intl_wisdom

#endif  // EXAMPLES_INTL_WISDOM_CPP_CLIENT_INTL_WISDOM_CLIENT_H_
