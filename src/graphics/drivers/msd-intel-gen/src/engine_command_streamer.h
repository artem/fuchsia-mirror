// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ENGINE_COMMAND_STREAMER_H
#define ENGINE_COMMAND_STREAMER_H

#include <memory>
#include <queue>

#include "address_space.h"
#include "hardware_status_page.h"
#include "magma_util/register_io.h"
#include "magma_util/status.h"
#include "mapped_batch.h"
#include "msd_intel_context.h"
#include "pagetable.h"
#include "render_init_batch.h"
#include "scheduler.h"
#include "sequencer.h"

class EngineCommandStreamer {
 public:
  class Owner {
   public:
    virtual magma::RegisterIo* register_io() = 0;
    virtual Sequencer* sequencer() = 0;
    virtual HardwareStatusPage* hardware_status_page(EngineCommandStreamerId id) = 0;
    // Keep the device informed when we have scheduled command sequences
    virtual void batch_submitted(uint32_t sequence_number) = 0;
  };

  EngineCommandStreamer(Owner* owner, EngineCommandStreamerId id, uint32_t mmio_base);

  virtual ~EngineCommandStreamer() {}

  EngineCommandStreamerId id() const { return id_; }

  // Initialize backing store for the given context on this engine command streamer.
  bool InitContext(MsdIntelContext* context) const;

  bool InitContextWorkarounds(MsdIntelContext* context);
  bool InitContextCacheConfig(MsdIntelContext* context);

  // Initialize engine command streamer hardware.
  void InitHardware();

  uint64_t GetActiveHeadPointer();

  virtual void SubmitBatch(std::unique_ptr<MappedBatch> batch) = 0;

  virtual bool IsIdle() = 0;

  virtual bool Reset();

  bool StartBatchBuffer(MsdIntelContext* context, uint64_t gpu_addr,
                        AddressSpaceType address_space_type);

 protected:
  virtual bool ExecBatch(std::unique_ptr<MappedBatch> mapped_batch) = 0;

  bool SubmitContext(MsdIntelContext* context, uint32_t tail);
  bool UpdateContext(MsdIntelContext* context, uint32_t tail);
  void SubmitExeclists(MsdIntelContext* context);
  void InvalidateTlbs();

  // from intel-gfx-prm-osrc-bdw-vol03-gpu_overview_3.pdf p.7
  static constexpr uint32_t kRenderEngineMmioBase = 0x2000;

  magma::RegisterIo* register_io() { return owner_->register_io(); }

  uint32_t mmio_base() const { return mmio_base_; }

  Sequencer* sequencer() { return owner_->sequencer(); }

  HardwareStatusPage* hardware_status_page(EngineCommandStreamerId id) {
    return owner_->hardware_status_page(id);
  }

  void batch_submitted(uint32_t sequence_number) { owner_->batch_submitted(sequence_number); }

 private:
  virtual uint32_t GetContextSize() const { return PAGE_SIZE * 2; }

  bool InitContextBuffer(MsdIntelBuffer* context_buffer, Ringbuffer* ringbuffer,
                         AddressSpace* address_space) const;

  Owner* owner_;
  EngineCommandStreamerId id_;
  uint32_t mmio_base_;

  friend class TestEngineCommandStreamer;
};

class InflightCommandSequence {
 public:
  InflightCommandSequence(uint32_t sequence_number, uint32_t ringbuffer_offset,
                          std::unique_ptr<MappedBatch> mapped_batch)
      : sequence_number_(sequence_number),
        ringbuffer_offset_(ringbuffer_offset),
        mapped_batch_(std::move(mapped_batch)) {}

  uint32_t sequence_number() { return sequence_number_; }

  uint32_t ringbuffer_offset() { return ringbuffer_offset_; }

  std::weak_ptr<MsdIntelContext> GetContext() { return mapped_batch_->GetContext(); }

  MappedBatch* mapped_batch() { return mapped_batch_.get(); }

  InflightCommandSequence(InflightCommandSequence&& seq) {
    sequence_number_ = seq.sequence_number_;
    ringbuffer_offset_ = seq.ringbuffer_offset_;
    mapped_batch_ = std::move(seq.mapped_batch_);
  }

 private:
  uint32_t sequence_number_;
  uint32_t ringbuffer_offset_;
  std::unique_ptr<MappedBatch> mapped_batch_;
};

#endif  // ENGINE_COMMAND_STREAMER_H
