// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cstdarg>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <ios>
#include <iostream>
#include <memory>
#include <sstream>
#include <string>
#include <utility>
#include <vector>

#include "tools/fidl/fidlc/src/compiler.h"
#include "tools/fidl/fidlc/src/experimental_flags.h"
#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/index_json_generator.h"
#include "tools/fidl/fidlc/src/json_generator.h"
#include "tools/fidl/fidlc/src/json_schema.h"
#include "tools/fidl/fidlc/src/lexer.h"
#include "tools/fidl/fidlc/src/parser.h"
#include "tools/fidl/fidlc/src/source_manager.h"
#include "tools/fidl/fidlc/src/versioning_types.h"
#include "tools/fidl/fidlc/src/virtual_source_file.h"

namespace {

void Usage() {
  std::cout << R"USAGE(The FIDL compiler

Usage: fidlc [--json JSON_PATH]
             [--available PLATFORM:VERSION]
             [--versioned PLATFORM[:VERSION]]
             [--name LIBRARY_NAME]
             [--experimental FLAG_NAME]
             [--werror]
             [--format=[text|json]]
             [--json-schema]
             [--depfile DEPFILE_PATH]
             [--files [FIDL_FILE...]...]
             [--help]

All of the arguments can also be provided via a response file, denoted as
`@responsefile`. The contents of the file at `responsefile` will be interpreted
as a whitespace-delimited list of arguments. Response files cannot be nested.

See <https://fuchsia.dev/fuchsia-src/development/languages/fidl/reference/compiler>
for more information.

Options:

 * `--json JSON_PATH`. If present, this flag instructs `fidlc` to output the
   library's intermediate representation at the given path. The intermediate
   representation is JSON that conforms to the schema available via --json-schema.
   The intermediate representation is used as input to the various backends.

 * `--available PLATFORM:VERSION`. If present, this flag instructs `fidlc` to compile
    libraries versioned under PLATFORM at VERSION, based on `@available` attributes.
    PLATFORM corresponds to a library's `@available(platform="PLATFORM")` attribute,
    or to the library name's first component if the `platform` argument is omitted.

 * `--versioned PLATFORM[:VERSION]`. If present, this flag instructs `fidlc` to
   validate that the main library being compiled is versioned under PLATFORM.
   If VERSION is provided, also validates that the library is added at VERSION.
   The library's platform is determined as follows:
    * If there are no `@available` attributes, the platform is "unversioned".
    * The platform can be explicit with `@available(platform="PLATFORM")`.
    * Otherwise, the platform is the first component of the library name.

 * `--name LIBRARY_NAME`. If present, this flag instructs `fidlc` to validate
   that the main library being compiled has the given name. This flag is useful
   to cross-check between the library's declaration in a build system and the
   actual contents of the library.

 * `--experimental FLAG_NAME`. If present, this flag enables an experimental
    feature of fidlc.

 * `--depfile DEPFILE_PATH`. Path of depfile generated by `fidlc`. This depfile is
   used to get correct incremental compilation rules. This file is populated by fidlc
   as Line1: out1: in1 in2 in3, Line2: out2: in1 in2 in3 ... Where out[1-2] are all the
   outputs generated by fidlc and in[1-3] are the files read. The input files are
   what are passed by --files. Output files are those generated by fidlc.

 * `--files [FIDL_FILE...]...`. Each `--files [FIDL_FILE...]` chunk of arguments
   describes a library, all of which must share the same top-level library name
   declaration. Libraries must be presented in dependency order, with later
   libraries able to use declarations from preceding libraries but not vice versa.
   Output is only generated for the final library, not for each of its dependencies.

 * `--json-schema`. If present, this flag instructs `fidlc` to output the
   JSON schema of the intermediate representation.

 * `--format=[text|json]`. If present, this flag sets the output mode of `fidlc`.
    This specifies whether to output errors and warnings, if compilation fails, in
    plain text (the default), or as JSON.

 * `--werror`. Treats warnings as errors.

 * `--help`. Prints this help, and exit immediately.
)USAGE";
}

void PrintJsonSchema() { std::cout << JsonSchema::schema() << '\n'; }

[[noreturn]] void FailWithUsage(const char* message, ...) {
  va_list args;
  va_start(args, message);
  vfprintf(stderr, message, args);
  va_end(args);
  Usage();
  exit(1);
}

[[noreturn]] void Fail(const char* message, ...) {
  va_list args;
  va_start(args, message);
  vfprintf(stderr, message, args);
  va_end(args);
  exit(1);
}

