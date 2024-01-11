// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/verbs_settings.h"

#include <gtest/gtest.h>

#include "src/developer/debug/shared/platform_message_loop.h"
#include "src/developer/debug/zxdb/client/execution_scope.h"
#include "src/developer/debug/zxdb/client/mock_remote_api.h"
#include "src/developer/debug/zxdb/client/process.h"
#include "src/developer/debug/zxdb/client/remote_api_test.h"
#include "src/developer/debug/zxdb/console/console_context.h"
#include "src/developer/debug/zxdb/console/mock_console.h"
#include "src/developer/debug/zxdb/symbols/loaded_module_symbols.h"
#include "src/developer/debug/zxdb/symbols/process_symbols.h"

namespace zxdb {

namespace {

// TODO(brettw) Convert to a ConsoleTest to remove some boilerplate.
class VerbsSettingsTest : public RemoteAPITest {
 public:
  VerbsSettingsTest() : RemoteAPITest() {}

  // The MockConsole must live in a smaller scope than the Session/System managed by the
  // RemoteAPITest's SetUp()/TearDown() functions.
  void SetUp() override {
    RemoteAPITest::SetUp();
    console_ = std::make_unique<MockConsole>(&session());
    console_->EnableOutput();
  }
  void TearDown() override {
    console_.reset();
    RemoteAPITest::TearDown();
  }

  MockConsole& console() { return *console_; }

  // Runs an input command and returns the text synchronously reported from it.
  std::string DoInput(const std::string& input) {
    console_->ProcessInputLine(input);

    auto event = console_->GetOutputEvent();
    EXPECT_EQ(MockConsole::OutputEvent::Type::kOutput, event.type);

    return event.output.AsString();
  }

