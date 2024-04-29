// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>

#include "src/media/audio/tools/wav_recorder/wav_recorder.h"

int main(int argc, const char** argv) {
  printf(
      "WARNING: wav_recorder is deprecated. Please use `ffx audio record` or\n"
      "or `ffx audio device record` to record WAV files. For more information, run \n"
      "`ffx audio record --help` and `ffx audio device record --help`\n\n");

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto component_context = sys::ComponentContext::CreateAndServeOutgoingDirectory();

  media::tools::WavRecorder wav_recorder(fxl::CommandLineFromArgcArgv(argc, argv), [&loop]() {
    async::PostTask(loop.dispatcher(), [&loop]() { loop.Quit(); });
  });

  wav_recorder.Run(component_context.get());
  loop.Run();

  return 0;
}
