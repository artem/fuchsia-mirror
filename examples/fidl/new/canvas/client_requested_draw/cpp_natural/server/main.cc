// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/examples.canvas.clientrequesteddraw/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/syslog/cpp/macros.h>
#include <unistd.h>

#include <src/lib/fxl/macros.h>
#include <src/lib/fxl/memory/weak_ptr.h>

// A struct that stores the two things we care about for this example: the set of lines, and the
// bounding box that contains them.
struct CanvasState {
  // Tracks whether there has been a change since the last send, to prevent redundant updates.
  bool changed = true;
  // [START diff_1]
  // Tracks whether or not the client has declared itself ready to receive more updated.
  bool ready = true;
  // [END diff_1]
  examples_canvas_clientrequesteddraw::BoundingBox bounding_box;
};

// An implementation of the |Instance| protocol.
class InstanceImpl final : public fidl::Server<examples_canvas_clientrequesteddraw::Instance> {
 public:
  // Bind this implementation to a channel.
  InstanceImpl(async_dispatcher_t* dispatcher,
               fidl::ServerEnd<examples_canvas_clientrequesteddraw::Instance> server_end)
      : binding_(dispatcher, std::move(server_end), this, std::mem_fn(&InstanceImpl::OnFidlClosed)),
        weak_factory_(this) {
    // Start the update timer on startup. Our server sends one update per second
    ScheduleOnDrawnEvent(dispatcher, zx::sec(1));
  }

  void OnFidlClosed(fidl::UnbindInfo info) {
    if (info.reason() != ::fidl::Reason::kPeerClosed) {
      FX_LOGS(ERROR) << "Shutdown unexpectedly";
    }
    delete this;
  }

  // [START diff_2]
  void AddLines(AddLinesRequest& request, AddLinesCompleter::Sync& completer) override {
    FX_LOGS(INFO) << "AddLines request received";
    for (const auto& points : request.lines()) {
      FX_LOGS(INFO) << "AddLines printing line: [Point { x: " << points[1].x()
                    << ", y: " << points[1].y() << " }, Point { x: " << points[0].x()
                    << ", y: " << points[0].y() << " }]";
      // [END diff_2]

      // Update the bounding box to account for the new line we've just "added" to the canvas.
      auto& bounds = state_.bounding_box;
      for (const auto& point : points) {
        if (point.x() < bounds.top_left().x()) {
          bounds.top_left().x() = point.x();
        }
        if (point.y() > bounds.top_left().y()) {
          bounds.top_left().y() = point.y();
        }
        if (point.x() > bounds.bottom_right().x()) {
          bounds.bottom_right().x() = point.x();
        }
        if (point.y() < bounds.bottom_right().y()) {
          bounds.bottom_right().y() = point.y();
        }
      }
    }

    // Mark the state as "dirty", so that an update is sent back to the client on the next |OnDrawn|
    // event.
    state_.changed = true;
  }

  // [START diff_3]
  void Ready(ReadyCompleter::Sync& completer) override {
    FX_LOGS(INFO) << "Ready request received";

    // The client must only call `Ready() -> ();` after receiving an `-> OnDrawn();` event; if two
    // "consecutive" `Ready() -> ();` calls are received, this interaction has entered an invalid
    // state, and should be aborted immediately.
    if (state_.ready == true) {
      FX_LOGS(ERROR) << "Invalid back-to-back `Ready` requests received";
    }

    state_.ready = true;
    completer.Reply();
  }
  // [END diff_3]

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<examples_canvas_clientrequesteddraw::Instance> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_LOGS(WARNING) << "Received an unknown method with ordinal " << metadata.method_ordinal;
  }

 private:
  // Each scheduled update waits for the allotted amount of time, sends an update if something has
  // changed, and schedules the next update.
  void ScheduleOnDrawnEvent(async_dispatcher_t* dispatcher, zx::duration after) {
    async::PostDelayedTask(
        dispatcher,
        [&, dispatcher, after, weak = weak_factory_.GetWeakPtr()] {
          // Halt execution if the binding has been deallocated already.
          if (!weak) {
            return;
          }

          // Schedule the next update if the binding still exists.
          weak->ScheduleOnDrawnEvent(dispatcher, after);

          // [START diff_4]
          // No need to send an update if nothing has changed since the last one, or the client has
          // not yet informed us that it is ready for more updates.
          if (!weak->state_.changed || !weak->state_.ready) {
            return;
          }
          // [END diff_4]

          // This is where we would draw the actual lines. Since this is just an example, we'll
          // avoid doing the actual rendering, and simply send the bounding box to the client
          // instead.
          auto result = fidl::SendEvent(binding_)->OnDrawn(state_.bounding_box);
          if (!result.is_ok()) {
            return;
          }

          auto top_left = state_.bounding_box.top_left();
          auto bottom_right = state_.bounding_box.bottom_right();
          FX_LOGS(INFO) << "OnDrawn event sent: top_left: Point { x: " << top_left.x()
                        << ", y: " << top_left.y()
                        << " }, bottom_right: Point { x: " << bottom_right.x()
                        << ", y: " << bottom_right.y() << " }";

          // [START diff_5]
          // Reset the change and ready trackers.
          state_.ready = false;
          // [END diff_5]
          state_.changed = false;
        },
        after);
  }

  fidl::ServerBinding<examples_canvas_clientrequesteddraw::Instance> binding_;
  CanvasState state_ = CanvasState{};

  // Generates weak references to this object, which are appropriate to pass into asynchronous
  // callbacks that need to access this object. The references are automatically invalidated
  // if this object is destroyed.
  fxl::WeakPtrFactory<InstanceImpl> weak_factory_;
};

int main(int argc, char** argv) {
  FX_LOGS(INFO) << "Started";

  // The event loop is used to asynchronously listen for incoming connections and requests from the
  // client. The following initializes the loop, and obtains the dispatcher, which will be used when
  // binding the server implementation to a channel.
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();

  // Create an |OutgoingDirectory| instance.
  //
  // The |component::OutgoingDirectory| class serves the outgoing directory for our component. This
  // directory is where the outgoing FIDL protocols are installed so that they can be provided to
  // other components.
  component::OutgoingDirectory outgoing = component::OutgoingDirectory(dispatcher);

  // The `ServeFromStartupInfo()` function sets up the outgoing directory with the startup handle.
  // The startup handle is a handle provided to every component by the system, so that they can
  // serve capabilities (e.g. FIDL protocols) to other components.
  zx::result result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << result.status_string();
    return -1;
  }

  // Register a handler for components trying to connect to
  // |examples.canvas.clientrequesteddraw.Instance|.
  result = outgoing.AddUnmanagedProtocol<examples_canvas_clientrequesteddraw::Instance>(
      [dispatcher](fidl::ServerEnd<examples_canvas_clientrequesteddraw::Instance> server_end) {
        // Create an instance of our InstanceImpl that destroys itself when the connection closes.
        new InstanceImpl(dispatcher, std::move(server_end));
      });
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Instance protocol: " << result.status_string();
    return -1;
  }

  // Everything is wired up. Sit back and run the loop until an incoming connection wakes us up.
  FX_LOGS(INFO) << "Listening for incoming connections";
  loop.Run();
  return 0;
}
