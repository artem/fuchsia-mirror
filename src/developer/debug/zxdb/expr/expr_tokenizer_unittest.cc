// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/expr/expr_tokenizer.h"

#include <gtest/gtest.h>

namespace zxdb {

TEST(ExprTokenizer, Empty) {
  ExprTokenizer t((std::string()));

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  EXPECT_TRUE(t.tokens().empty());
}

TEST(ExprTokenizer, InvalidChar) {
  // Offsets:      012345
  ExprTokenizer t("1234 ` hello");

  EXPECT_FALSE(t.Tokenize());
  EXPECT_TRUE(t.err().has_error());
  EXPECT_EQ(
      "Invalid character '`' in expression.\n"
      "  1234 ` hello\n"
      "       ^",
      t.err().msg());
  EXPECT_EQ(5u, t.error_location());
}

TEST(ExprTokenizer, Punctuation) {
  // Char offsets: 0 123456789012345678901234567890123
  // Token #'s:      0 1 2  3 45 67 8 9  0  1 2 3 4 56
  ExprTokenizer t("\n. * -> & () [] - :: == = % + ^ /@");

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(17u, tokens.size());

  EXPECT_EQ(ExprTokenType::kDot, tokens[0].type());
  EXPECT_EQ(".", tokens[0].value());
  EXPECT_EQ(1u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kStar, tokens[1].type());
  EXPECT_EQ("*", tokens[1].value());
  EXPECT_EQ(3u, tokens[1].byte_offset());

  EXPECT_EQ(ExprTokenType::kArrow, tokens[2].type());
  EXPECT_EQ("->", tokens[2].value());
  EXPECT_EQ(5u, tokens[2].byte_offset());

  EXPECT_EQ(ExprTokenType::kAmpersand, tokens[3].type());
  EXPECT_EQ("&", tokens[3].value());
  EXPECT_EQ(8u, tokens[3].byte_offset());

  EXPECT_EQ(ExprTokenType::kLeftParen, tokens[4].type());
  EXPECT_EQ("(", tokens[4].value());
  EXPECT_EQ(10u, tokens[4].byte_offset());

  EXPECT_EQ(ExprTokenType::kRightParen, tokens[5].type());
  EXPECT_EQ(")", tokens[5].value());
  EXPECT_EQ(11u, tokens[5].byte_offset());

  EXPECT_EQ(ExprTokenType::kLeftSquare, tokens[6].type());
  EXPECT_EQ("[", tokens[6].value());
  EXPECT_EQ(13u, tokens[6].byte_offset());

  EXPECT_EQ(ExprTokenType::kRightSquare, tokens[7].type());
  EXPECT_EQ("]", tokens[7].value());
  EXPECT_EQ(14u, tokens[7].byte_offset());

  EXPECT_EQ(ExprTokenType::kMinus, tokens[8].type());
  EXPECT_EQ("-", tokens[8].value());
  EXPECT_EQ(16u, tokens[8].byte_offset());

  EXPECT_EQ(ExprTokenType::kColonColon, tokens[9].type());
  EXPECT_EQ("::", tokens[9].value());
  EXPECT_EQ(18u, tokens[9].byte_offset());

  EXPECT_EQ(ExprTokenType::kEquality, tokens[10].type());
  EXPECT_EQ("==", tokens[10].value());
  EXPECT_EQ(21u, tokens[10].byte_offset());

  EXPECT_EQ(ExprTokenType::kEquals, tokens[11].type());
  EXPECT_EQ("=", tokens[11].value());
  EXPECT_EQ(24u, tokens[11].byte_offset());

  EXPECT_EQ(ExprTokenType::kPercent, tokens[12].type());
  EXPECT_EQ("%", tokens[12].value());
  EXPECT_EQ(26u, tokens[12].byte_offset());

  EXPECT_EQ(ExprTokenType::kPlus, tokens[13].type());
  EXPECT_EQ("+", tokens[13].value());
  EXPECT_EQ(28u, tokens[13].byte_offset());

  EXPECT_EQ(ExprTokenType::kCaret, tokens[14].type());
  EXPECT_EQ("^", tokens[14].value());
  EXPECT_EQ(30u, tokens[14].byte_offset());

  EXPECT_EQ(ExprTokenType::kSlash, tokens[15].type());
  EXPECT_EQ("/", tokens[15].value());
  EXPECT_EQ(32u, tokens[15].byte_offset());

  EXPECT_EQ(ExprTokenType::kAt, tokens[16].type());
  EXPECT_EQ("@", tokens[16].value());
  EXPECT_EQ(33u, tokens[16].byte_offset());
}

TEST(ExprTokenizer, LessGreater) {
  // "<<" is identified normally, but ">>" is always tokenized as two separate '>' tokens. The
  // parser will disambiguate in the next phase which we don't see here.

  // Char offsets: 0123456789012345678901234567890123456
  // Token #'s:    0 1 2  34 5 6789
  ExprTokenizer t("< > << >> <<<>>>");

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(10u, tokens.size());

  EXPECT_EQ(ExprTokenType::kLess, tokens[0].type());
  EXPECT_EQ("<", tokens[0].value());
  EXPECT_EQ(0u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kGreater, tokens[1].type());
  EXPECT_EQ(">", tokens[1].value());
  EXPECT_EQ(2u, tokens[1].byte_offset());

  EXPECT_EQ(ExprTokenType::kShiftLeft, tokens[2].type());
  EXPECT_EQ("<<", tokens[2].value());
  EXPECT_EQ(4u, tokens[2].byte_offset());

  EXPECT_EQ(ExprTokenType::kGreater, tokens[3].type());
  EXPECT_EQ(">", tokens[3].value());
  EXPECT_EQ(7u, tokens[3].byte_offset());

  EXPECT_EQ(ExprTokenType::kGreater, tokens[4].type());
  EXPECT_EQ(">", tokens[4].value());
  EXPECT_EQ(8u, tokens[4].byte_offset());

  EXPECT_EQ(ExprTokenType::kShiftLeft, tokens[5].type());
  EXPECT_EQ("<<", tokens[5].value());
  EXPECT_EQ(10u, tokens[5].byte_offset());

  EXPECT_EQ(ExprTokenType::kLess, tokens[6].type());
  EXPECT_EQ("<", tokens[6].value());
  EXPECT_EQ(12u, tokens[6].byte_offset());

  EXPECT_EQ(ExprTokenType::kGreater, tokens[7].type());
  EXPECT_EQ(">", tokens[7].value());
  EXPECT_EQ(13u, tokens[7].byte_offset());

  EXPECT_EQ(ExprTokenType::kGreater, tokens[8].type());
  EXPECT_EQ(">", tokens[8].value());
  EXPECT_EQ(14u, tokens[8].byte_offset());

  EXPECT_EQ(ExprTokenType::kGreater, tokens[9].type());
  EXPECT_EQ(">", tokens[9].value());
  EXPECT_EQ(15u, tokens[9].byte_offset());
}

TEST(ExprTokenizer, Integers) {
  // Note that the tokenizer doesn't validate numbers, so "7hello" is extracted as a number token.
  // The complex rules for number validation and conversion will be done at a higher layer.

  // Char offsets: 01234567890123456789012345678901
  // Token #'s:    0    12 34 5          6        7
  ExprTokenizer t("1234 -56-1 0x5a4bcdef 0o123llu 7_hell'o ");

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(8u, tokens.size());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[0].type());
  EXPECT_EQ("1234", tokens[0].value());
  EXPECT_EQ(0u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kMinus, tokens[1].type());
  EXPECT_EQ("-", tokens[1].value());
  EXPECT_EQ(5u, tokens[1].byte_offset());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[2].type());
  EXPECT_EQ("56", tokens[2].value());
  EXPECT_EQ(6u, tokens[2].byte_offset());

  EXPECT_EQ(ExprTokenType::kMinus, tokens[3].type());
  EXPECT_EQ("-", tokens[3].value());
  EXPECT_EQ(8u, tokens[3].byte_offset());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[4].type());
  EXPECT_EQ("1", tokens[4].value());
  EXPECT_EQ(9u, tokens[4].byte_offset());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[5].type());
  EXPECT_EQ("0x5a4bcdef", tokens[5].value());
  EXPECT_EQ(11u, tokens[5].byte_offset());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[6].type());
  EXPECT_EQ("0o123llu", tokens[6].value());
  EXPECT_EQ(22u, tokens[6].byte_offset());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[7].type());
  EXPECT_EQ("7_hell'o", tokens[7].value());
  EXPECT_EQ(31u, tokens[7].byte_offset());
}

