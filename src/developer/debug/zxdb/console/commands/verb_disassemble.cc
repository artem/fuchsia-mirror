// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/commands/verb_disassemble.h"

#include "src/developer/debug/zxdb/client/arch_info.h"
#include "src/developer/debug/zxdb/client/frame.h"
#include "src/developer/debug/zxdb/client/memory_dump.h"
#include "src/developer/debug/zxdb/client/process.h"
#include "src/developer/debug/zxdb/client/session.h"
#include "src/developer/debug/zxdb/client/source_file_provider_impl.h"
#include "src/developer/debug/zxdb/console/command.h"
#include "src/developer/debug/zxdb/console/command_utils.h"
#include "src/developer/debug/zxdb/console/console.h"
#include "src/developer/debug/zxdb/console/format_context.h"
#include "src/developer/debug/zxdb/console/input_location_parser.h"
#include "src/developer/debug/zxdb/console/output_buffer.h"
#include "src/developer/debug/zxdb/console/verbs.h"
#include "src/developer/debug/zxdb/symbols/code_block.h"
#include "src/developer/debug/zxdb/symbols/location.h"

namespace zxdb {

namespace {

constexpr int kNumSwitch = 1;
constexpr int kRawSwitch = 2;

const char kDisassembleShortHelp[] = "disassemble / di: Disassemble machine instructions.";
const char kDisassembleHelp[] =
    R"(disassemble [ --num=<lines> ] [ --raw ] [ <location> ]

  Alias: "di"

  Disassembles machine instructions at the given location. If no location is
  given, the instruction pointer of the thread/frame will be used. If the
  thread is not stopped, you must specify a start address.

Location arguments

)" LOCATION_ARG_HELP("disassemble") LOCATION_EXPRESSION_HELP("disassemble")
        R"(        It is the user's responsibility to make sure that the starting address
        expression is appropriately aligned on an instruction boundary. For ARM
        this will be multiples of 4 bytes. For Intel, you will have to know
        some other way.

Arguments

  --num=<lines> | -n <lines>
      The number of lines/instructions to emit. Defaults to the instructions
      in the given function (if the location is a function name), or 16
      otherwise.

  --raw | -r
      Output raw bytes in addition to the decoded instructions.

Examples

  di
  disassemble
      Disassembles starting at the current thread's instruction pointer.

  thread 3 disassemble -n 128
      Disassembles 128 instructions starting at thread 3's instruction
      pointer.

  di MyClass::MyFunc
      Disassembles the given function.

  frame 3 disassemble
  thread 2 frame 3 disassemble
      Disassembles starting at the thread's "frame 3" instruction pointer
      (which will be the call return address).

  process 1 disassemble 0x7b851239a0
      Disassembles instructions in process 1 starting at the given address.

  disassemble *pc - 0x10   # arm64
  disassemble *rip - 0x10  # x64
  disassemble -- -0x10
  disassemble -- +0x10
      Disassembles instructions starting at the active thread's PC with relative
      byte offset.
)";

// Completion callback after reading process memory.
void CompleteDisassemble(fxl::RefPtr<CommandContext> cmd_context, const Err& err, MemoryDump dump,
                         fxl::WeakPtr<Process> weak_process, const FormatAsmOpts& options) {
  if (err.has_error())
    return cmd_context->ReportError(err);

  if (!weak_process)
    return;  // Give up if the process went away.

  OutputBuffer out;
  Err format_err =
      FormatAsmContext(weak_process->session()->arch_info(), dump, options, weak_process.get(),
                       SourceFileProviderImpl(weak_process->GetTarget()->settings()), &out);
  if (format_err.has_error())
    return cmd_context->ReportError(err);

  cmd_context->Output(out);
}

