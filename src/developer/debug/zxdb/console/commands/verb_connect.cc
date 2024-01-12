// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/commands/verb_connect.h"

#include <map>
#include <string>

#include "src/developer/debug/zxdb/client/session.h"
#include "src/developer/debug/zxdb/common/inet_util.h"
#include "src/developer/debug/zxdb/console/command.h"
#include "src/developer/debug/zxdb/console/console.h"
#include "src/developer/debug/zxdb/console/output_buffer.h"
#include "src/developer/debug/zxdb/console/verbs.h"

namespace zxdb {

namespace {

constexpr int kUnixSwitch = 1;
constexpr int kQuietSwitch = 2;

const char kConnectShortHelp[] = R"(connect: Connect to a remote system for debugging.)";
const char kConnectHelp[] =
    R"(connect [ <remote_address> ]

  Connects to a debug_agent at the given address/port. With no arguments,
  attempts to reconnect to the previously used remote address.

  See also "disconnect".

Addresses

  Addresses can be of the form "<host> <port>" or "<host>:<port>". When using
  the latter form, IPv6 addresses must be [bracketed]. Otherwise the brackets
  are optional.

Options

  --unix-socket
  -u
      Attempt to connect to a unix socket. In this case <host> is a filesystem
      path.

  --quiet
  -q
      Produce less information console output.

Examples

  connect mystem.localnetwork 1234
  connect mystem.localnetwork:1234
  connect 192.168.0.4:1234
  connect 192.168.0.4 1234
  connect [1234:5678::9abc] 1234
  connect 1234:5678::9abc 1234
  connect [1234:5678::9abc]:1234
  connect -u /path/to/socket
)";

// Displays the failed connection error message. Connections are normally initiated on startup
// and it can be difficult to see the message with all the other normal startup messages. This
// can confuse users who wonder why nothing is working. As a result, make the message really big.
void DisplayConnectionFailed(CommandContext* cmd_context, const Err& err) {
  if (cmd_context->GetConsoleContext()->session()->IsConnected()) {
    // There could be a race connection (like the user hit enter twice rapidly when issuing the
    // connection command) that will cause a connection to fail because there's already one pending.
    // This might not have been knowable before issuing the command. If there's already a
    // connection, skip the big scary message.
    cmd_context->ReportError(err);
  } else {
    // Print a banner to highlight the error.
    OutputBuffer out;
    out.Append(Syntax::kError, "╒═══════════════════════════════════════════╕\n│ ");
    out.Append(Syntax::kHeading, "Connection to the debugged system failed. ");
    out.Append(Syntax::kError, "│\n╘═══════════════════════════════════════════╛\n");
    cmd_context->Output(out);
    cmd_context->ReportError(err);
  }
}

void RunVerbConnect(const Command& cmd, fxl::RefPtr<CommandContext> cmd_context) {
  SessionConnectionInfo connection_info;

  // Should always be present because we were called synchronously.
  ConsoleContext* console_context = cmd_context->GetConsoleContext();

  // Catch the "already connected" case early to display a simple low-key error message. This
  // avoids the more complex error messages issues by the Session object which might seem
  // out-of-context.
  if (console_context->session()->IsConnected()) {
    return cmd_context->ReportError(
        Err("connect: Already connected to the debugged system. Type \"status\" for more."));
  }

  const bool quiet = cmd.HasSwitch(kQuietSwitch);

  if (cmd.HasSwitch(kUnixSwitch)) {
    connection_info.type = SessionConnectionType::kUnix;
    if (cmd.args().size() == 1) {
      connection_info.host = cmd.args()[0];
    } else {
      return cmd_context->ReportError(Err(ErrType::kInput, "Too many arguments."));
    }
  } else {
    // 0 args means pass empty string and 0 port to try to reconnect.
    if (cmd.args().size() == 1) {
      const std::string& host_port = cmd.args()[0];
      // Provide an additional assist to users if they forget to wrap an IPv6 address in [].
      if (Ipv6HostPortIsMissingBrackets(host_port)) {
        return cmd_context->ReportError(Err(ErrType::kInput,
                                            "For IPv6 addresses use either: \"[::1]:1234\"\n"
                                            "or the two-parameter form: \"::1 1234."));
      }
      Err err = ParseHostPort(host_port, &connection_info.host, &connection_info.port);
      if (err.has_error())
        return cmd_context->ReportError(err);
      connection_info.type = SessionConnectionType::kNetwork;
    } else if (cmd.args().size() == 2) {
      Err err =
          ParseHostPort(cmd.args()[0], cmd.args()[1], &connection_info.host, &connection_info.port);
      if (err.has_error())
        return cmd_context->ReportError(err);
      connection_info.type = SessionConnectionType::kNetwork;
    } else if (cmd.args().size() > 2) {
      return cmd_context->ReportError(Err(ErrType::kInput, "Too many arguments."));
    }
  }

  console_context->session()->Connect(connection_info,
                                      [cmd_context, quiet](const Err& err) mutable {
                                        if (err.has_error()) {
                                          // Don't display error message if they canceled the
                                          // connection.
                                          if (err.type() != ErrType::kCanceled)
                                            DisplayConnectionFailed(cmd_context.get(), err);
                                        } else {
                                          if (!quiet)
                                            cmd_context->Output("Connected successfully.\n");
                                        }
                                      });
  if (!quiet)
    cmd_context->Output("Connecting (use \"disconnect\" to cancel)...\n");
}

}  // namespace

VerbRecord GetConnectVerbRecord() {
  SwitchRecord unix_switch(kUnixSwitch, false, "unix-socket", 'u');
  SwitchRecord quiet_switch(kQuietSwitch, false, "quiet", 'q');
  VerbRecord connect_record = VerbRecord(&RunVerbConnect, {"connect"}, kConnectShortHelp,
                                         kConnectHelp, CommandGroup::kGeneral);
  connect_record.switches.push_back(unix_switch);
  connect_record.switches.push_back(quiet_switch);
  return connect_record;
}

}  // namespace zxdb