// Most floating-point stuff is checked by the NumberParser test. This just validates integration.
TEST(ExprTokenizer, Floats) {
  ExprTokenizer t(" 7.2 3e12 78e+19+1 ");

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(5u, tokens.size());

  EXPECT_EQ(ExprTokenType::kFloat, tokens[0].type());
  EXPECT_EQ("7.2", tokens[0].value());
  EXPECT_EQ(1u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kFloat, tokens[1].type());
  EXPECT_EQ("3e12", tokens[1].value());
  EXPECT_EQ(5u, tokens[1].byte_offset());

  EXPECT_EQ(ExprTokenType::kFloat, tokens[2].type());
  EXPECT_EQ("78e+19", tokens[2].value());
  EXPECT_EQ(10u, tokens[2].byte_offset());

  EXPECT_EQ(ExprTokenType::kPlus, tokens[3].type());
  EXPECT_EQ("+", tokens[3].value());
  EXPECT_EQ(16u, tokens[3].byte_offset());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[4].type());
  EXPECT_EQ("1", tokens[4].value());
  EXPECT_EQ(17u, tokens[4].byte_offset());
}

TEST(ExprTokenizer, CStrings) {
  // Char offsets:         0 12345 67890123 45 67890 12345678
  // Token #'s:            0                 1
  ExprTokenizer c_strings("\"some\\tstring\" R\"(raw\"string)\"", ExprLanguage::kC);

  EXPECT_TRUE(c_strings.Tokenize());
  EXPECT_FALSE(c_strings.err().has_error()) << c_strings.err().msg();
  const auto& tokens = c_strings.tokens();
  ASSERT_EQ(2u, tokens.size());

  EXPECT_EQ(ExprTokenType::kStringLiteral, tokens[0].type());
  EXPECT_EQ("some\tstring", tokens[0].value());
  EXPECT_EQ(0u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kStringLiteral, tokens[1].type());
  EXPECT_EQ("raw\"string", tokens[1].value());
  EXPECT_EQ(15u, tokens[1].byte_offset());

  ExprTokenizer err_string("\"No terminator", ExprLanguage::kC);
  EXPECT_FALSE(err_string.Tokenize());
  EXPECT_TRUE(err_string.err().has_error());
  EXPECT_EQ("Hit end of input before the end of the string.", err_string.err().msg());
}

