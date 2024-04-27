# Debug Agent Test Data

This directory contains all test applications and libraries developed for testing and debugging
the debug agent. This means utilities for things like spawning processes, keeping threads looping
and having a .so that is also linked to generated binary.

Every new functionality that is not meant to be consumed by the end users but rather by the
developers of zxdb should put that code in here.

## Instructions

There are some constructs in this directory:

- Debug .so: A shared library that gets packaged that can be dynamically loaded through the
  "package" routing (/pkg/lib if started as a component). It's mostly loaded by the breakpoint tests
  to have a shared module with the binary they're debugging.
- Test binaries: These are actual binaries that get breakpoint tests run as a sub-process so that
  they can load breakpoints on them.
  See src/developer/debug/debug_agent/integration_tests/breakpoint_test.cc for an example.
- Test utilities: These are various one-off programs that are meant to be run manually as a
  component ($ run this_component.cm... See limbo caller as an example).

## Deploying

This suite of programs and helpers are mostly packed as part of the "debug_agent_helpers" package,
which is defined in src/developer/debug/debug_agent/BUILD.gn. That package does all the job of
adding the correct dependencies and doing all the packaging/meta files managing.

### How to add a new test executable

The process to add a new executable is to:

1. Add a new executble to src/developer/debug/debug_agent/test_data/BUILD.gn
2. Add it as a dependency to the `:helpers_executable` group within the test_data BUILD.gn.
3. In the debug agent BUILD.gn (src/developer/debug/debug_agent/BUILD.gn), you need to specify that
   the `debug_agent_helpers package exports this new executable. For that, look for the `binaries`
   array and add the exported name of the executable in (1) and add it there.
4. If there is a .cml file (meant to be run as a component), remember to also add the meta file
   translation in there. See `limbo_caller` or `test_suite` as an example of a test executable that
   also can get called as a component.
5. If your .cml file requires the fuchsia.kernel.RootJob service, update the
   policy file at //src/security/policy/root_job_allowlist_eng.txt file,
   otherwise it will fail to launch at runtime with a mysterious error like the
   one below (and nothing in the system log!):

     fuchsia-pkg://fuchsia.com/...#meta/your.cm: failed to create component (8)

## Test Suite

The "Debug Agent Test Suite" is a more elaborate program that is used to create more complicated
scenerios, such as adding several watchpoints on another process, send multiple channel calls, etc.

The suite has CLI instructions about to execute the different test cases. Each test case is
documented so you can refer to the source to see what each test case is supposed to do.

The test suite has a set of helpers (test_suite_helpers.h) that facilitate the creation of test
cases (hence the name test suite). Normally if more intricate examples need to be written to try out
a behaviour, it's very probable that adding it to the test suite is the easiest way to go about it
developing and deploying it, as adding a new test case is very easy (see the last part of
test_suite.cc).

The test suite can be run directly: `ffx component explore /core/debug_agent -c /pkg/bin/test_suite`.