void MakeParentDirectory(const std::string& filename) {
  std::string::size_type slash = 0;

  for (;;) {
    slash = filename.find('/', slash);
    if (slash == std::string::npos) {
      return;
    }

    std::string path = filename.substr(0, slash);
    ++slash;
    if (path.empty()) {
      // Skip creating "/".
      continue;
    }

    if (mkdir(path.c_str(), 0755) != 0 && errno != EEXIST) {
      Fail("Could not create directory %s for output file %s: error %s\n", path.c_str(),
           filename.c_str(), strerror(errno));
    }
  }
}

std::fstream Open(const std::string& filename, std::ios::openmode mode) {
  if ((mode & std::ios::out) != 0) {
    MakeParentDirectory(filename);
  }

  std::fstream stream;
  stream.open(filename, mode);
  if (!stream.is_open()) {
    Fail("Could not open file: %s\n", filename.c_str());
  }
  return stream;
}

class Arguments {
 public:
  virtual ~Arguments() = default;

  virtual std::string Claim() = 0;
  virtual bool Remaining() const = 0;
};

class ResponseFileArguments : public Arguments {
 public:
  explicit ResponseFileArguments(std::string_view filename)
      : file_(Open(std::string(filename), std::ios::in)) {
    ConsumeWhitespace();
  }

  std::string Claim() override {
    std::string argument;
    while (Remaining() && !IsWhitespace()) {
      argument.push_back(static_cast<char>(file_.get()));
    }
    ConsumeWhitespace();
    return argument;
  }

  bool Remaining() const override { return !file_.eof(); }

 private:
  bool IsWhitespace() {
    switch (file_.peek()) {
      case ' ':
      case '\n':
      case '\r':
      case '\t':
        return true;
      default:
        return false;
    }
  }

  void ConsumeWhitespace() {
    while (Remaining() && IsWhitespace()) {
      file_.get();
    }
  }

  std::fstream file_;
};

class ArgvArguments : public Arguments {
 public:
  ArgvArguments(int count, char** arguments)
      : count_(count), arguments_(const_cast<const char**>(arguments)) {}

  std::string Claim() override {
    if (response_file_) {
      if (response_file_->Remaining()) {
        return response_file_->Claim();
      }
      response_file_.reset();
    }
    if (count_ < 1) {
      FailWithUsage("Missing part of an argument\n");
    }
    std::string argument = arguments_[0];
    --count_;
    ++arguments_;
    if (argument.empty() || argument[0] != '@') {
      return argument;
    }

    std::string_view rsp_file = argument.c_str() + 1;
    response_file_ = std::make_unique<ResponseFileArguments>(rsp_file);
    return Claim();
  }

  bool Remaining() const override {
    if (response_file_.get() && response_file_->Remaining())
      return true;

    return count_ > 0;
  }

 private:
  int count_;
  const char** arguments_;
  std::unique_ptr<ResponseFileArguments> response_file_;
};

std::pair<fidlc::Platform, std::optional<fidlc::Version>> ParsePlatformAndVersion(
    const std::string& arg) {
  auto colon_idx = arg.find(':');
  auto platform_str = arg.substr(0, colon_idx);
  auto platform = fidlc::Platform::Parse(platform_str);
  if (!platform.has_value())
    FailWithUsage("Invalid platform name `%s`\n", platform_str.c_str());
  std::optional<fidlc::Version> version;
  if (colon_idx != std::string::npos) {
    auto version_str = arg.substr(colon_idx + 1);
    version = fidlc::Version::Parse(version_str);
    if (!version.has_value())
      FailWithUsage("Invalid version `%s`\n", version_str.c_str());
    if (platform->is_unversioned())
      FailWithUsage("Selecting a version for '%s' is not allowed\n", platform_str.c_str());
  }
  return {std::move(platform).value(), version};
}

enum struct Behavior {
  kJSON,
  kIndex,
};

bool Parse(const fidlc::SourceFile& source_file, fidlc::Reporter* reporter,
           fidlc::Compiler* compiler, fidlc::ExperimentalFlagSet experimental_flags) {
  fidlc::Lexer lexer(source_file, reporter);
  fidlc::Parser parser(&lexer, reporter, experimental_flags);
  auto ast = parser.Parse();
  if (!parser.Success()) {
    return false;
  }
  if (!compiler->ConsumeFile(std::move(ast))) {
    return false;
  }
  return true;
}

void Write(const std::ostringstream& output_stream, const std::string& file_path) {
  std::string contents = output_stream.str();
  struct stat st;
  if (!stat(file_path.c_str(), &st)) {
    // File exists.
    std::streamsize current_size = st.st_size;
    if (current_size == static_cast<std::streamsize>(contents.size())) {
      // Lengths match.
      std::string current_contents(current_size, '\0');
      std::fstream current_file = Open(file_path, std::ios::in);
      current_file.read(current_contents.data(), current_size);
      if (current_contents == contents) {
        // Contents match, no need to write the file.
        return;
      }
    }
  }
  std::fstream file = Open(file_path, std::ios::out);
  file << contents;
  file.flush();
  if (file.fail()) {
    Fail("Failed to flush output to file: %s\n", file_path.c_str());
  }
}

}  // namespace

