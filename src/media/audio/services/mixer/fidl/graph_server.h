// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_MIXER_FIDL_GRAPH_SERVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_MIXER_FIDL_GRAPH_SERVER_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/zx/profile.h>
#include <zircon/errors.h>

#include <list>
#include <memory>
#include <optional>

#include "src/media/audio/lib/clock/timer.h"
#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/mixer/common/basic_types.h"
#include "src/media/audio/services/mixer/fidl/clock_registry.h"
#include "src/media/audio/services/mixer/fidl/node.h"
#include "src/media/audio/services/mixer/fidl/ptr_decls.h"
#include "src/media/audio/services/mixer/mix/detached_thread.h"

namespace media_audio {

class GraphServer : public BaseFidlServer<GraphServer, fuchsia_audio_mixer::Graph>,
                    public std::enable_shared_from_this<GraphServer> {
 public:
  struct Args {
    // Name of this graph.
    // For debugging only: may be empty or not unique.
    std::string name;

    // The real-time FIDL thread.
    std::shared_ptr<const FidlThread> realtime_fidl_thread;

    // Factory to create clocks used by this graph.
    std::shared_ptr<ClockFactory> clock_factory;

    // Registry for all clocks used by this graph.
    std::shared_ptr<ClockRegistry> clock_registry;
  };

  // The returned server will live until the `server_end` channel is closed.
  static std::shared_ptr<GraphServer> Create(std::shared_ptr<const FidlThread> main_fidl_thread,
                                             fidl::ServerEnd<fuchsia_audio_mixer::Graph> server_end,
                                             Args args);

  // Implementation of fidl::WireServer<fuchsia_audio_mixer::Graph>.
  void CreateProducer(CreateProducerRequestView request,
                      CreateProducerCompleter::Sync& completer) override;
  void CreateConsumer(CreateConsumerRequestView request,
                      CreateConsumerCompleter::Sync& completer) override;
  void CreateMixer(CreateMixerRequestView request, CreateMixerCompleter::Sync& completer) override;
  void CreateSplitter(CreateSplitterRequestView request,
                      CreateSplitterCompleter::Sync& completer) override;
  void CreateCustom(CreateCustomRequestView request,
                    CreateCustomCompleter::Sync& completer) override;
  void DeleteNode(DeleteNodeRequestView request, DeleteNodeCompleter::Sync& completer) override;
  void CreateEdge(CreateEdgeRequestView request, CreateEdgeCompleter::Sync& completer) override;
  void DeleteEdge(DeleteEdgeRequestView request, DeleteEdgeCompleter::Sync& completer) override;
  void CreateThread(CreateThreadRequestView request,
                    CreateThreadCompleter::Sync& completer) override;
  void DeleteThread(DeleteThreadRequestView request,
                    DeleteThreadCompleter::Sync& completer) override;
  void CreateGainControl(CreateGainControlRequestView request,
                         CreateGainControlCompleter::Sync& completer) override;
  void DeleteGainControl(DeleteGainControlRequestView request,
                         DeleteGainControlCompleter::Sync& completer) override;
  void CreateGraphControlledReferenceClock(
      CreateGraphControlledReferenceClockCompleter::Sync& completer) override;

  // Name of this graph.
  // For debugging only: may be empty or not unique.
  std::string_view name() const { return name_; }

 private:
  static inline constexpr std::string_view kName = "GraphServer";
  template <class ServerT, class ProtocolT>
  friend class BaseFidlServer;

  // Note: args.server_end is consumed by BaseFidlServer.
  explicit GraphServer(Args args);

  void OnShutdown(fidl::UnbindInfo info) final;
  NodeId NextNodeId();
  ThreadId NextThreadId();

  const std::string name_;
  const std::shared_ptr<const FidlThread> realtime_fidl_thread_;
  const std::shared_ptr<DetachedThread> detached_thread_ = DetachedThread::Create();
  const std::shared_ptr<GlobalTaskQueue> global_task_queue_ = std::make_shared<GlobalTaskQueue>();
  const std::shared_ptr<ClockFactory> clock_factory_;
  const std::shared_ptr<ClockRegistry> clock_registry_;

  // Nodes mapping.
  std::unordered_map<NodeId, NodePtr> nodes_;
  NodeId next_node_id_ = 1;

  // Threads mapping.
  struct MixThreadInfo {
    MixThreadPtr ptr;
    int64_t num_consumers = 0;
  };
  std::unordered_map<ThreadId, MixThreadInfo> mix_threads_;
  ThreadId next_thread_id_ = 1;

  // List of pending one-shot waiters. Each waiter is responsible for removing themselves from this
  // list after they have run.
  std::list<async::WaitOnce> pending_one_shot_waiters_;

  // How many graph-controlled clocks have been created.
  int64_t num_graph_controlled_clocks_ = 0;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_MIXER_FIDL_GRAPH_SERVER_H_
