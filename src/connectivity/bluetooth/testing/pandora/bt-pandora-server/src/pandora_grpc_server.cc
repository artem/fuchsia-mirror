// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pandora_grpc_server.h"

#include <lib/syslog/cpp/macros.h>

#include <string>

PandoraGrpcServer::PandoraGrpcServer(async_dispatcher_t* dispatcher) : host_service_(dispatcher) {}

PandoraGrpcServer::~PandoraGrpcServer() { Shutdown(); }

zx_status_t PandoraGrpcServer::Run(uint16_t port, bool verbose) {
  if (IsRunning()) {
    return ZX_ERR_ALREADY_EXISTS;
  }

  if (verbose) {
    setenv("GRPC_TRACE", "http,api", 1);
    setenv("GRPC_VERBOSITY", "info", 1);
  }

  grpc::ServerBuilder builder;
  const std::string address = "0.0.0.0:" + std::to_string(port);
  builder.AddListeningPort(address, grpc::InsecureServerCredentials());
  builder.RegisterService(&host_service_);
  builder.RegisterService(&a2dp_service_);

  FX_LOGS(INFO) << "Server listening on " << address;
  server_ = builder.BuildAndStart();
  return (server_ ? ZX_OK : ZX_ERR_INTERNAL);
}

void PandoraGrpcServer::Shutdown() {
  if (IsRunning()) {
    FX_LOGS(INFO) << "shuting down Pandora gRPC server";
    server_->Shutdown();
    server_.reset(nullptr);
  }
}

bool PandoraGrpcServer::IsRunning() { return static_cast<bool>(server_); }