TEST(ExprTokenizer, RustStrings) {
  // Char offsets:            0 12345 67890123 45 67890 12345678 9
  // Token #'s:               0                 1
  ExprTokenizer rust_strings("\"some\\tstring\" r#\"raw\"string\"#", ExprLanguage::kRust);

  EXPECT_TRUE(rust_strings.Tokenize());
  EXPECT_FALSE(rust_strings.err().has_error()) << rust_strings.err().msg();
  const auto& tokens = rust_strings.tokens();
  ASSERT_EQ(2u, tokens.size());

  EXPECT_EQ(ExprTokenType::kStringLiteral, tokens[0].type());
  EXPECT_EQ("some\tstring", tokens[0].value());
  EXPECT_EQ(0u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kStringLiteral, tokens[1].type());
  EXPECT_EQ("raw\"string", tokens[1].value());
  EXPECT_EQ(15u, tokens[1].byte_offset());

  ExprTokenizer err_string("\"No terminator", ExprLanguage::kRust);
  EXPECT_FALSE(err_string.Tokenize());
  EXPECT_TRUE(err_string.err().has_error());
  EXPECT_EQ("Hit end of input before the end of the string.", err_string.err().msg());
}

TEST(ExprTokenizer, OtherLiterals) {
  // Char offsets: 01234567890123456789012345678901234567890123
  // Token #'s:    0    1    2   34     5      6     7        8
  ExprTokenizer t("true True true)false falsey const volatile restrict");

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(9u, tokens.size());

  EXPECT_EQ(ExprTokenType::kTrue, tokens[0].type());
  EXPECT_EQ("true", tokens[0].value());
  EXPECT_EQ(0u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kName, tokens[1].type());
  EXPECT_EQ("True", tokens[1].value());
  EXPECT_EQ(5u, tokens[1].byte_offset());

  EXPECT_EQ(ExprTokenType::kTrue, tokens[2].type());
  EXPECT_EQ("true", tokens[2].value());
  EXPECT_EQ(10u, tokens[2].byte_offset());

  EXPECT_EQ(ExprTokenType::kRightParen, tokens[3].type());
  EXPECT_EQ(")", tokens[3].value());
  EXPECT_EQ(14u, tokens[3].byte_offset());

  EXPECT_EQ(ExprTokenType::kFalse, tokens[4].type());
  EXPECT_EQ("false", tokens[4].value());
  EXPECT_EQ(15u, tokens[4].byte_offset());

  EXPECT_EQ(ExprTokenType::kName, tokens[5].type());
  EXPECT_EQ("falsey", tokens[5].value());
  EXPECT_EQ(21u, tokens[5].byte_offset());

  EXPECT_EQ(ExprTokenType::kConst, tokens[6].type());
  EXPECT_EQ("const", tokens[6].value());
  EXPECT_EQ(28u, tokens[6].byte_offset());

  EXPECT_EQ(ExprTokenType::kVolatile, tokens[7].type());
  EXPECT_EQ("volatile", tokens[7].value());
  EXPECT_EQ(34u, tokens[7].byte_offset());

  EXPECT_EQ(ExprTokenType::kRestrict, tokens[8].type());
  EXPECT_EQ("restrict", tokens[8].value());
  EXPECT_EQ(43u, tokens[8].byte_offset());
}

