// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_SYMBOLIZER_SYMBOLIZER_H_
#define TOOLS_SYMBOLIZER_SYMBOLIZER_H_

#include <lib/fit/function.h>

#include <iostream>
#include <string_view>

namespace symbolizer {

// This is the core logic of the symbolizer. The implementation is separated from the interface here
// for better testing.
class Symbolizer {
 public:
  enum class AddressType {
    kUnknown,
    kReturnAddress,   // :ra suffix
    kProgramCounter,  // :pc suffix
  };
  enum class ResetType {
    kUnknown,
    kBegin,  // :begin suffix
    kEnd,    // :end suffix
  };
  virtual ~Symbolizer() = default;

  // Each of the following functions corresponds to one markup tag in
  // //docs/reference/kernel/symbolizer_markup.md.

  // The output is printed through the last parameter of each function rather than returned
  // directly, so that the implementation can output asynchronously.
  using OutputFn = fit::function<void(std::string_view)>;

  // {{{reset}}}, {{{reset:begin}}}, {{{reset:end}}}
  // Resets the internal state and starts processing the stack trace for a new process.
  // symbolizing_dart indicates whether the next stack stace is from Dart. The behavior could be
  // slightly different for Dart and other stack traces.
  virtual void Reset(bool symbolizing_dart, ResetType type, OutputFn output) = 0;

  // {{{module:%i:%s:%s:...}}}
  // Adds a module to the current process, indexed by id.
  virtual void Module(uint64_t id, std::string_view name, std::string_view build_id,
                      OutputFn output) = 0;

  // {{{mmap:%p:%x:...}}}
  // Associates a memory region with the module indexed by its id.
  virtual void MMap(uint64_t address, uint64_t size, uint64_t module_id, std::string_view flags,
                    uint64_t module_offset, OutputFn output) = 0;

  // {{{bt:%u:%p}}}, {{{bt:%u:%p:ra}}}, {{{bt:%u:%p:pc}}}
  // Represents one frame in the backtrace. We'll output the symbolized content for each frame.
  virtual void Backtrace(uint64_t frame_id, uint64_t address, AddressType type,
                         std::string_view message, OutputFn output) = 0;

  // {{{dumpfile:%s:%s}}}
  // Dumps the current modules and mmaps to a json file.
  virtual void DumpFile(std::string_view type, std::string_view name, OutputFn output) = 0;
};

}  // namespace symbolizer

#endif  // TOOLS_SYMBOLIZER_SYMBOLIZER_H_