void RunDisassembleVerb(const Command& cmd, fxl::RefPtr<CommandContext> cmd_context) {
  // Can take process overrides (to specify which process to read) and thread and frame ones (to
  // specify which thread to read the instruction pointer from).
  if (Err err = cmd.ValidateNouns({Noun::kProcess, Noun::kThread, Noun::kFrame}); err.has_error())
    return cmd_context->ReportError(err);

  ConsoleContext* console_context = cmd_context->GetConsoleContext();
  if (Err err = AssertRunningTarget(console_context, "disassemble", cmd.target()); err.has_error())
    return cmd_context->ReportError(err);

  FormatAsmOpts options;
  options.emit_addresses = true;

  if (cmd.frame())
    options.active_address = cmd.frame()->GetAddress();

  // We may want to add an option for this.
  options.include_source = true;

  // Num argument (optional).
  //
  // When there is no known byte size, compute the max bytes requires to get the requested
  // instructions. It doesn't matter if we request more memory than necessary so use a high bound.
  size_t size = 0;
  bool size_is_default = false;  // Indicates size may be overridden below.
  if (cmd.HasSwitch(kNumSwitch)) {
    // Num lines explicitly given.
    uint64_t num_instr = 0;
    if (Err err = StringToUint64(cmd.GetSwitchValue(kNumSwitch), &num_instr); err.has_error())
      return cmd_context->ReportError(err);
    options.max_instructions = num_instr;
    size = options.max_instructions * console_context->session()->arch_info().max_instr_len();
  } else {
    // Default instruction count when no symbol and no explicit size is given.
    options.max_instructions = 16;
    size = options.max_instructions * console_context->session()->arch_info().max_instr_len();
    size_is_default = true;
  }

  // Show bytes.
  options.emit_bytes = cmd.HasSwitch(kRawSwitch);

  Process* process = cmd.target()->GetProcess();
  auto weak_process = process->GetWeakPtr();

  if (cmd.args().size() > 1) {
    return cmd_context->ReportError(
        Err("\"disassemble\" requires exactly one argument specifying a location."));
  } else if (cmd.args().empty() || cmd.args()[0][0] == '+' || cmd.args()[0][0] == '-') {
    int64_t offset = 0;
    if (!cmd.args().empty()) {
      std::string number = cmd.args()[0];

      // Need to trim the leading "+" for the integer parser.
      if (number[0] == '+') {
        number = number.substr(1);
      }

      auto err = StringToInt64(number, &offset);

      if (err.has_error()) {
        return cmd_context->ReportError(
            Err("pc relative byte offset input did not parse as a number: %s", err.msg().c_str()));
      }
    }

    // No args: implicitly read the frame's instruction pointer.
    //
    // TODO(brettw) by default it would be nice if this showed a few lines of disassembly before the
    // given address. Going backwards in x86 can be dicey though, the formatter may have to
    // guess-and-check about a good starting boundary for the dump.
    Frame* frame = cmd.frame();
    if (!frame) {
      return cmd_context->ReportError(
          Err("There is no frame to read the instruction pointer from. The thread\n"
              "must be stopped to use the implicit current address. Otherwise,\n"
              "you must supply an explicit address to disassemble."));
    }
    Location location = frame->GetLocation();

    // Schedule memory request.
    process->ReadMemory(location.address() + offset, size,
                        [cmd_context, options, weak_process](const Err& err, MemoryDump dump) {
                          CompleteDisassemble(cmd_context, err, std::move(dump),
                                              std::move(weak_process), options);
                        });
  } else {
    // One arg: parse as an input location.
    EvalLocalInputLocation(
        GetEvalContextForCommand(cmd), cmd.frame(), cmd.args()[0],
        [cmd_context, options, size, size_is_default, weak_process](
            ErrOr<std::vector<InputLocation>> locs, std::optional<uint32_t> expr_size) mutable {
          if (locs.has_error())
            return cmd_context->ReportError(locs.err());
          if (!weak_process)
            return cmd_context->ReportError(Err("Process terminated."));

          Location location;
          if (Err err = ResolveUniqueInputLocation(weak_process->GetSymbols(), locs.value(), true,
                                                   &location);
              err.has_error()) {
            cmd_context->ReportError(err);
            return;
          }

          // Some symbols can give us sizes which we will prefer to use instead of the default size.
          // All input locations will have the same type (matching the user input type).
          if (size_is_default && locs.value()[0].type == InputLocation::Type::kName) {
            if (location.symbol()) {
              if (const CodeBlock* block = location.symbol().Get()->As<CodeBlock>()) {
                size = block->GetFullRange(location.symbol_context()).size();
                options.max_instructions = 0;  // No instruction limit.
              }
            }
          }

          // Schedule memory request.
          weak_process->ReadMemory(
              location.address(), size,
              [cmd_context, options, weak_process](const Err& err, MemoryDump dump) {
                CompleteDisassemble(cmd_context, err, std::move(dump), std::move(weak_process),
                                    options);
              });
        });
  }
}

}  // namespace

VerbRecord GetDisassembleVerbRecord() {
  VerbRecord disass(&RunDisassembleVerb, &CompleteInputLocation, {"disassemble", "di"},
                    kDisassembleShortHelp, kDisassembleHelp, CommandGroup::kAssembly,
                    SourceAffinity::kAssembly);
  disass.param_type = VerbRecord::kOneParam;  // Don't require quoting for expressions.

  disass.switches.emplace_back(kNumSwitch, true, "num", 'n');
  disass.switches.emplace_back(kRawSwitch, false, "raw", 'r');
  return disass;
}

}  // namespace zxdb
