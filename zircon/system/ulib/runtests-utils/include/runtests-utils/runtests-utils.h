// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Helper functions for running test binaries and recording their results.

#ifndef RUNTESTS_UTILS_RUNTESTS_UTILS_H_
#define RUNTESTS_UTILS_RUNTESTS_UTILS_H_

#include <inttypes.h>
#include <lib/debugdata/datasink.h>
#include <lib/zircon-internal/fnv1hash.h>
#include <zircon/types.h>

#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/string.h>
#include <fbl/vector.h>

namespace runtests {

// Status of launching a test subprocess.
enum LaunchStatus {
  SUCCESS,
  FAILED_TO_LAUNCH,
  FAILED_TO_WAIT,
  FAILED_DURING_IO,
  FAILED_TO_RETURN_CODE,
  FAILED_NONZERO_RETURN_CODE,
  FAILED_COLLECTING_SINK_DATA,
  FAILED_UNKNOWN,
  TIMED_OUT,
};

// Represents the result of a single test run.
struct Result {
  fbl::String name;
  LaunchStatus launch_status;
  int64_t return_code;  // Only valid if launch_status == SUCCESS or FAILED_NONZERO_RETURN_CODE.
  std::unordered_map<std::string, std::vector<debugdata::DumpFile>>
      data_sinks;  // Mapping from data sink name to list of files.
  int64_t duration_milliseconds;

  // Constructor really only needed until we have C++14, which will allow call-sites to use
  // aggregate initializer syntax.
  Result(const char* name_arg, LaunchStatus launch_status_arg, int64_t return_code_arg,
         int64_t duration_milliseconds_arg)
      : name(name_arg),
        launch_status(launch_status_arg),
        return_code(return_code_arg),
        duration_milliseconds(duration_milliseconds_arg) {}
};

// Invokes a test binary and writes its output to a file.
//
// |argv| is a null-terminated array of argument strings passed to the test
//   program.
// |output_dir| is the name of a directory where debug data
//   will be written. If nullptr, no debug data will be collected.
// |test_name| is used to populate Result and in log messages.
// |timeout_msec| is a number of milliseconds to wait for the test. If 0,
//   will wait indefinitely.
// |realm_label| is the realm label to be applied to calls to underlying test executor.
//   The test will run in that realm and its run artifacts will not be cleaned up automatically
//   after the completion of the test.
std::unique_ptr<Result> RunTest(const char* argv[], const char* output_dir, const char* test_name,
                                int64_t timeout_msec, const char* realm_label);

// A means of measuring how long it takes to run tests.
class Stopwatch {
 public:
  virtual ~Stopwatch() = default;

  // Starts timing.
  virtual void Start() = 0;

  // Returns the elapsed time in milliseconds since invoking Start(), or else
  // since initialization if Start() has not yet been called.
  virtual int64_t DurationInMsecs() = 0;
};

// Splits |input| by ',' and appends the results onto |output|.
// Empty strings are not put into output.
void ParseTestNames(std::string_view input, fbl::Vector<fbl::String>* output);

// Returns true iff |name| is equal to one of strings in |whitelist|.
bool IsInWhitelist(std::string_view name, const fbl::Vector<fbl::String>& whitelist);

// Ensures |dir_name| exists by creating it and its parents if it doesn't.
// Returns 0 on success, else an error code compatible with errno.
int MkDirAll(std::string_view dir_name);

// Returns "|parent|/|child|". Unless child is absolute, in which case it returns |child|.
//
// |parent| is the parent path.
// |child| is the child path.
fbl::String JoinPath(std::string_view parent, std::string_view child);

// Writes a JSON summary of test results given a sequence of results.
//
// |results| are the run results to summarize.
// |syslog_path| is the file path where syslogs are written.
// |summary_json| is the file stream to write the JSON summary to.
//
// Returns 0 on success, else an error code compatible with errno.
int WriteSummaryJSON(const fbl::Vector<std::unique_ptr<Result>>& results,
                     std::string_view syslog_path, FILE* summary_json);

// Resolves a set of globs.
//
// |globs| is an array of glob patterns.
// |resolved| will hold the results of resolving |globs|.
//
// Returns 0 on success, else an error code from glob.h.
int ResolveGlobs(const fbl::Vector<fbl::String>& globs, fbl::Vector<fbl::String>* resolved);

// Executes all specified binaries.
//
// |test_paths| are the paths of the binaries to execute.
// |test_args| are arguments passed into the binaries under test.
// |repeat| runs the entire test suite this many times. The entire suite is repeated rather than
//   each test individually so that:
//   a) any flakes due to the sequencing of tests can be reproduced
//   b) we can get an idea of global flake rates without waiting for all runs to complete
// |timeout_msec| is the number of milliseconds to wait for a test before considering it failed.
//   ignored if 0.
// |output_dir| is the directory for all the tests' debug data sinks. May be nullptr, in
//   which case debug data will not be captured.
// |realm_label| is the realm label to be applied to calls to underlying test executor.
//   The test will run in that realm and its run artifacts will not be cleaned up automatically
//   after the completion of the test.
// |failed_count| is an output parameter which will be set to the number of test
//   binaries that failed.
// |results| is an output parameter to which run results will be appended.
//
// Returns false if any test binary failed, true otherwise.
bool RunTests(const fbl::Vector<fbl::String>& test_paths, const fbl::Vector<fbl::String>& test_args,
              int repeat, int64_t timeout_msec, const char* output_dir, const char* realm_label,
              int* failed_count, fbl::Vector<std::unique_ptr<Result>>* results);

// Expands |dir_globs| and searches those directories for files.
//
// |dir_globs| are expanded as globs to directory names, and then those directories are searched.
// |ignore_dir_name| iff not null, any directory with this basename will not be searched.
// |basename_whitelist| iff not empty, only files that have a basename in this whitelist will be
//    returned.
// |test_paths| is an output parameter to which absolute file paths will be appended.
//
// Returns 0 on success, else an error code compatible with errno.
int DiscoverTestsInDirGlobs(const fbl::Vector<fbl::String>& dir_globs, const char* ignore_dir_name,
                            const fbl::Vector<fbl::String>& basename_whitelist,
                            fbl::Vector<fbl::String>* test_paths);

// Discovers and runs tests based on command line arguments.
//
// |argc|: length of |argv|.
// |argv|: see //system/ulib/runtests-utils/discover-and-run-tests.cpp,
//    specifically the 'Usage()' function, for documentation.
// |default_test_dirs|: directories in which to look for tests if no test
//    directory globs are specified.
// |stopwatch|: for timing how long all tests took to run.
// |syslog_file_name|: if an output directory is specified ("-o"), syslog ouput
//    will be written to a file under that directory and this name.
//
// Returns EXIT_SUCCESS if all tests passed; else, returns EXIT_FAILURE.
int DiscoverAndRunTests(int argc, const char* const* argv,
                        const fbl::Vector<fbl::String>& default_test_dirs, Stopwatch* stopwatch,
                        std::string_view syslog_file_name);

// Returns true iff |s| is a fuchsia-pkg URI.
bool IsFuchsiaPkgURI(const char* s);

}  // namespace runtests

#endif  // RUNTESTS_UTILS_RUNTESTS_UTILS_H_
