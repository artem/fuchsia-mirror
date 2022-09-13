// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_MIXER_FIDL_PACKET_QUEUE_PRODUCER_NODE_H_
#define SRC_MEDIA_AUDIO_SERVICES_MIXER_FIDL_PACKET_QUEUE_PRODUCER_NODE_H_

#include "src/media/audio/services/mixer/fidl/node.h"
#include "src/media/audio/services/mixer/mix/producer_stage.h"
#include "src/media/audio/services/mixer/mix/simple_packet_queue_producer_stage.h"

namespace media_audio {

// This is an ordinary node driven by a queue of packets that feed into a PacketQueueProducerStage.
class PacketQueueProducerNode : public Node {
 public:
  struct Args {
    // Name of this node.
    std::string_view name;

    // Parent meta node.
    NodePtr parent;

    // Format of this Nodes's destination stream.
    Format format;

    // Reference clock of this nodes's destination stream.
    zx_koid_t reference_clock_koid;

    // Message queues for communicating with a packet-queue-based producer.
    std::shared_ptr<ProducerStage::CommandQueue> start_stop_command_queue;
    std::shared_ptr<SimplePacketQueueProducerStage::CommandQueue> packet_command_queue;

    // On creation, the node is initially assigned to this DetachedThread.
    DetachedThreadPtr detached_thread;
  };

  static std::shared_ptr<PacketQueueProducerNode> Create(Args args);

 private:
  PacketQueueProducerNode(std::string_view name, PipelineStagePtr pipeline_stage, NodePtr parent)
      : Node(name, /*is_meta=*/false, std::move(pipeline_stage), std::move(parent)) {}

  // Implementation of Node.
  NodePtr CreateNewChildSource() final {
    UNREACHABLE << "CreateNewChildSource should not be called on ordinary nodes";
  }
  NodePtr CreateNewChildDest() final {
    UNREACHABLE << "CreateNewChildDest should not be called on ordinary nodes";
  }
  bool CanAcceptSource(NodePtr src) const final;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_MIXER_FIDL_PACKET_QUEUE_PRODUCER_NODE_H_
