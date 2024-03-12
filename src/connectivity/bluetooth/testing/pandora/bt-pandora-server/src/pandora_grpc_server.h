// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_PANDORA_GRPC_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_PANDORA_GRPC_SERVER_H_

#include "grpc_services/a2dp.h"
#include "grpc_services/host.h"

#include <grpc++/grpc++.h>

// PandoraGrpcServer wraps a gRPC server implementing the Pandora Bluetooth test interfaces.
//
// TODO(https://fxbug.dev/316721276): Implement gRPCs necessary to enable GAP/A2DP testing.
class PandoraGrpcServer {
 public:
  // |dispatcher| is used for asynchronous FIDL communication with Sapphire.
  explicit PandoraGrpcServer(async_dispatcher_t* dispatcher);
  ~PandoraGrpcServer();

  // Start gRPC server on |port| if not already running. Set |verbose| to enable internal gRPC
  // logging. Returns ZX_OK on success and ZX_ERR_INTERNAL otherwise.
  zx_status_t Run(uint16_t port, bool verbose = false);

  // Stop gRPC server if running.
  void Shutdown();

  // Returns whether or not the gRPC server is running.
  bool IsRunning();

 private:
  HostService host_service_;
  A2dpService a2dp_service_;

  std::unique_ptr<grpc::Server> server_;
};

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_PANDORA_GRPC_SERVER_H_
