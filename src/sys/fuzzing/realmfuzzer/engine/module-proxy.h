// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_SYS_FUZZING_REALMFUZZER_ENGINE_MODULE_PROXY_H_
#define SRC_SYS_FUZZING_REALMFUZZER_ENGINE_MODULE_PROXY_H_

#include <fuchsia/fuzzer/cpp/fidl.h>
#include <fuchsia/mem/cpp/fidl.h>
#include <stddef.h>
#include <stdint.h>

#include <array>
#include <memory>
#include <mutex>
#include <vector>

#include "src/lib/fxl/macros.h"
#include "src/sys/fuzzing/common/shared-memory.h"

namespace fuzzing {

// This class in the fuzzer engine is analogous to |fuzzing::Module| in an instrumented process.
// This association is one-to-many: The engine collects feedback from multiple processes which may
// possibly even restart. As a result it maintains a single |ModuleProxy| for all instances of a
// particular LLVM module across multiple processes, uniquely identified by the combination of its
// given |id| and its number of PCs, e.g. its size.
class ModuleProxy final {
 public:
  ModuleProxy(const std::string& id, size_t size);
  ~ModuleProxy() = default;

  const std::string& id() const { return id_; }

  // De/registers the shared memory as a source of counter values. This object does not take
  // ownership of the memory; it must remain valid between calls to |Add| and |Remove|.
  void Add(void* counters, size_t counters_len);
  void Remove(void* counters);

  // Collects counters for linked instances of the associated module, converts them to opaque
  // features and returns the number of new features. This method does not record the features, and
  // so is useful for evaluating a set of inputs as compared to a base set of features, e.g. from a
  // seed corpus. For info on "features", see: http://lcamtuf.coredump.cx/afl/technical_details.txt.
  size_t Measure();

  // Like |Measure|, but additionally records the new features, making the method useful for
  // incrementally growing a corpus.
  size_t Accumulate();

  // Returns how many PCs have accumulated at least one feature. If |out_num_features| is not null,
  // sets it to how many features have been accumulated in total.
  size_t GetCoverage(size_t* out_num_features);

  // Resets the recorded features.
  void Clear();

 private:
  size_t MeasureImpl(bool accumulate);

  const std::string id_;
  const size_t features_len_;
  const size_t extra_bytes_;

  std::vector<uint64_t*> counters_;

  // TODO(https://fxbug.dev/84363): Smaller inputs that cover previously observed features are currently
  // discarded. To help minimize the corpus, this object could also track the smallest input size
  // for each feature, in order to save smaller inputs and prefer them in a subsequent (possibly
  // periodic) merge.
  std::unique_ptr<uint64_t[]> features_;
  std::unique_ptr<uint64_t[]> accumulated_;

  FXL_DISALLOW_COPY_ASSIGN_AND_MOVE(ModuleProxy);
};

}  // namespace fuzzing

#endif  // SRC_SYS_FUZZING_REALMFUZZER_ENGINE_MODULE_PROXY_H_
