// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/command_line_options.h"

#include <lib/cmdline/args_parser.h>

#include <filesystem>
#include <iostream>
#include <system_error>

namespace zxdb {

namespace {

// Appears at the top of the --help output above the switch list.
const char kHelpIntro[] = R"(zxdb [ <options> ]

  For information on using the debugger, type "help" at the interactive prompt.

Options

)";

const char kUnixConnectHelp[] = R"(  --unix-connect=<filepath>
  -u <filepath>
      Attempts to connect to a debug_agent through a unix socket.)";

const char kConnectHelp[] = R"(  --connect=<host>:<port>
  -c <host>:<port>
      Attempts to connect to a debug_agent running on the given host/port.)";

const char kCoreHelp[] = R"(  --core=<filename>
      Attempts to open a core file for analysis.)";

const char kDebugModeHelp[] = R"(  --debug-mode
  -d
      Output debug information about zxdb.
      Should only be useful for people developing zxdb.)";

const char kHelpHelp[] = R"(  --help
  -h
      Prints all command-line switches.)";

const char kAttachHelp[] = R"(  --attach=<koid|filter>
  -a <koid|filter>
      Attaches to the given process or creates a filter that matches current
      and future processes. The argument will be parsed in the same way as the
      "attach" command in the console. Multiple attaches can be specified to
      match more than one process.)";

const char kScriptFileHelp[] = R"(  --script-file=<file>
  -S <file>
      Reads a script file from a file. The file must contains valid zxdb
      commands as they would be input from the command line. They will be
      executed sequentially.)";

const char kExecuteCommandHelp[] = R"(  --execute=<command>
  -e <command>
      Execute one zxdb command. Multiple commands will be executed sequentially.)";

const char kSymbolIndexHelp[] = R"(  --symbol-index=<path>
      Populates --ids-txt and --build-id-dir using the given symbol-index file,
      which defaults to ~/.fuchsia/debug/symbol-index.json. The file should be
      created and maintained by the "symbol-index" host tool.)";

const char kSymbolPathHelp[] = R"(  --symbol-path=<path>
  -s <path>
      Adds the given directory or file to the symbol search path. Multiple
      -s switches can be passed to add multiple locations. When a directory
      path is passed, the directory will be enumerated non-recursively to
      index all ELF files. When a file is passed, it will be loaded as an ELF
      file (if possible).)";

const char kBuildIdDirHelp[] = R"(  --build-id-dir=<path>
      Adds the given directory to the symbol search path. Multiple
      --build-id-dir switches can be passed to add multiple directories.
      The directory must have the same structure as a .build-id directory,
      that is, each symbol file lives at xx/yyyyyyyy.debug where xx is
      the first two characters of the build ID and yyyyyyyy is the rest.
      However, the name of the directory doesn't need to be .build-id.)";

const char kIdsTxtHelp[] = R"(  --ids-txt=<path>
      Adds the given file to the symbol search path. Multiple --ids-txt
      switches can be passed to add multiple files. The file, typically named
      "ids.txt", serves as a mapping from build ID to symbol file path and
      should contain multiple lines in the format of "<build ID> <file path>".)";

const char kSymbolCacheHelp[] = R"(  --symbol-cache=<path>
      Directory where we can keep a symbol cache, which defaults to
      ~/.fuchsia/debug/symbol-cache. If a symbol server has been specified,
      downloaded symbols will be stored in this directory. The directory
      structure will be the same as a .build-id directory, and symbols will
      be read from this location as though you had specified
      "--build-id-dir=<path>".)";

const char kSymbolServerHelp[] = R"(  --symbol-server=<url>
      Adds the given URL to symbol servers. Symbol servers host the debug
      symbols for prebuilt binaries and dynamic libraries.)";

using ::analytics::core_dev_tools::kAnalyticsHelp;
using ::analytics::core_dev_tools::kAnalyticsShowHelp;

const char kVersionHelp[] = R"(  --version
  -v
      Prints the version.)";

const char kEnableDebugAdapterHelp[] = R"(  --enable-debug-adapter
      Starts the debug adapter that serves debug adapter protocol.
      This is useful for connecting the debugger with an IDE.)";

const char kDebugAdapterPortHelp[] = R"(  --debug-adapter-port=<port>
      Uses this port number to serve debug adapter protocol.
      By default 15678 is used.)";