// Keywords
TEST(ExprTokenizer, Keywords) {
  // There must be punctuation separating alphanumeric keywords.
  ExprTokenizer t("truefalse.as_ref as true", ExprLanguage::kRust);
  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(5u, tokens.size());

  EXPECT_EQ(ExprTokenType::kName, tokens[0].type());
  EXPECT_EQ("truefalse", tokens[0].value());

  EXPECT_EQ(ExprTokenType::kDot, tokens[1].type());
  EXPECT_EQ(".", tokens[1].value());

  EXPECT_EQ(ExprTokenType::kName, tokens[2].type());
  EXPECT_EQ("as_ref", tokens[2].value());

  EXPECT_EQ(ExprTokenType::kAs, tokens[3].type());
  EXPECT_EQ("as", tokens[3].value());

  EXPECT_EQ(ExprTokenType::kTrue, tokens[4].type());
  EXPECT_EQ("true", tokens[4].value());
}

TEST(ExprTokenizer, Names) {
  // Char offsets: 0123456789012345678901
  // Token #'s:     0   12    3 4       5
  ExprTokenizer t(" name(hello] goodbye a");

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(6u, tokens.size());

  EXPECT_EQ(ExprTokenType::kName, tokens[0].type());
  EXPECT_EQ("name", tokens[0].value());
  EXPECT_EQ(1u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kLeftParen, tokens[1].type());
  EXPECT_EQ("(", tokens[1].value());
  EXPECT_EQ(5u, tokens[1].byte_offset());

  EXPECT_EQ(ExprTokenType::kName, tokens[2].type());
  EXPECT_EQ("hello", tokens[2].value());
  EXPECT_EQ(6u, tokens[2].byte_offset());

  EXPECT_EQ(ExprTokenType::kRightSquare, tokens[3].type());
  EXPECT_EQ("]", tokens[3].value());
  EXPECT_EQ(11u, tokens[3].byte_offset());

  EXPECT_EQ(ExprTokenType::kName, tokens[4].type());
  EXPECT_EQ("goodbye", tokens[4].value());
  EXPECT_EQ(13u, tokens[4].byte_offset());

  EXPECT_EQ(ExprTokenType::kName, tokens[5].type());
  EXPECT_EQ("a", tokens[5].value());
  EXPECT_EQ(21u, tokens[5].byte_offset());
}

