// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package fuzz

import (
	"bytes"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"testing"
)

// TODO(https://fxbug.dev/42121948): somehow validate build layouts?

func newBaseBuild() *BaseBuild {
	return &BaseBuild{
		Fuzzers: make(map[string]*Fuzzer),
		Paths:   make(map[string]string),
	}
}

func TestLoadFuzzersFromMissingFile(t *testing.T) {
	build := newBaseBuild()

	filename := invalidPath
	build.Paths["fuzzers.json"] = filename
	if err := build.LoadFuzzers(); err == nil {
		t.Fatalf("expected failure for %q", filename)
	}
}

func TestLoadFuzzersFromInvalidFile(t *testing.T) {
	build := newBaseBuild()

	filename := createTempfileWithContents(t, "not valid json", "json")
	defer os.Remove(filename)

	build.Paths["fuzzers.json"] = filename
	if err := build.LoadFuzzers(); err == nil {
		t.Fatal("expected failure for invalid json")
	}
}

func TestLoadFuzzersFromEmptyFile(t *testing.T) {
	build := newBaseBuild()

	filename := createTempfileWithContents(t, "[]", "json")
	defer os.Remove(filename)

	build.Paths["fuzzers.json"] = filename
	build.Paths["tests.json"] = filename

	if err := build.LoadFuzzers(); err != nil {
		t.Fatalf("error loading empty fuzzers: %s", err)
	}

	if len(build.Fuzzers) != 0 {
		t.Errorf("expected 0, got %d", len(build.Fuzzers))
	}
}

func TestLoadFuzzersWithIncompleteMetadata(t *testing.T) {
	build := newBaseBuild()

	// Missing "fuzzer"
	data := `[{"label": "//src/foo:bar", "package": "foo"}`
	filename := createTempfileWithContents(t, data, "json")
	defer os.Remove(filename)

	build.Paths["fuzzers.json"] = filename

	if err := build.LoadFuzzers(); err == nil {
		t.Fatal("expected failure for missing fuzzer in metadata")
	}

	// Missing "package"
	data = `[{"label": "//src/foo:bar", "fuzzer": "bar"}`
	filename = createTempfileWithContents(t, data, "json")
	defer os.Remove(filename)

	build.Paths["fuzzers.json"] = filename

	if err := build.LoadFuzzers(); err == nil {
		t.Fatal("expected failure for missing package in metadata")
	}
}

func TestFindToolPath(t *testing.T) {
	data := []byte(`[
  {
    "cpu": "x64",
    "label": "//build/tools/json_merge:json_merge(//build/toolchain:host_x64-novariant)",
    "name": "json_merge",
    "os": "linux",
    "path": "host_x64-novariant/json_merge"
  }
]`)

	buildDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(buildDir, "tool_paths.json"), data, 0o600); err != nil {
		t.Fatalf("error writing tool paths file: %s", err)
	}

	toolPath, err := findToolPath(buildDir, "json_merge")
	expected := filepath.Join(buildDir, "host_x64-novariant/json_merge")
	if err != nil || toolPath != expected {
		t.Fatalf("error loading fuzzers: %s", err)
	}
}

