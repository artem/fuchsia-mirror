// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/debug_adapter/handlers/request_breakpoint.h"

#include "src/developer/debug/zxdb/client/breakpoint.h"
#include "src/developer/debug/zxdb/client/breakpoint_settings.h"
#include "src/developer/debug/zxdb/client/session.h"

namespace zxdb {

std::string GetFile(const dap::Source& source) {
  if (source.sourceReference.has_value()) {
    // TODO: Add support for reference once SourceRequest is supported
  }

  // "path" is an absolute path.
  if (source.path.has_value()) {
    return source.path.value();
  }

  if (source.name.has_value()) {
    return source.name.value();
  }

  return std::string();
}
dap::ResponseOrError<dap::SetBreakpointsResponse> OnRequestBreakpoint(
    DebugAdapterContext* ctx, const dap::SetBreakpointsRequest& req) {
  dap::SetBreakpointsResponse response;
  size_t numBreakpoints = 0;

  auto file = GetFile(req.source);
  if (file.empty()) {
    return response;
  }

  // Delete all existing breakpoints in the file if any.
  ctx->DeleteBreakpointsForSource(file);

  // Add the specified breakpoints.
  if (req.breakpoints.has_value()) {
    auto const& breakpoints = req.breakpoints.value();
    numBreakpoints = breakpoints.size();

    for (size_t i = 0; i < numBreakpoints; i++) {
      auto& request_bp = breakpoints[i];

      Breakpoint* breakpoint = ctx->session()->system().CreateNewBreakpoint();
      BreakpointSettings settings;

      std::vector<InputLocation> locations;
      locations.emplace_back(FileLine(file, request_bp.line));
      settings.locations = locations;
      breakpoint->SetSettings(settings);
      ctx->StoreBreakpointForSource(file, breakpoint);

      dap::Breakpoint response_bp;
      response_bp.verified = (!breakpoint->GetLocations().empty());
      response_bp.id = ctx->IdForBreakpoint(breakpoint);
      response_bp.source = req.source;
      response_bp.line = request_bp.line;
      response.breakpoints.push_back(response_bp);
    }
  }
  return response;
}

}  // namespace zxdb
