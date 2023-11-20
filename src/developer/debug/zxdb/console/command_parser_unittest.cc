// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/command_parser.h"

#include <gtest/gtest.h>

#include "src/developer/debug/zxdb/common/err.h"
#include "src/developer/debug/zxdb/console/command.h"
#include "src/developer/debug/zxdb/console/nouns.h"

namespace zxdb {

namespace {

bool CompletionContains(const std::vector<std::string>& suggestions, const std::string& contains) {
  return std::find(suggestions.begin(), suggestions.end(), contains) != suggestions.end();
}

}  // namespace

TEST(CommandParser, Tokenizer) {
  std::vector<CommandToken> output;

  EXPECT_FALSE(TokenizeCommand("", &output).has_error());
  EXPECT_TRUE(output.empty());

  EXPECT_FALSE(TokenizeCommand("   ", &output).has_error());
  EXPECT_TRUE(output.empty());

  EXPECT_FALSE(TokenizeCommand("a", &output).has_error());
  ASSERT_EQ(1u, output.size());
  EXPECT_EQ(0u, output[0].offset);
  EXPECT_EQ("a", output[0].str);

  EXPECT_FALSE(TokenizeCommand("ab cd", &output).has_error());
  ASSERT_EQ(2u, output.size());
  EXPECT_EQ(0u, output[0].offset);
  EXPECT_EQ("ab", output[0].str);
  EXPECT_EQ(3u, output[1].offset);
  EXPECT_EQ("cd", output[1].str);

  EXPECT_FALSE(TokenizeCommand("  ab  cd  ", &output).has_error());
  ASSERT_EQ(2u, output.size());
  EXPECT_EQ(2u, output[0].offset);
  EXPECT_EQ("ab", output[0].str);
  EXPECT_EQ(6u, output[1].offset);
  EXPECT_EQ("cd", output[1].str);

  // Quoting.
  EXPECT_FALSE(TokenizeCommand("  \"one string\" R\"(two\"string)\"  ", &output).has_error());
  ASSERT_EQ(2u, output.size());
  EXPECT_EQ(2u, output[0].offset);
  EXPECT_EQ("one string", output[0].str);
  EXPECT_EQ(15u, output[1].offset);
  EXPECT_EQ("two\"string", output[1].str);

  // Quoting in the middle of a token.
  EXPECT_FALSE(TokenizeCommand(" foo\"one string\"bar ", &output).has_error());
  ASSERT_EQ(1u, output.size());
  EXPECT_EQ(1u, output[0].offset);
  EXPECT_EQ("fooone stringbar", output[0].str);

  // Single quotes don't quote.
  EXPECT_FALSE(TokenizeCommand("123'456", &output).has_error());
  ASSERT_EQ(1u, output.size());
  EXPECT_EQ(0u, output[0].offset);
  EXPECT_EQ("123'456", output[0].str);
}

TEST(CommandParser, ParserBasic) {
  Command output;

  // Verb-only command.
  Err err = ParseCommand("pause", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_TRUE(output.nouns().empty());
  EXPECT_EQ(Verb::kPause, output.verb());

  // Noun-only command.
  err = ParseCommand("process", &output);
  EXPECT_FALSE(err.has_error()) << err.msg();
  EXPECT_EQ(1u, output.nouns().size());
  EXPECT_TRUE(output.HasNoun(Noun::kProcess));
  EXPECT_EQ(Command::kNoIndex, output.GetNounIndex(Noun::kProcess));
  EXPECT_EQ(Verb::kNone, output.verb());

  // Noun-index command.
  err = ParseCommand("process 1", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.nouns().size());
  EXPECT_TRUE(output.HasNoun(Noun::kProcess));
  EXPECT_EQ(1, output.GetNounIndex(Noun::kProcess));
  EXPECT_EQ(Verb::kNone, output.verb());

  // Noun-verb command.
  err = ParseCommand("process pause", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.nouns().size());
  EXPECT_TRUE(output.HasNoun(Noun::kProcess));
  EXPECT_EQ(Command::kNoIndex, output.GetNounIndex(Noun::kProcess));
  EXPECT_EQ(Verb::kPause, output.verb());

  // Noun-index-verb command.
  err = ParseCommand("process 2 pause", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.nouns().size());
  EXPECT_TRUE(output.HasNoun(Noun::kProcess));
  EXPECT_EQ(2, output.GetNounIndex(Noun::kProcess));
  EXPECT_EQ(Verb::kPause, output.verb());

  err = ParseCommand("process 2 thread 1 pause", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(2u, output.nouns().size());
  EXPECT_TRUE(output.HasNoun(Noun::kProcess));
  EXPECT_EQ(2, output.GetNounIndex(Noun::kProcess));
  EXPECT_TRUE(output.HasNoun(Noun::kThread));
  EXPECT_EQ(1, output.GetNounIndex(Noun::kThread));
  EXPECT_EQ(Verb::kPause, output.verb());
}

TEST(CommandParser, ParserBasicErrors) {
  Command output;

  // Unknown command in different contexts.
  Err err = ParseCommand("zzyzx", &output);
  EXPECT_TRUE(err.has_error());
  EXPECT_EQ("The string \"zzyzx\" is not a valid verb.", err.msg());

  err = ParseCommand("process 1 zzyzx", &output);
  EXPECT_TRUE(err.has_error());
  EXPECT_EQ("The string \"zzyzx\" is not a valid verb.", err.msg());

  err = ParseCommand("process 1 process pause", &output);
  EXPECT_TRUE(err.has_error());
  EXPECT_EQ("Noun \"process\" specified twice.", err.msg());
}

TEST(CommandParser, NounSwitches) {
  Command output;

  // Look up the switch ID for the "-v" noun switch.
  const SwitchRecord* verbose_switch = nullptr;
  for (const auto& sr : GetNounSwitches()) {
    if (sr.ch == 'v') {
      verbose_switch = &sr;
      break;
    }
  }
  ASSERT_TRUE(verbose_switch);

  Err err = ParseCommand("frame -", &output);
  EXPECT_TRUE(err.has_error());
  EXPECT_EQ("Invalid switch \"-\".", err.msg());

  // Valid short switch.
  err = ParseCommand("frame -v", &output);
  EXPECT_FALSE(err.has_error()) << err.msg();
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_TRUE(output.HasSwitch(verbose_switch->id));

  // Valid long switch.
  err = ParseCommand("frame --verbose", &output);
  EXPECT_FALSE(err.has_error()) << err.msg();
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_TRUE(output.HasSwitch(verbose_switch->id));
}

TEST(CommandParser, VerbSwitches) {
  Command output;

  // Look up the switch ID for the size switch to "memory read". This allows
  // the checks below to stay in sync without knowing much about how the memory
  // command is implemented.
  const auto& verbs = GetVerbs();
  auto read_record = verbs.find(Verb::kMemRead);
  ASSERT_NE(read_record, verbs.end());
  const SwitchRecord* size_switch = nullptr;
  for (const auto& sr : read_record->second.switches) {
    if (sr.ch == 's') {
      size_switch = &sr;
      break;
    }
  }
  ASSERT_TRUE(size_switch);

  Err err = ParseCommand("mem-read -", &output);
  EXPECT_TRUE(err.has_error());
  EXPECT_EQ("Invalid switch \"-\".", err.msg());

  // Valid long switch with no equals.
  err = ParseCommand("mem-read --size 234 next", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_EQ("234", output.GetSwitchValue(size_switch->id));
  ASSERT_EQ(1u, output.args().size());
  EXPECT_EQ("next", output.args()[0]);

  // Quoted value.
  err = ParseCommand("mem-read --size \"23 4\" next", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_EQ("23 4", output.GetSwitchValue(size_switch->id));
  ASSERT_EQ(1u, output.args().size());
  EXPECT_EQ("next", output.args()[0]);

  // Valid long switch with equals sign.
  err = ParseCommand("mem-read --size=234 next", &output);
  EXPECT_FALSE(err.has_error()) << err.msg();
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_EQ("234", output.GetSwitchValue(size_switch->id));
  ASSERT_EQ(1u, output.args().size());
  EXPECT_EQ("next", output.args()[0]);

  // Valid long switch with equals and no value (this is OK, value is empty
  // string).
  err = ParseCommand("mem-read --size= next", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_EQ("", output.GetSwitchValue(size_switch->id));
  ASSERT_EQ(1u, output.args().size());
  EXPECT_EQ("next", output.args()[0]);

  // Expects a value for a long switch.
  err = ParseCommand("mem-read --size", &output);
  EXPECT_TRUE(err.has_error());
  EXPECT_EQ("Argument needed for \"--size\".", err.msg());

  // Valid short switch with value following.
  err = ParseCommand("mem-read -s 567 next", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_EQ("567", output.GetSwitchValue(size_switch->id));
  ASSERT_EQ(1u, output.args().size());
  EXPECT_EQ("next", output.args()[0]);

  // Valid short switch with value concatenated.
  err = ParseCommand("mem-read -s567 next", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_EQ("567", output.GetSwitchValue(size_switch->id));
  ASSERT_EQ(1u, output.args().size());
  EXPECT_EQ("next", output.args()[0]);

  // Short switch with quoted value.
  err = ParseCommand("mem-read -s\"56 7\" next", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(1u, output.switches().size());
  EXPECT_EQ("56 7", output.GetSwitchValue(size_switch->id));
  ASSERT_EQ(1u, output.args().size());
  EXPECT_EQ("next", output.args()[0]);

  // Expects a value for a short switch.
  err = ParseCommand("mem-read -s", &output);
  EXPECT_TRUE(err.has_error());
  EXPECT_EQ("Argument needed for \"-s\".", err.msg());
}

// Some verbs take all arguments as one large string, ignoring whitespace. "print" is one of these.
TEST(CommandParser, OneParam) {
  Command output;

  Err err = ParseCommand("print  x + 2 ", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(Verb::kPrint, output.verb());

  ASSERT_EQ(1u, output.args().size());

  // The whitespace at the end is not trimmed. This could possibly be changed in the future.
  EXPECT_EQ("x + 2 ", output.args()[0]);

  err = ParseCommand("opendump path/has a/space/mini.dmp", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(Verb::kOpenDump, output.verb());

  ASSERT_EQ(1u, output.args().size());

  // Even unquoted, the path is consumed as a single string.
  EXPECT_EQ("path/has a/space/mini.dmp", output.args()[0]);

  err = ParseCommand("opendump \"path/has a/space/mini.dmp\"", &output);
  EXPECT_FALSE(err.has_error());
  EXPECT_EQ(Verb::kOpenDump, output.verb());

  ASSERT_EQ(1u, output.args().size());

  // A quoted path should continue to work regardless of the OneParam setting.
  EXPECT_EQ("\"path/has a/space/mini.dmp\"", output.args()[0]);
}

TEST(CommandParser, Completions) {
  std::vector<std::string> comp;

  // Noun completion.
  comp = GetCommandCompletions("t", FillCommandContextCallback());
  EXPECT_TRUE(CompletionContains(comp, "thread"));

  // Verb completion.
  comp = GetCommandCompletions("h", FillCommandContextCallback());
  EXPECT_TRUE(CompletionContains(comp, "help"));

  // Noun + Verb completion.
  comp = GetCommandCompletions("process 2 p", FillCommandContextCallback());
  EXPECT_TRUE(CompletionContains(comp, "process 2 pause"));

  // Ending in a space gives everything.
  comp = GetCommandCompletions("process ", FillCommandContextCallback());
  EXPECT_TRUE(CompletionContains(comp, "process quit"));
  EXPECT_TRUE(CompletionContains(comp, "process pause"));

  // No input should give everything
  comp = GetCommandCompletions("", FillCommandContextCallback());
  EXPECT_TRUE(CompletionContains(comp, "pause"));
  EXPECT_TRUE(CompletionContains(comp, "quit"));

  // Verb with no argument prefix
  comp = GetCommandCompletions("set ", FillCommandContextCallback());
  EXPECT_TRUE(CompletionContains(comp, "set source-map"));
  EXPECT_TRUE(CompletionContains(comp, "set language"));

  // Verb with argument prefix
  comp = GetCommandCompletions("set lan", FillCommandContextCallback());
  EXPECT_TRUE(CompletionContains(comp, "set language"));
  EXPECT_EQ(1u, comp.size());
}

}  // namespace zxdb