 private:
  std::unique_ptr<MockConsole> console_;
};

}  // namespace

TEST(VerbsSettings, ParseSetCommand) {
  // No input.
  ErrOr<ParsedSetCommand> result = ParseSetCommand("");
  ASSERT_TRUE(result.has_error());
  EXPECT_EQ("Expected a setting name to set.", result.err().msg());

  // Missing value.
  result = ParseSetCommand("foo");
  ASSERT_TRUE(result.has_error());
  EXPECT_EQ("Expecting a value to set. Use \"set setting-name setting-value\".",
            result.err().msg());

  // Regular two-argument form.
  result = ParseSetCommand("foo bar");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ("foo", result.value().name);
  EXPECT_EQ(ParsedSetCommand::kAssign, result.value().op);
  EXPECT_EQ("bar", result.value().raw_value);
  ASSERT_EQ(1u, result.value().values.size());
  EXPECT_EQ("bar", result.value().values[0]);

  // Value ending in hyphen.
  result = ParseSetCommand("foo-");
  ASSERT_TRUE(result.has_error());
  EXPECT_EQ("Invalid setting name.", result.err().msg());

  result = ParseSetCommand("foo- bar");
  ASSERT_TRUE(result.has_error());
  EXPECT_EQ("Invalid setting name.", result.err().msg());

  // Whitespace or an assignment operator is required after the name.
  result = ParseSetCommand("foo#bar");
  ASSERT_FALSE(result.ok());
  EXPECT_EQ("Invalid setting name.", result.err().msg());

  // Regular three-argument form with spaces.
  result = ParseSetCommand("foo = bar ");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ("foo", result.value().name);
  EXPECT_EQ(ParsedSetCommand::kAssign, result.value().op);
  EXPECT_EQ("bar", result.value().raw_value);
  ASSERT_EQ(1u, result.value().values.size());
  EXPECT_EQ("bar", result.value().values[0]);

  // Regular three-argument form with no spaces and an "append" mode.
  result = ParseSetCommand("foo+=bar");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ("foo", result.value().name);
  EXPECT_EQ(ParsedSetCommand::kAppend, result.value().op);
  ASSERT_EQ(1u, result.value().values.size());
  EXPECT_EQ("bar", result.value().values[0]);

  // Remove mode with value quoting.
  result = ParseSetCommand("foo-=\"bar baz\"");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ("foo", result.value().name);
  EXPECT_EQ(ParsedSetCommand::kRemove, result.value().op);
  EXPECT_EQ("\"bar baz\"", result.value().raw_value);
  ASSERT_EQ(1u, result.value().values.size());
  EXPECT_EQ("bar baz", result.value().values[0]);

  // Assign many values.
  result = ParseSetCommand("foo bar baz");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ("foo", result.value().name);
  EXPECT_EQ(ParsedSetCommand::kAssign, result.value().op);
  EXPECT_EQ("bar baz", result.value().raw_value);
  ASSERT_EQ(2u, result.value().values.size());
  EXPECT_EQ("bar", result.value().values[0]);
  EXPECT_EQ("baz", result.value().values[1]);

  result = ParseSetCommand("foo+=bar \"baz goo\"");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ("foo", result.value().name);
  EXPECT_EQ(ParsedSetCommand::kAppend, result.value().op);
  EXPECT_EQ("bar \"baz goo\"", result.value().raw_value);
  ASSERT_EQ(2u, result.value().values.size());
  EXPECT_EQ("bar", result.value().values[0]);
  EXPECT_EQ("baz goo", result.value().values[1]);
}

TEST_F(VerbsSettingsTest, GetSet) {
  console().FlushOutputEvents();

  // "get" with no input.
  EXPECT_EQ("", DoInput("get --value-only source-map"));

  // "get" with an invalid object (there is no active filter).
  EXPECT_EQ("No current object of this type.", DoInput("filter get --value-only"));

  // Process qualified set.
  EXPECT_EQ(
      "Set process 1 source-map = \n"
      "  • prdir\n",
      DoInput("pr set source-map prdir"));

  // Both the unqualified and process-qualified one should get it,
  EXPECT_EQ("prdir", DoInput("get --value-only source-map"));
  EXPECT_EQ("prdir", DoInput("process get --value-only source-map"));

  // Globally qualified set.
  EXPECT_EQ(
      "Set global source-map = \n"
      "  • gldir\n",
      DoInput("global set source-map gldir"));

  // The globally qualified one should return it, but the unqualified one should return the process
  // since it's more specific.
  EXPECT_EQ("prdir", DoInput("process get --value-only source-map"));
  EXPECT_EQ("gldir", DoInput("global get --value-only source-map"));
  EXPECT_EQ("prdir", DoInput("get --value-only source-map"));

  // Unqualified set.
  EXPECT_EQ(
      "Set global source-map = \n"
      "  • gldir2\n",
      DoInput("set source-map gldir2"));

  // Append.
  EXPECT_EQ(
      "Set global source-map = \n"
      "  • gldir2\n"
      "  • gldir3\n"
      "  • \"gldir four\"\n",
      DoInput("set source-map += gldir3 \"gldir four\""));

  // Create a breakpoint and test scope assignment (this doesn't need to be quoted).
  DoInput("break main");
  EXPECT_EQ("Set breakpoint 1 scope = pr 1\n", DoInput("bp 1 set scope pr 1"));

  // There is no current thread so setting the thread scope should fail.
  EXPECT_EQ("There are no threads in the process.", DoInput("bp 1 set scope thread 1"));
  EXPECT_EQ("There is no current thread to use for the scope.", DoInput("bp 1 set scope thread"));

  EXPECT_EQ("gldir2 gldir3 \"gldir four\"", DoInput("global get --value-only source-map"));
  EXPECT_EQ("prdir", DoInput("get --value-only source-map"));

  // Check invalid values.
  EXPECT_EQ("Could not find setting \"unknown-setting\".", DoInput("set unknown-setting = blah"));
  EXPECT_EQ("Could not find setting \"unknown-setting\".", DoInput("get unknown-setting"));
}

TEST_F(VerbsSettingsTest, ParseExecutionScope) {
  ConsoleContext context(&session());

  // Inject one running process and thread.
  constexpr int kProcessKoid = 1234;
  InjectProcess(kProcessKoid);
  InjectThread(kProcessKoid, 5678);

  // Random input.
  ErrOr<ExecutionScope> result = ParseExecutionScope(&context, "something invalid");
  EXPECT_TRUE(result.has_error());

  // Valid nouns but not the right ones.
  result = ParseExecutionScope(&context, "breakpoint 3");
  EXPECT_TRUE(result.has_error());

  // Verb (not valid).
  result = ParseExecutionScope(&context, "process next");
  EXPECT_TRUE(result.has_error());

  // Valid global scope.
  result = ParseExecutionScope(&context, "global");
  EXPECT_TRUE(result.ok());
  EXPECT_EQ(ExecutionScope::kSystem, result.value().type());

  // Valid process scope.
  result = ParseExecutionScope(&context, "process");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ(ExecutionScope::kTarget, result.value().type());
  result = ParseExecutionScope(&context, "process 1");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ(ExecutionScope::kTarget, result.value().type());

  // Invalid process scope (there's only one).
  result = ParseExecutionScope(&context, "process 2");
  EXPECT_TRUE(result.has_error());

  // Valid thread scope.
  result = ParseExecutionScope(&context, "thread");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ(ExecutionScope::kThread, result.value().type());
  result = ParseExecutionScope(&context, "process 1 thread 1");
  ASSERT_TRUE(result.ok());
  EXPECT_EQ(ExecutionScope::kThread, result.value().type());

  // Invalid process scope (there's only one).
  result = ParseExecutionScope(&context, "thread 2");
  EXPECT_TRUE(result.has_error());
}

}  // namespace zxdb
