// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_CODEC_CODECS_VAAPI_BUFFER_POOL_H_
#define SRC_MEDIA_CODEC_CODECS_VAAPI_BUFFER_POOL_H_

#include <fuchsia/mediacodec/cpp/fidl.h>
#include <lib/media/codec_impl/codec_buffer.h>

#include <map>
#include <mutex>
#include <optional>

#include "src/lib/fxl/synchronization/thread_annotations.h"
#include "src/media/lib/mpsc_queue/mpsc_queue.h"

// BufferPool manages CodecBuffers for use with local output types in software
// encoders.
class BufferPool {
 public:
  struct Allocation {
    const CodecBuffer* buffer;
    size_t bytes_used;
  };

  void AddBuffer(const CodecBuffer* buffer);

  // Allocates a buffer for the caller and remembers the allocation size.
  const CodecBuffer* AllocateBuffer(size_t alloc_len = 0);

  // Frees a buffer by its base address, releasing it back to the pool.
  void FreeBuffer(uint8_t* base);

  // Looks up what buffer from the pool backs a frame Ffmpeg has output.
  std::optional<Allocation> FindBufferByBase(uint8_t* base);

  // Removes all free buffers and re-arms the buffer pool to block when
  // servicing allocation requests.
  //
  // If keep_data is true, does not modify the tracking for buffers already in
  // use.
  //
  // If keep_data is false, old buffers are forgotten, and it's up to client
  // code to avoid calling FreeBuffer() regarding any old buffer.
  void Reset(bool keep_data = false);

  // Stop blocking for new buffers when empty.
  void StopAllWaits();

  // Returns whether any buffers in the pool are currently allocated.
  bool has_buffers_in_use();

 private:
  std::mutex lock_;
  std::map<uint8_t*, Allocation> buffers_in_use_ FXL_GUARDED_BY(lock_);
  BlockingMpscQueue<const CodecBuffer*> free_buffers_;
};

#endif  // SRC_MEDIA_CODEC_CODECS_VAAPI_BUFFER_POOL_H_
