// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_REMOTE_API_ADAPTER_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_REMOTE_API_ADAPTER_H_

#include "src/lib/fxl/macros.h"

namespace debug {
class StreamBuffer;
}

namespace debug_agent {

class RemoteAPI;

// Converts a raw stream of input data to a series of RemoteAPI calls.
class RemoteAPIAdapter {
 public:
  // Construct a RemoteAPIAdapter that's not hooked up to any streams. Attempting to call
  // |OnStreamReadable| in this state will do nothing.
  RemoteAPIAdapter() = default;

  // The stream will be used to read input and send replies back to the
  // client. The creator must set it up so that OnStreamReadable() is called
  // whenever there is new data to read on the stream.
  //
  // The pointers must outlive this class (ownership is not taken).
  RemoteAPIAdapter(RemoteAPI* remote_api, debug::StreamBuffer* stream);

  ~RemoteAPIAdapter() = default;

  void set_api(RemoteAPI* api) { api_ = api; }
  RemoteAPI* api() { return api_; }
  void set_stream(debug::StreamBuffer* stream) { stream_ = stream; }
  debug::StreamBuffer* stream() { return stream_; }

  // Callback for when data is available to read on the stream.
  void OnStreamReadable();

 private:
  // All pointers are non-owning.
  RemoteAPI* api_ = nullptr;
  debug::StreamBuffer* stream_ = nullptr;

  FXL_DISALLOW_COPY_AND_ASSIGN(RemoteAPIAdapter);
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_REMOTE_API_ADAPTER_H_
