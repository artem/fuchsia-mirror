# Contributing Tests to CTF

This guide explains how to [add](#add-a-test) and [remove](#remove-a-test) [CTF]
tests.

## How to add a test to CTF {#add-a-test}

Please read the CTF [test requirements](#test-requirements) before getting
started. The most notable requirement is that a test must use a
[test realm factory] before this guide can be followed.

A test is included in CTF if it is in the group at [//sdk/ctf/tests:tests].
Before adding the test to this group, it's build rules must be rewritten to use
CTF versions. This is an unfortunate but necessary step that will go away soon.

To add a test to this group, perform each of the steps below in the BUILD.gn
file containing the test's `fuchsia_test_component` target.

### 1. Import CTF build rules

```gn
import("//sdk/ctf/build/ctf.gni")
```

### 2. Use CTF GN templates {#the-ctf-templates}

Replace the test's `fuchsia_package` declaration and each of its direct
dependencies' declarations with equivalent `ctf_*` GN targets. For example,
`fuchsia_package` should be changed to `ctf_fuchsia_package`. This only needs to
be done for the package's dependencies because they are versioned with the test
in CTF. It does not need to be done to any parent packages that subpackage the
test. For a list of all available CTF templates, see [//sdk/ctf/build].

* {Before (Rust)}

  ```gn
  rustc_test("bin") {
    edition = "2018"
    name = "fuchsia_example_test"
    source_root = "src/main.rs"
    sources = [ "src/main.rs" ]
    deps = [ ... ]
  }

  fuchsia_component("fuchsia-example-test-component") {
    testonly = true
    component_name = "fuchsia-example-test"
    manifest = "meta/fuchsia-example-test.cml"
    deps = [ ":bin" ]
  }

  fuchsia_package("fuchsia-example-tests") {
    testonly = true
    package_name = "fuchsia-example-tests"
    deps = [ ":fuchsia-example-test-component" ]
  }
  ```

* {After (Rust)}

  ```gn
  ctf_rustc_test("bin") {
    edition = "2018"
    name = "fuchsia_example_test"
    source_root = "src/main.rs"
    sources = [ "src/main.rs" ]
    deps = [ ... ]
  }

  ctf_fuchsia_component("fuchsia-example-test-component") {
    testonly = true
    component_name = "fuchsia-example-test"
    manifest = "meta/fuchsia-example-test.cml"
    deps = [ ":bin" ]
  }

  ctf_fuchsia_package("fuchsia-example-tests") {
    testonly = true
    package_name = "fuchsia-example-tests"
    deps = [ ":fuchsia-example-test-component" ]
  }
  ```

* {Before (C++)}

  ```gn
  executable("bin") {
    testonly = true
    name = "fuchsia_example_test"
    sources = [ "src/main.cc" ]
    deps = [ ... ]
  }

  fuchsia_component("fuchsia-example-test-component") {
    testonly = true
    component_name = "fuchsia-example-test"
    manifest = "meta/fuchsia-example-test.cml"
    deps = [ ":bin" ]
  }

  fuchsia_package("fuchsia-example-tests") {
    testonly = true
    package_name = "fuchsia-example-tests"
    deps = [ ":fuchsia-example-test-component" ]
  }
  ```

* {After (C++)}

  ```gn
  ctf_executable("bin") {
    testonly = true
    name = "fuchsia_example_test"
    sources = [ "src/main.cc" ]
    deps = [ ... ]
  }

  ctf_fuchsia_component("fuchsia-example-test-component") {
    testonly = true
    component_name = "fuchsia-example-test"
    manifest = "meta/fuchsia-example-test.cml"
    deps = [ ":bin" ]
  }

  ctf_fuchsia_package("fuchsia-example-tests") {
    testonly = true
    package_name = "fuchsia-example-tests"
    deps = [ ":fuchsia-example-test-component" ]
  }
  ```

### 3. Add the test release archive to the build graph {#the-test-archive}

`ctf_fuchsia_package` generates a `${target_name}_archive` target that produces
a FAR archive of the test. This archive is what gets released in CTF. Add a
`group("tests")` target to the BUILD.gn file if it doesn't exist, then add the
archive as a dependency:

```gn
group("tests") {
  testonly = true
  deps = [
    ":fuchsia-example-tests_archive",
    ...
  ]
}
```

You successfully added a `group("tests")` to the `BUILD.gn` file. You are now
ready to add that group to [//sdk/ctf/tests:tests].

### 4. Add a GN template to build the prebuilt CTF test {#the-gn-template}

To teach CTF how to build the test's prebuilt package into a runnable target,
create a GN template that generates a package identical to the
`fuchsia_test_package` used to run the latest version of the test. For example,
if the test root component and test package look like this:

```gn
fuchsia_test_component("fuchsia-example-test-root") {
  testonly = true
  manifest = "meta/fuchsia-example-test-root.cml"
  test_type = "ctf"
}

fuchsia_test_package("fuchsia-example-tests-latest") {
  test_components = [ ":fuchsia-example-test-root" ]
  subpackages = [
    ":fuchsia-example-tests", # latest version of the test suite.
    ":fuchsia-example-test-realm-factory",
  ]
  deps = [ ":fuchsia-example-test-helper" ]
}
```

Then add this template to [//sdk/ctf/build/generate_ctf_tests.gni]

```gn
template("generate_fuchsia-example-tests") {
  forward_variables_from(invoker, [ "test_info" ])
  fuchsia_package_with_test(target_name) {
    test_component = "{{ '<var>' }}//path/to/test{{ '</var>' }}:fuchsia-example-test-root",
    test_component_name = "test-root.cm"
    subpackages = [
      test_info.target_label, # prebuilt version of the test suite.
      "{{ '<var>' }}//path/to/test{{ '</var>' }}:fuchsia-example-test-realm_factory"
    ]
    deps = [ "{{ '<var>' }}//path/to/test{{ '</var>' }}:fuchsia-example-test-helper" ]
  }
}
```

Note:

* For CTF to match this template with the prebuilt test package, this template
  _must_ be named as `generate_{package_name}` and `package_name` must match
  the test's original `ctf_fuchsia_package` name.
* This template subpackages `test_info.target_label` instead of
  `:fuchsia-example-tests` because the former points to the prebuilt version of
  the test from CIPD and the latter points to the latest version built from
  source.

### 5. Test the changes

To verify that these steps were completed correctly, run these commands:

```sh
fx set core.x64 --with //sdk/ctf:ctf_artifacts
fx build
```

The build should show an error prompt to run a command that updates
[//sdk/ctf/goldens/package_archives.json]. Run that command, then run
`fx build` again.

At this stage the build will print an error if the GN template defined in the
previous step is missing or if it cannot match the prebuilt version of the test
with the GN template. If everything succeeds, the test can be run using this
command:

```sh
fx test fuchsia-example-test_apitest
```

The `_apitest` suffix indicates that this is the version of the prebuilt test
from the latest build rather than some version from a previous CTF release.

### 6. Submit the changes

Send the changes for review. After submission the test will automatically be
included in the next CTF release when the next milestone branch is cut. For
additional review, please reach out to <fuchsia-ctf-team@google.com>.

## How to remove a test from CTF {#remove-a-test}

To remove a test from future CTF releases:

1. Remove the [test archive](#the-test-archive) from the build graph.
2. Delete the test's [gn template](#the-gn-template).
3. Remove the test's dependencies from the [allowlist].
4. Remove the [ctf templates](#the-ctf-templates) from the test's build rules.

Submit these changes to the main branch. Over time all of the API levels
corresponding to each release will become unsupported and the last version of
the test will stop running in CQ.

If you must immediately remove a test and all of its prebuilt version from CQ,
as an additional step you should follow Fuchsia's change control process and
cherry pick a CL that removes the test from the each of the corresponding
release branches. For example, If you want to remove a test from the CTF release
for API level 20, your CL needs to be cherry picked onto the `releases/f20`
branch.

## Test requirements

Every test must meet these requirements before being added to CTF.

### CTF tests must use the test realm factory pattern

The [test realm factory] pattern allows the build to version the test suite
component in CTF without versioning the component(s) under test. Please read the
test realm factory guide and refactor the test if necessary before following
this guide.

### CTF tests must only depend on partner-facing ABIs

A CTF test must only depend on software in the partner [SDK category] at
runtime because the test will force its dependencies to remain stable.
Exceptions for dependencies that are not in the partner SDK category are made by
updating the [allowlist].

### CTF tests must be written in C, C++, or Rust

At the time of writing CTF only supports these languages.

<!-- Links. Please link source code to https://cs.opensource.google -->
[allowlist]: /sdk/ctf/build/internal/allowed_ctf_deps.gni
[CTF]: /docs/development/testing/ctf/compatibility_testing.md
[SDK category]: /docs/contribute/sdk/categories.md
[test realm factory]: /docs/development/testing/components/test_realm_factory.md
[test root component]: /docs/development/testing/components/test_realm_factory.md#package-structure
[//sdk/ctf/tests:tests]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/ctf/tests/BUILD.gn
[//sdk/ctf/build]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/ctf/build/
[//sdk/ctf/build/generate_ctf_tests.gni]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/ctf/build/generate_ctf_tests.gni
[//sdk/ctf/goldens/package_archives.json]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/ctf/goldens/package_archives.json
[//sdk/ctf/test_realm/meta/test-collection.shard.cml]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/ctf/test_realm/meta/test-collection.shard.cml