const char kNoAutoAttachLimboHelp[] = R"(  --no-auto-attach-limbo
  -n
      Disables automatically attaching to all processes found in Process Limbo
      upon successful connection.)";

}  // namespace

cmdline::Status ParseCommandLine(int argc, const char* argv[], CommandLineOptions* options,
                                 std::vector<std::string>* params) {
  using analytics::core_dev_tools::AnalyticsOption;
  using analytics::core_dev_tools::ParseAnalyticsOption;

  cmdline::ArgsParser<CommandLineOptions> parser;

  parser.AddSwitch("connect", 'c', kConnectHelp, &CommandLineOptions::connect);
  parser.AddSwitch("unix-connect", 'u', kUnixConnectHelp, &CommandLineOptions::unix_connect);
  parser.AddSwitch("core", 0, kCoreHelp, &CommandLineOptions::core);
  parser.AddSwitch("debug-mode", 'd', kDebugModeHelp, &CommandLineOptions::debug_mode);
  parser.AddSwitch("attach", 'a', kAttachHelp, &CommandLineOptions::attach);
  parser.AddSwitch("script-file", 'S', kScriptFileHelp, &CommandLineOptions::script_files);
  parser.AddSwitch("execute", 'e', kExecuteCommandHelp, &CommandLineOptions::execute_commands);
  parser.AddSwitch("symbol-index", 0, kSymbolIndexHelp, &CommandLineOptions::symbol_index_files);
  parser.AddSwitch("symbol-path", 's', kSymbolPathHelp, &CommandLineOptions::symbol_paths);
  parser.AddSwitch("build-id-dir", 0, kBuildIdDirHelp, &CommandLineOptions::build_id_dirs);
  parser.AddSwitch("ids-txt", 0, kIdsTxtHelp, &CommandLineOptions::ids_txts);
  parser.AddSwitch("symbol-cache", 0, kSymbolCacheHelp, &CommandLineOptions::symbol_cache);
  parser.AddSwitch("symbol-server", 0, kSymbolServerHelp, &CommandLineOptions::symbol_servers);
  parser.AddSwitch("version", 'v', kVersionHelp, &CommandLineOptions::requested_version);
  parser.AddSwitch("analytics", 0, kAnalyticsHelp, &CommandLineOptions::analytics);
  parser.AddSwitch("analytics-show", 0, kAnalyticsShowHelp, &CommandLineOptions::analytics_show);
  parser.AddSwitch("enable-debug-adapter", 0, kEnableDebugAdapterHelp,
                   &CommandLineOptions::enable_debug_adapter);
  parser.AddSwitch("debug-adapter-port", 0, kDebugAdapterPortHelp,
                   &CommandLineOptions::debug_adapter_port);
  parser.AddSwitch("no-auto-attach-limbo", 'n', kNoAutoAttachLimboHelp,
                   &CommandLineOptions::no_auto_attach_limbo);

  // Special --help switch which doesn't exist in the options structure.
  bool requested_help = false;
  parser.AddGeneralSwitch("help", 'h', kHelpHelp, [&requested_help]() { requested_help = true; });

  cmdline::Status status = parser.Parse(argc, argv, options, params);
  if (status.has_error())
    return status;

  // Handle --help switch since we're the one that knows about the switches.
  if (requested_help)
    return cmdline::Status::Error(kHelpIntro + parser.GetHelp());

  // Default values for vector types.
  if (const char* home = std::getenv("HOME"); home) {
    std::string home_str = home;
    if (!options->symbol_cache) {
      options->symbol_cache = home_str + "/.fuchsia/debug/symbol-cache";
    }
    if (options->symbol_index_files.empty()) {
      std::error_code ec;
      std::string symbol_index = home_str + "/.fuchsia/debug/symbol-index.json";
      if (std::filesystem::exists(symbol_index, ec)) {
        options->symbol_index_files.push_back(symbol_index);
      }
    }
    std::string zxdbrc = home_str + "/.fuchsia/debug/zxdbrc";
    std::error_code ec;
    if (std::filesystem::exists(zxdbrc, ec)) {
      // zxdbrc is expected to execute first.
      options->script_files.insert(options->script_files.begin(), zxdbrc);
    }
  }

  return cmdline::Status::Ok();
}

}  // namespace zxdb