TEST(ExprTokenizer, GetErrorContext) {
  EXPECT_EQ(
      "  foo\n"
      "  ^",
      ExprTokenizer::GetErrorContext("foo", 0));
  EXPECT_EQ(
      "  foo\n"
      "    ^",
      ExprTokenizer::GetErrorContext("foo", 2));

  // One-past-the end is allowed.
  EXPECT_EQ(
      "  foo\n"
      "     ^",
      ExprTokenizer::GetErrorContext("foo", 3));
}

TEST(ExprTokenizer, Tilde) {
  // Char offsets: 0123456789012345678901
  // Token #'s:     01    23 4    5
  ExprTokenizer t(" ~~Foo ~9 ~_bar~", ExprLanguage::kC);

  EXPECT_TRUE(t.Tokenize());
  EXPECT_FALSE(t.err().has_error()) << t.err().msg();
  const auto& tokens = t.tokens();
  ASSERT_EQ(6u, tokens.size());

  EXPECT_EQ(ExprTokenType::kTilde, tokens[0].type());
  EXPECT_EQ("~", tokens[0].value());
  EXPECT_EQ(1u, tokens[0].byte_offset());

  EXPECT_EQ(ExprTokenType::kName, tokens[1].type());
  EXPECT_EQ("~Foo", tokens[1].value());
  EXPECT_EQ(2u, tokens[1].byte_offset());

  EXPECT_EQ(ExprTokenType::kTilde, tokens[2].type());
  EXPECT_EQ("~", tokens[2].value());
  EXPECT_EQ(7u, tokens[2].byte_offset());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[3].type());
  EXPECT_EQ("9", tokens[3].value());
  EXPECT_EQ(8u, tokens[3].byte_offset());

  EXPECT_EQ(ExprTokenType::kName, tokens[4].type());
  EXPECT_EQ("~_bar", tokens[4].value());
  EXPECT_EQ(10u, tokens[4].byte_offset());

  EXPECT_EQ(ExprTokenType::kTilde, tokens[5].type());
  EXPECT_EQ("~", tokens[5].value());
  EXPECT_EQ(15u, tokens[5].byte_offset());

  // Tilde is not used in Rust.
  ExprTokenizer rust_tilde("~", ExprLanguage::kRust);
  EXPECT_FALSE(rust_tilde.Tokenize());
  EXPECT_TRUE(rust_tilde.err().has_error());

  ExprTokenizer rust_tilde_name("~", ExprLanguage::kRust);
  EXPECT_FALSE(rust_tilde_name.Tokenize());
  EXPECT_TRUE(rust_tilde_name.err().has_error());
}

// Tests that C and Rust tokens are separated.
TEST(ExprTokenizer, Language) {
  // Test that "reinterpret_cast is valid in C but not in Rust.
  ExprTokenizer c("reinterpret_cast", ExprLanguage::kC);
  EXPECT_TRUE(c.Tokenize());
  EXPECT_FALSE(c.err().has_error()) << c.err().msg();
  auto tokens = c.tokens();
  ASSERT_EQ(1u, tokens.size());
  EXPECT_EQ(ExprTokenType::kReinterpretCast, tokens[0].type());

  // In Rust it's interpreted as a regular name.
  ExprTokenizer r("reinterpret_cast", ExprLanguage::kRust);
  EXPECT_TRUE(r.Tokenize());
  EXPECT_FALSE(r.err().has_error()) << r.err().msg();
  tokens = r.tokens();
  ASSERT_EQ(1u, tokens.size());
  EXPECT_EQ(ExprTokenType::kName, tokens[0].type());

  // Currently we don't have any Rust-only tokens. When we add one we should test that it works only
  // in Rust mode.
}