int compile(fidlc::Reporter* reporter, const std::optional<fidlc::Platform>& expected_platform,
            std::optional<fidlc::Version> expected_version_added,
            const std::optional<std::string>& expected_library_name,
            const std::string& dep_file_path, const std::vector<std::string>& source_list,
            const std::vector<std::pair<Behavior, std::string>>& outputs,
            const std::vector<fidlc::SourceManager>& source_managers,
            fidlc::VirtualSourceFile* virtual_file,
            const fidlc::VersionSelection* version_selection,
            fidlc::ExperimentalFlagSet experimental_flags) {
  fidlc::Libraries all_libraries(reporter, virtual_file);
  for (const auto& source_manager : source_managers) {
    if (source_manager.sources().empty()) {
      continue;
    }
    fidlc::Compiler compiler(&all_libraries, version_selection, fidlc::Sha256MethodHasher,
                             experimental_flags);
    for (const auto& source_file : source_manager.sources()) {
      if (!Parse(*source_file, reporter, &compiler, experimental_flags)) {
        return 1;
      }
    }
    if (!compiler.Compile()) {
      return 1;
    }
  }
  if (all_libraries.Empty()) {
    Fail("No library was produced.\n");
  }

  auto unused_libraries = all_libraries.Unused();
  // TODO(https://fxbug.dev/42172334): Remove this once all GN rules only include zx
  // sources when the zx library is actually used.
  if (auto zx_library = all_libraries.Lookup({"zx"})) {
    if (auto iter = unused_libraries.find(zx_library); iter != unused_libraries.end()) {
      // Remove from unused_libraries to avoid reporting an error below.
      unused_libraries.erase(iter);
      // Remove from all_libraries to avoid emitting it in the JSON IR
      // "library_dependencies" property.
      all_libraries.Remove(zx_library);
    }
  }
  if (!unused_libraries.empty()) {
    std::string library_names;
    bool first = true;
    for (auto library : unused_libraries) {
      if (first) {
        first = false;
      } else {
        library_names.append(", ");
      }
      library_names.append(library->name);
    }
    library_names.append("\n");
    Fail("Unused libraries provided via --files: %s", library_names.c_str());
  }

  auto compilation = all_libraries.Filter(version_selection);
  auto library_name = std::string(compilation->library_name);

  if (expected_library_name.has_value() && library_name != *expected_library_name) {
    Fail("Found `library %s;`, but expected `library %s;` based on the --name flag\n",
         library_name.c_str(), expected_library_name->c_str());
  }

  if (expected_platform.has_value()) {
    auto& actual = *compilation->platform;
    auto& expected = expected_platform.value();
    if (actual != expected) {
      std::stringstream hint;
      auto version = expected_version_added.value_or(fidlc::Version::kHead).name();
      if (expected.is_unversioned()) {
        hint << "try removing @available attributes";
      } else if (actual.is_unversioned()) {
        if (fidlc::FirstComponent(compilation->library_name) == expected.name()) {
          hint << "try adding `@available(added=" << version << ")` on the `library` declaration";
        } else {
          hint << "try adding `@available(platform=\"" << expected.name() << "\", added=" << version
               << ")` on the `library` declaration";
        }
      } else {
        hint << "try changing the library name to start with '" << expected.name()
             << ".', or use `@available(platform=\"" << expected.name() << "\", added=" << version
             << ")`";
      }
      Fail(
          "Library '%s' is versioned under platform '%s', but expected platform '%s' based on the "
          "--versioned flag; %s\n",
          library_name.c_str(), actual.name().c_str(), expected.name().c_str(), hint.str().c_str());
    }
  }
  if (expected_version_added.has_value()) {
    auto& actual = compilation->version_added;
    auto& expected = expected_version_added.value();
    if (actual != expected) {
      Fail(
          "Library '%s' is marked @available(added=%s), but expected @available(added=%s) based on "
          "the --versioned flag\n",
          library_name.c_str(), actual.ToString().c_str(), expected.ToString().c_str());
    }
  }

  // Write depfile, with format:
  // output1 : inputA inputB inputC
  // output2 : inputA inputB inputC
  // ...
  if (!dep_file_path.empty()) {
    std::ostringstream dep_file_contents;
    for (auto& output : outputs) {
      auto& file_path = output.second;
      dep_file_contents << file_path << " : ";
      for (auto& input_path : source_list) {
        dep_file_contents << input_path << ' ';
      }
      dep_file_contents << '\n';
    }

    Write(dep_file_contents, dep_file_path);
  }

  // We recompile dependencies, and only emit output for the target library.
  for (auto& output : outputs) {
    auto& behavior = output.first;
    auto& file_path = output.second;

    switch (behavior) {
      case Behavior::kJSON: {
        fidlc::JSONGenerator generator(compilation.get(), experimental_flags);
        Write(generator.Produce(), file_path);
        break;
      }
      case Behavior::kIndex: {
        fidlc::IndexJSONGenerator generator(compilation.get());
        Write(generator.Produce(), file_path);
        break;
      }
    }
  }
  return 0;
}

