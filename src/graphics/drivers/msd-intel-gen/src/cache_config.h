// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef CACHE_CONFIG_H
#define CACHE_CONFIG_H

#include <lib/magma_service/util/instruction_writer.h>
#include <stdint.h>

#include <memory>

#include "msd_intel_register_io.h"
#include "types.h"

class CacheConfig {
 public:
  // Returns the number of bytes required to write into the instruction stream.
  static uint32_t InstructionBytesRequired();

  // Assumes there is sufficient space available to write into the instruction stream.
  static bool InitCacheConfig(magma::InstructionWriter* writer, EngineCommandStreamerId engine_id);

  // On gen12 cache config is written directly to registers.
  static bool InitCacheConfigGen12(MsdIntelRegisterIo* register_io,
                                   std::shared_ptr<ForceWakeDomain> forcewake);

  static constexpr uint32_t kMemoryObjectControlStateEntries = 62;
  static constexpr uint32_t kLncfMemoryObjectControlStateEntries =
      kMemoryObjectControlStateEntries / 2;

  static_assert(kMemoryObjectControlStateEntries % 2 == 0,
                "kMemoryObjectControlStateEntries not even");

 private:
  static void GetLncfMemoryObjectControlState(std::vector<uint16_t>& mocs);
  static void GetMemoryObjectControlState(std::vector<uint32_t>& mocs);

  static void GetLncfMemoryObjectControlStateGen12(std::vector<uint16_t>& mocs);
  static void GetMemoryObjectControlStateGen12(std::vector<uint32_t>& mocs);

  friend class TestCacheConfig;
};

#endif  // CACHE_CONFIG