TEST(ExprTokenizer, IsNameToken) {
  EXPECT_TRUE(ExprTokenizer::IsNameToken(ExprLanguage::kC, "foo"));
  EXPECT_TRUE(ExprTokenizer::IsNameToken(ExprLanguage::kC, "~foo"));
  EXPECT_TRUE(ExprTokenizer::IsNameToken(ExprLanguage::kC, "foo9"));

  EXPECT_FALSE(ExprTokenizer::IsNameToken(ExprLanguage::kC, ""));
  EXPECT_FALSE(ExprTokenizer::IsNameToken(ExprLanguage::kC, "foo~bar"));
  EXPECT_FALSE(ExprTokenizer::IsNameToken(ExprLanguage::kC, "~"));
  EXPECT_FALSE(ExprTokenizer::IsNameToken(ExprLanguage::kC, "9foo"));

  EXPECT_FALSE(ExprTokenizer::IsNameToken(ExprLanguage::kRust, "~foo"));
}

TEST(ExprTokenizer, Comments) {
  // Single-line comment to end of input.
  ExprTokenizer single_to_end("1// foo");
  EXPECT_TRUE(single_to_end.Tokenize());
  auto tokens = single_to_end.tokens();
  ASSERT_EQ(2u, tokens.size());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[0].type());
  EXPECT_EQ(ExprTokenType::kComment, tokens[1].type());
  EXPECT_EQ("// foo", tokens[1].value());

  // Single-line comment to end of line.
  ExprTokenizer single_to_line("1// foo\n2");
  EXPECT_TRUE(single_to_line.Tokenize());
  tokens = single_to_line.tokens();
  ASSERT_EQ(3u, tokens.size());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[0].type());
  EXPECT_EQ("1", tokens[0].value());
  EXPECT_EQ(ExprTokenType::kComment, tokens[1].type());
  EXPECT_EQ("// foo", tokens[1].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[2].type());
  EXPECT_EQ("2", tokens[2].value());

  // Block comment.
  ExprTokenizer block("1/* foo */2");
  EXPECT_TRUE(block.Tokenize());
  tokens = block.tokens();
  ASSERT_EQ(3u, tokens.size());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[0].type());
  EXPECT_EQ("1", tokens[0].value());
  EXPECT_EQ(ExprTokenType::kComment, tokens[1].type());
  EXPECT_EQ("/* foo */", tokens[1].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[2].type());
  EXPECT_EQ("2", tokens[2].value());

  // Block comment with no terminator. See comment in ExprTokenizer::HandleComment about why
  // we don't throw an error in this case.
  ExprTokenizer block_to_end("1/* foo ");
  EXPECT_TRUE(block_to_end.Tokenize());
  tokens = block_to_end.tokens();
  ASSERT_EQ(2u, tokens.size());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[0].type());
  EXPECT_EQ("1", tokens[0].value());
  EXPECT_EQ(ExprTokenType::kComment, tokens[1].type());
  EXPECT_EQ("/* foo ", tokens[1].value());

  // Block end comment with no beginning. This just has a special token type and is otherwise
  // ignored.
  ExprTokenizer mismatched_end("1*/2");
  EXPECT_TRUE(mismatched_end.Tokenize());
  tokens = mismatched_end.tokens();
  ASSERT_EQ(3u, tokens.size());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[0].type());
  EXPECT_EQ("1", tokens[0].value());
  EXPECT_EQ(ExprTokenType::kCommentBlockEnd, tokens[1].type());
  EXPECT_EQ("*/", tokens[1].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[2].type());
  EXPECT_EQ("2", tokens[2].value());
}