func TestLoadFuzzers(t *testing.T) {
	build := newBaseBuild()

	testData := `[{
    "environments": [
      {
        "dimensions": {
          "device_type": "AEMU"
        },
        "is_emu": true
      }
    ],
    "test": {
      "build_rule": "fuchsia_fuzzer_package",
      "cpu": "x64",
      "label": "//examples/fuzzers:example-fuzzer_package(//build/toolchain/fuchsia:x64)",
      "log_settings": {
        "max_severity": "WARN"
      },
      "name": "fuchsia-pkg://fuchsia.com/example-fuzzers#meta/crash_fuzzer.cm",
      "os": "fuchsia",
      "package_label": "//examples/fuzzers:example-fuzzers_fuzzed(//build/toolchain/fuchsia:x64)",
      "package_manifests": [
        "obj/examples/fuzzers/example-fuzzer_package/package_manifest.json"
      ],
      "package_url": "fuchsia-pkg://fuchsia.com/example-fuzzers#meta/crash_fuzzer.cm",
      "runtime_deps": "gen/examples/fuzzers/crash_fuzzer_component_test.deps.json"
    }
  },
  {
    "environments": [
      {
        "dimensions": {
          "device_type": "AEMU"
        },
        "is_emu": true
      }
    ],
    "test": {
      "build_rule": "fuchsia_fuzzer_package",
      "cpu": "x64",
      "label": "//foo/fuzzers:foo-fuzzer_package(//build/toolchain/fuchsia:x64)",
      "log_settings": {
        "max_severity": "WARN"
      },
      "name": "fuchsia-pkg://fuchsia.com/foo-fuzzers#meta/crash_fuzzer.cm",
      "os": "fuchsia",
      "package_label": "//foo/fuzzers:foo-fuzzers_fuzzed(//build/toolchain/fuchsia:x64)",
      "package_manifests": [
        "obj/foo/fuzzers/foo-fuzzer_package/package_manifest.json"
      ],
      "package_url": "fuchsia-pkg://fuchsia.com/foo-fuzzers#meta/crash_fuzzer.cm",
      "runtime_deps": "gen/foo/fuzzers/crash_fuzzer_component_test.deps.json"
    }
  }]`

	testFilename := createTempfileWithContents(t, testData, "json")
	defer os.Remove(testFilename)

	build.Paths["tests.json"] = testFilename

	if err := build.LoadFuzzers(); err != nil {
		t.Fatalf("error loading fuzzers: %s", err)
	}

	if _, err := build.Fuzzer("example-fuzzers/crash_fuzzer"); err != nil {
		t.Fatalf("missing expected fuzzer")
	}

	fuzzers := build.ListFuzzers()
	if !reflect.DeepEqual(fuzzers, []string{"foo-fuzzers/crash_fuzzer"}) {
		t.Fatalf("incorrect fuzzer list: %v", fuzzers)
	}
}

func TestPath(t *testing.T) {
	build := newBaseBuild()

	if _, err := build.Path(""); err == nil {
		t.Fatalf("expected failure for empty build")
	}
	if _, err := build.Path("foo"); err == nil {
		t.Fatalf("expected failure for empty build")
	}

	build.Paths["foo"] = "foo-value"
	val, err := build.Path("foo")
	if err != nil {
		t.Fatalf("expected success but got: %s", err)
	}
	if len(val) < 1 || val[0] != "foo-value" {
		t.Fatalf("expected foo-value, got %q", val)
	}

	build.Paths["bar"] = "bar-value"
	build.Paths["baz"] = "baz-value"
	build.Paths["foo"] = "foo-value"
	val, err = build.Path("foo")
	if err != nil {
		t.Fatalf("expected success but got: %s", err)
	}
	if len(val) < 1 || val[0] != "foo-value" {
		t.Fatalf("expected foo-value, got %q", val)
	}

	val, err = build.Path("bar", "baz")
	if err != nil {
		t.Fatalf("expected success but got: %s", err)
	}
	if len(val) < 2 || val[0] != "bar-value" || val[1] != "baz-value" {
		t.Fatalf("unexpected value, got %q", val)
	}
}

func TestSymbolize(t *testing.T) {
	// Enable subprocess mocking
	ExecCommand = mockCommand
	defer func() { ExecCommand = exec.Command }()

	build := newBaseBuild()
	build.Paths["symbolizer"] = "symbolizer"
	build.Paths["llvm-symbolizer"] = "llvm-symbolizer"

	// TODO(https://fxbug.dev/42121948): more realistic test data
	inputData := "[1234.5][klog] INFO: {{{0x41}}}"
	expectedOutput := "wow.c:1\n"
	src := io.NopCloser(bytes.NewBufferString(inputData))
	var dst bytes.Buffer
	if err := build.Symbolize(src, &dst); err != nil {
		t.Fatalf("error during symbolization: %s", err)
	}

	if dst.String() != expectedOutput {
		t.Fatalf("unexpected symbolizer output: %q", dst.String())
	}
}

func TestStripLogPrefix(t *testing.T) {
	testCases := []string{
		"something",
		"[klog] INFO: something",
		"[1234][5678][9][klog] INFO: something",
		"[1234][klog] INFO: something",
		"[1234.5][klog] INFO: something",
	}

	want := "something"

	for _, input := range testCases {
		if got := stripLogPrefix(input); got != want {
			t.Fatalf("unexpected log prefix stripping result for %q: got %q, want %q",
				input, got, want)
		}
	}
}
