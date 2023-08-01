// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <filesystem>
#include <fstream>
#include <string>
#include <string_view>

#include <gtest/gtest.h>

#include "src/developer/debug/zxdb/common/host_util.h"
#include "tools/symbolizer/log_parser.h"
#include "tools/symbolizer/symbolizer_impl.h"

namespace {

const std::filesystem::path kSelfPath = zxdb::GetSelfPath();
const std::filesystem::path kFuchsiaSrcDir =
    kSelfPath.parent_path().parent_path().parent_path().parent_path();
const std::filesystem::path kSymbolsDir =
    kFuchsiaSrcDir / "prebuilt" / "test_data" / "symbolizer" / "symbols";
const std::filesystem::path kTestCasesDir = kFuchsiaSrcDir / "tools" / "symbolizer" / "test_cases";

class TestCase : public testing::Test {
 public:
  explicit TestCase(const std::string& name) : name_(name) {}
  void TestBody() override {
    symbolizer::CommandLineOptions options;
    options.build_id_dirs.push_back(kSymbolsDir);
    options.prettify_backtrace = true;

    std::stringstream output;
    symbolizer::SymbolizerImpl symbolizer(options);

    std::ifstream input(kTestCasesDir / (name_ + ".in"));
    std::ifstream expected_output(kTestCasesDir / (name_ + ".out"));
    symbolizer::LogParser parser(input, output, &symbolizer);

    while (parser.ProcessNextLine()) {
      std::string got;
      while (std::getline(output, got)) {
        std::string expected;
        EXPECT_TRUE(std::getline(expected_output, expected));
        EXPECT_EQ(got, expected);
      }
      // Reset the output buffer.
      output.clear();
      output.str("");
    }
  }

 private:
  std::string name_;
};

void SetupTests() {
  for (const auto& entry : std::filesystem::directory_iterator(kTestCasesDir)) {
    if (entry.path().extension() == ".in") {
      std::string name = entry.path().stem();
      ::testing::RegisterTest("E2ETest", name.c_str(), nullptr, nullptr, __FILE__, __LINE__,
                              [name]() { return new TestCase(name); });
    }
  }
}

}  // namespace

int main(int argc, char** argv) {
  SetupTests();
  ::testing::InitGoogleTest(&argc, argv);
  return RUN_ALL_TESTS();
}
