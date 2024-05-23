// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/media/test/one_shot_event.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <stdint.h>
#include <stdio.h>

#include <thread>

#include <fbl/unique_fd.h>

#include "in_stream_peeker.h"
#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"
#include "src/media/codec/examples/use_media_decoder/in_stream_file.h"
#include "src/media/codec/examples/use_media_decoder/util.h"
#include "use_aac_decoder.h"
#include "use_video_decoder.h"

namespace {

// The 8MiB is needed for scanning for h264 start codes, not for VP9 ivf
// headers.  The 8MiB is fairly arbitrary - just meant to be larger than any
// frame size we'll encounter in the test streams we use.  We currently rely on
// finding the next start code within this distance - in future maybe it'd
// become worthwhile to incremenally continue an input AU if we haven't yet
// found the next start code / EOS, in which case this size could be made
// smaller.
constexpr uint32_t kMaxPeekBytes = 8 * 1024 * 1024;

void usage(const char* prog_name) {
  printf("usage: %s (--aac_adts|--h264) <input_file> [<output_file>]\n", prog_name);
}

std::optional<int32_t> GetInt32Option(const fxl::CommandLine& command_line,
                                      std::string option_name) {
  std::string option_as_string;
  if (!command_line.GetOptionValue(option_name.c_str(), &option_as_string)) {
    return std::nullopt;
  }
  errno = 0;
  int32_t result = std::stoi(option_as_string);
  if (errno == ERANGE) {
    printf("error parsing command line option as int32_t: %s\n", option_name.c_str());
    usage(command_line.argv0().c_str());
    exit(-1);
  }
  return result;
}

}  // namespace