TEST(ExprTokenizer, RustLifetime) {
  // This example is from a Rust "where" clause.
  ExprTokenizer where("F: 'static + Fn(usize) -> Fut,", ExprLanguage::kRust);
  ASSERT_TRUE(where.Tokenize()) << where.err().msg();
  auto tokens = where.tokens();
  ASSERT_EQ(11u, tokens.size());

  EXPECT_EQ(ExprTokenType::kName, tokens[0].type());
  EXPECT_EQ("F", tokens[0].value());
  EXPECT_EQ(ExprTokenType::kColon, tokens[1].type());
  EXPECT_EQ(":", tokens[1].value());
  EXPECT_EQ(ExprTokenType::kRustLifetime, tokens[2].type());
  EXPECT_EQ("'static", tokens[2].value());
  EXPECT_EQ(ExprTokenType::kPlus, tokens[3].type());
  EXPECT_EQ("+", tokens[3].value());
  EXPECT_EQ(ExprTokenType::kName, tokens[4].type());
  EXPECT_EQ("Fn", tokens[4].value());
  EXPECT_EQ(ExprTokenType::kLeftParen, tokens[5].type());
  EXPECT_EQ("(", tokens[5].value());
  EXPECT_EQ(ExprTokenType::kName, tokens[6].type());
  EXPECT_EQ("usize", tokens[6].value());
  EXPECT_EQ(ExprTokenType::kRightParen, tokens[7].type());
  EXPECT_EQ(")", tokens[7].value());
  EXPECT_EQ(ExprTokenType::kArrow, tokens[8].type());
  EXPECT_EQ("->", tokens[8].value());
  EXPECT_EQ(ExprTokenType::kName, tokens[9].type());
  EXPECT_EQ("Fut", tokens[9].value());
  EXPECT_EQ(ExprTokenType::kComma, tokens[10].type());
  EXPECT_EQ(",", tokens[10].value());
}

TEST(ExprTokenizer, RustTupleAccess) {
  // In C "foo.0.1.bar" would be parsed as: Identifier(foo) Dot Float(0.1) Dot Identifier(bar)
  // But in Rust, the leading dot puts us into "tuple accessor" mode where the following thing can
  // NOT be a floating-point number and this is parsed as a sequence of separate tokens.
  ExprTokenizer tuple_access("foo.0.1.bar", ExprLanguage::kRust);
  ASSERT_TRUE(tuple_access.Tokenize()) << tuple_access.err().msg();
  auto tokens = tuple_access.tokens();
  ASSERT_EQ(7u, tokens.size());

  EXPECT_EQ(ExprTokenType::kName, tokens[0].type());
  EXPECT_EQ("foo", tokens[0].value());
  EXPECT_EQ(ExprTokenType::kDot, tokens[1].type());
  EXPECT_EQ(".", tokens[1].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[2].type());
  EXPECT_EQ("0", tokens[2].value());
  EXPECT_EQ(ExprTokenType::kDot, tokens[3].type());
  EXPECT_EQ(".", tokens[3].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[4].type());
  EXPECT_EQ("1", tokens[4].value());
  EXPECT_EQ(ExprTokenType::kDot, tokens[5].type());
  EXPECT_EQ(".", tokens[5].value());
  EXPECT_EQ(ExprTokenType::kName, tokens[6].type());
  EXPECT_EQ("bar", tokens[6].value());

  // This should parse the same with spaces.
  ExprTokenizer spaced_access("foo . 0.1 .bar", ExprLanguage::kRust);
  ASSERT_TRUE(spaced_access.Tokenize()) << spaced_access.err().msg();
  tokens = spaced_access.tokens();
  ASSERT_EQ(7u, tokens.size());

  EXPECT_EQ(ExprTokenType::kInteger, tokens[2].type());
  EXPECT_EQ("0", tokens[2].value());
  EXPECT_EQ(ExprTokenType::kDot, tokens[3].type());
  EXPECT_EQ(".", tokens[3].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[4].type());
  EXPECT_EQ("1", tokens[4].value());

  // Test with a comment in the middle of the tokens.
  ExprTokenizer commented_access("foo . /* yikes */ 0.1 .bar", ExprLanguage::kRust);
  ASSERT_TRUE(commented_access.Tokenize()) << commented_access.err().msg();
  tokens = commented_access.tokens();
  ASSERT_EQ(8u, tokens.size());

  EXPECT_EQ(ExprTokenType::kComment, tokens[2].type());
  EXPECT_EQ("/* yikes */", tokens[2].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[3].type());
  EXPECT_EQ("0", tokens[3].value());
  EXPECT_EQ(ExprTokenType::kDot, tokens[4].type());
  EXPECT_EQ(".", tokens[4].value());
  EXPECT_EQ(ExprTokenType::kInteger, tokens[5].type());
  EXPECT_EQ("1", tokens[5].value());
}

}  // namespace zxdb