int main(int argc, char* argv[]) {
  auto args = std::make_unique<ArgvArguments>(argc, argv);
  // Parse the program name.
  args->Claim();
  if (!args->Remaining()) {
    Usage();
    exit(0);
  }

  std::optional<fidlc::Platform> expected_platform;
  std::optional<fidlc::Version> expected_version_added;
  std::optional<std::string> expected_library_name;

  std::string dep_file_path;
  std::string json_path;

  bool warnings_as_errors = false;
  std::string format = "text";
  std::vector<std::pair<Behavior, std::string>> outputs;
  fidlc::VersionSelection version_selection;
  fidlc::ExperimentalFlagSet experimental_flags;
  while (args->Remaining()) {
    // Try to parse an output type.
    std::string flag = args->Claim();
    if (flag == "--help") {
      Usage();
      exit(0);
    } else if (flag == "--json-schema") {
      PrintJsonSchema();
      exit(0);
    } else if (flag == "--werror") {
      warnings_as_errors = true;
    } else if (flag.rfind("--format") == 0) {
      const auto equals = flag.rfind('=');
      if (equals == std::string::npos) {
        FailWithUsage("Unknown value for flag `format`\n");
      }
      const auto format_value = flag.substr(equals + 1, flag.length());
      if (format_value != "text" && format_value != "json") {
        FailWithUsage("Unknown value `%s` for flag `format`\n", format_value.c_str());
      }
      format = format_value;
    } else if (flag == "--json") {
      std::string path = args->Claim();
      json_path = path;
      outputs.emplace_back(Behavior::kJSON, path);
    } else if (flag == "--available") {
      auto [platform, version] = ParsePlatformAndVersion(args->Claim());
      if (!version.has_value())
        FailWithUsage("Missing version for flag `available`\n");
      version_selection.Insert(platform, version.value());
    } else if (flag == "--versioned") {
      std::tie(expected_platform, expected_version_added) = ParsePlatformAndVersion(args->Claim());
    } else if (flag == "--name") {
      expected_library_name = args->Claim();
    } else if (flag == "--experimental") {
      std::string string = args->Claim();
      auto it = fidlc::kAllExperimentalFlags.find(string);
      if (it == fidlc::kAllExperimentalFlags.end()) {
        FailWithUsage("Unknown experimental flag %s\n", string.c_str());
      }
      experimental_flags.Enable(it->second);
    } else if (flag == "--depfile") {
      dep_file_path = args->Claim();
    } else if (flag == "--files") {
      // Start parsing filenames.
      break;
    } else {
      FailWithUsage("Unknown argument: %s\n", flag.c_str());
    }
  }

  // Prepare source files.
  std::vector<fidlc::SourceManager> source_managers;
  std::vector<std::string> source_list;
  source_managers.emplace_back();
  while (args->Remaining()) {
    std::string arg = args->Claim();
    if (arg == "--files") {
      source_managers.emplace_back();
    } else {
      const char* reason;
      if (!source_managers.back().CreateSource(arg, &reason)) {
        Fail("Couldn't read in source data from %s: %s\n", arg.c_str(), reason);
      }
      source_list.emplace_back(std::move(arg));
    }
  }

  // Ready. Set. Go.
  if (experimental_flags.IsEnabled(fidlc::ExperimentalFlag::kOutputIndexJson)) {
    auto path = std::filesystem::path(json_path).replace_extension("index.json");
    outputs.emplace_back(Behavior::kIndex, path);
  }
  fidlc::Reporter reporter;
  fidlc::VirtualSourceFile virtual_file("generated");
  reporter.set_warnings_as_errors(warnings_as_errors);
  auto status = compile(&reporter, expected_platform, expected_version_added, expected_library_name,
                        dep_file_path, source_list, outputs, source_managers, &virtual_file,
                        &version_selection, experimental_flags);
  if (format == "json") {
    reporter.PrintReportsJson();
  } else {
    bool enable_color = !std::getenv("NO_COLOR") && isatty(fileno(stderr));
    reporter.PrintReports(enable_color);
  }
  return status;
}