int main(int argc, char* argv[]) {
  fxl::CommandLine command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetLogSettingsFromCommandLine(command_line)) {
    printf("fxl::SetLogSettingsFromCommandLine() failed\n");
    exit(-1);
  }
  if (command_line.positional_args().size() < 1 || command_line.positional_args().size() > 2) {
    usage(command_line.argv0().c_str());
    exit(-1);
  }

  async::Loop fidl_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  thrd_t fidl_thread;
  ZX_ASSERT(ZX_OK == fidl_loop.StartThread("fidl_thread", &fidl_thread));
  async_dispatcher_t* fidl_dispatcher = fidl_loop.dispatcher();

  // The moment we serve the outgoing directory + let the fidl_thread retrieve
  // anything from its port, we potentially are letting a request for
  // fuchsia::ui::views::View fail, since it'll fail to find the View service in
  // outgoing_services(), since we haven't yet added View to
  // outgoing_services().  A way to prevent this failure is by not letting
  // fidl_thread read from its port between Create() and
  // outgoing_services()+=View.
  //
  // To that end, we batch up the lambdas we want to run on the fidl_thread,
  // then run them all without returning to read from the port in between.
  //
  // We're intentionally running the fidl_thread separately, partly to
  // intentionally discover and implement example workarounds for this kind of
  // problem.  If you've just got a single thread that will later be used to
  // Run() the fidl dispatching, then this queueing of closures to run in a
  // single batch isn't relevant, since all the code before Run() is already in
  // an equivalent "batch".
  std::vector<fit::closure> to_run_on_fidl_thread;

  std::unique_ptr<sys::ComponentContext> component_context = sys::ComponentContext::Create();
  to_run_on_fidl_thread.emplace_back(
      [&component_context] { component_context->outgoing()->ServeFromStartupInfo(); });

  // This creates handles for channels for these protocols and connects them to
  // the namespace entries for these protocols. These handles will be bound to
  // InterfacePtr bindings objects on the fidl_thread as the binding objects
  // can only be safely used from a single thread.
  fuchsia::mediacodec::CodecFactoryHandle codec_factory;
  fuchsia::sysmem2::AllocatorHandle sysmem;
  component_context->svc()->Connect<fuchsia::mediacodec::CodecFactory>(codec_factory.NewRequest());
  component_context->svc()->Connect<fuchsia::sysmem2::Allocator>(sysmem.NewRequest());

  std::string input_file = command_line.positional_args()[0];
  std::string output_file_name;
  if (command_line.positional_args().size() >= 2) {
    output_file_name = command_line.positional_args()[1];
  }

  uint8_t md[SHA256_DIGEST_LENGTH];

  uint32_t loop_stream_count = GetInt32Option(command_line, "loop_stream_count").value_or(1);
  uint32_t frame_count =
      GetInt32Option(command_line, "frame_count").value_or(std::numeric_limits<int32_t>::max());

  // Now we can run everything we've queued in to_run_on_fidl_thread.
  PostSerial(fidl_dispatcher, [to_run_on_fidl_thread = std::move(to_run_on_fidl_thread)]() mutable {
    for (auto& to_run : to_run_on_fidl_thread) {
      // Move to local just to delete captures of each lambda before the next
      // one runs, to avoid being brittle in case a lambda starts to care about
      // that.
      fit::closure local_to_run = std::move(to_run);
      local_to_run();
      // ~local_to_run
    }
    // ~to_run_on_fidl_thread deletes some nullptr fit::closure(s)
  });
  ZX_DEBUG_ASSERT(to_run_on_fidl_thread.empty());

  auto in_stream_file =
      std::make_unique<InStreamFile>(&fidl_loop, fidl_thread, component_context.get(), input_file);
  auto in_stream_peeker = std::make_unique<InStreamPeeker>(
      &fidl_loop, fidl_thread, component_context.get(), std::move(in_stream_file), kMaxPeekBytes);

  fbl::unique_fd output_file;
  if (!output_file_name.empty()) {
    int output_file_desc =
        open(output_file_name.c_str(), O_CREAT | O_WRONLY | O_TRUNC, S_IRUSR | S_IWUSR);
    output_file.reset(output_file_desc);
    ZX_ASSERT(output_file.is_valid());
  }

  UseVideoDecoderTestParams test_params = {
      .loop_stream_count = loop_stream_count,
      .frame_count = frame_count,
  };

  // We set up a closure here just to avoid forcing the two decoder types to
  // take the same parameters, but still be able to share the
  // drive_decoder_thread code below.
  bool is_hash_valid = false;
  fit::closure use_decoder;
  if (command_line.HasOption("aac_adts")) {
    is_hash_valid = true;
    use_decoder = [&fidl_loop, codec_factory = std::move(codec_factory), sysmem = std::move(sysmem),
                   input_file, output_file_name, &md]() mutable {
      use_aac_decoder(&fidl_loop, std::move(codec_factory), std::move(sysmem), input_file,
                      output_file_name, md);
    };
  } else if (command_line.HasOption("h264")) {
    use_decoder = [&fidl_loop, fidl_thread, codec_factory = std::move(codec_factory),
                   sysmem = std::move(sysmem), in_stream_peeker = in_stream_peeker.get(),
                   &test_params]() mutable {
      UseVideoDecoderParams params{
          .fidl_loop = &fidl_loop,
          .fidl_thread = fidl_thread,
          .codec_factory = std::move(codec_factory),
          .sysmem = std::move(sysmem),
          .in_stream = in_stream_peeker,
          .test_params = &test_params,
      };
      use_h264_decoder(std::move(params));
    };
  } else if (command_line.HasOption("vp9")) {
    use_decoder = [&fidl_loop, fidl_thread, codec_factory = std::move(codec_factory),
                   sysmem = std::move(sysmem), in_stream_peeker = in_stream_peeker.get(),
                   &test_params]() mutable {
      UseVideoDecoderParams params{
          .fidl_loop = &fidl_loop,
          .fidl_thread = fidl_thread,
          .codec_factory = std::move(codec_factory),
          .sysmem = std::move(sysmem),
          .in_stream = in_stream_peeker,
          .test_params = &test_params,
      };
      use_vp9_decoder(std::move(params));
    };
  } else {
    usage(command_line.argv0().c_str());
    return -1;
  }

  use_decoder();

  fidl_loop.Quit();
  fidl_loop.JoinThreads();
  fidl_loop.Shutdown();

  if (is_hash_valid) {
    printf(
        "The sha256 of the output data (including data format "
        "parameters) is:\n");
    for (uint8_t byte : md) {
      printf("%02x", byte);
    }
    printf("\n");
  }

  // ~fidl_loop
  return 0;
}
