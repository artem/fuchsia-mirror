// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/ioutil"
	"log"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"time"

	testsharder "go.fuchsia.dev/fuchsia/tools/integration/testsharder/lib"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/net/sshutil"
	"go.fuchsia.dev/fuchsia/tools/testing/runtests"
	tap "go.fuchsia.dev/fuchsia/tools/testing/tap/lib"
	testparser "go.fuchsia.dev/fuchsia/tools/testing/testparser/lib"
	testrunner "go.fuchsia.dev/fuchsia/tools/testing/testrunner/lib"
)

// Fuchsia-specific environment variables possibly exposed to the testrunner.
const (
	nodenameEnvVar = "FUCHSIA_NODENAME"
	sshKeyEnvVar   = "FUCHSIA_SSH_KEY"
	// A directory that will be automatically isolated on completion of a task.
	testOutdirEnvVar = "FUCHSIA_TEST_OUTDIR"
)

// Command-line flags
var (
	// Whether to show Usage and exit.
	help bool

	// The path where a directory containing test results should be created.
	outDir string

	// Working directory of the local testing subprocesses.
	localWD string

	// Whether to use runtests when executing tests on fuchsia. If false, the
	// default will be run_test_component.
	useRuntests bool

	// Per-test timeout.
	perTestTimeout time.Duration
)

func usage() {
	fmt.Printf(`testrunner [flags] tests-file

Executes all tests found in the JSON [tests-file]
Fuchsia tests require both the nodename of the fuchsia instance and a private
SSH key corresponding to a authorized key to be set in the environment under
%s and %s respectively.
`, nodenameEnvVar, sshKeyEnvVar)
}

func init() {
	flag.BoolVar(&help, "help", false, "Whether to show Usage and exit.")
	flag.StringVar(&outDir, "out-dir", "", "Optional path where a directory containing test results should be created.")
	flag.StringVar(&localWD, "C", "", "Working directory of local testing subprocesses; if unset the current working directory will be used.")
	flag.BoolVar(&useRuntests, "use-runtests", false, "Whether to default to running fuchsia tests with runtests; if false, run_test_component will be used.")
	// TODO(fxb/36480): Support different timeouts for different tests.
	flag.DurationVar(&perTestTimeout, "per-test-timeout", 0, "Per-test timeout, applied to all tests. Ignored if <= 0.")
	flag.Usage = usage
}

func main() {
	flag.Parse()

	if help || flag.NArg() != 1 {
		flag.Usage()
		flag.PrintDefaults()
		return
	}

	l := logger.NewLogger(logger.DebugLevel, color.NewColor(color.ColorAuto), os.Stdout, os.Stderr, "testrunner ")
	ctx := logger.WithLogger(context.Background(), l)

	testsPath := flag.Arg(0)
	tests, err := loadTests(testsPath)
	if err != nil {
		log.Fatalf("failed to load tests from %q: %v", testsPath, err)
	}

	// Configure a test outputs object, responsible for producing TAP output,
	// recording data sinks, and archiving other test outputs.
	testOutdir := filepath.Join(os.Getenv(testOutdirEnvVar), outDir)
	if testOutdir == "" {
		var err error
		testOutdir, err = ioutil.TempDir("", "testrunner")
		if err != nil {
			log.Fatalf("failed to create a test output directory")
		}
	}
	logger.Debugf(ctx, "test output directory: %s", testOutdir)

	tapProducer := tap.NewProducer(os.Stdout)
	tapProducer.Plan(len(tests))
	outputs, err := createTestOutputs(tapProducer, testOutdir)
	if err != nil {
		log.Fatalf("failed to create test results object: %v", err)
	}
	defer outputs.Close()

	nodename := os.Getenv(nodenameEnvVar)
	sshKeyFile := os.Getenv(sshKeyEnvVar)

	if err := execute(ctx, tests, outputs, nodename, sshKeyFile); err != nil {
		log.Fatal(err)
	}
}

func validateTest(test testsharder.Test) error {
	if test.Name == "" {
		return fmt.Errorf("one or more tests missing `name` field")
	}
	if test.OS == "" {
		return fmt.Errorf("one or more tests missing `os` field")
	}
	if test.Runs <= 0 {
		return fmt.Errorf("one or more tests with invalid `runs` field")
	}
	if test.OS == "fuchsia" {
		if test.Test.PackageURL == "" {
			return fmt.Errorf("one or more fuchsia tests missing `packageurl` field")
		}
	} else if test.Test.Path == "" {
		return fmt.Errorf("one or more host tests missing `path` field")
	}
	return nil
}

func loadTests(path string) ([]testsharder.Test, error) {
	bytes, err := ioutil.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("failed to read %q: %w", path, err)
	}

	var tests []testsharder.Test
	if err := json.Unmarshal(bytes, &tests); err != nil {
		return nil, fmt.Errorf("failed to unmarshal %q: %w", path, err)
	}

	for _, test := range tests {
		if err := validateTest(test); err != nil {
			return nil, err
		}
	}

	return tests, nil
}

type tester interface {
	Test(context.Context, testsharder.Test, io.Writer, io.Writer) (runtests.DataSinkReference, error)
	Close() error
	CopySinks(context.Context, []runtests.DataSinkReference) error
}

func execute(ctx context.Context, tests []testsharder.Test, outputs *testOutputs, nodename, sshKeyFile string) error {
	var localTests, fuchsiaTests []testsharder.Test
	for _, test := range tests {
		switch test.OS {
		case "fuchsia":
			fuchsiaTests = append(fuchsiaTests, test)
		case "linux":
			if runtime.GOOS != "linux" {
				return fmt.Errorf("cannot run linux tests when GOOS = %q", runtime.GOOS)
			}
			localTests = append(localTests, test)
		case "mac":
			if runtime.GOOS != "darwin" {
				return fmt.Errorf("cannot run mac tests when GOOS = %q", runtime.GOOS)
			}
			localTests = append(localTests, test)
		default:
			return fmt.Errorf("test %#v has unsupported OS: %q", test, test.OS)
		}
	}

	localEnv := append(os.Environ(),
		// Tell tests written in Rust to print stack on failures.
		"RUST_BACKTRACE=1",
	)
	localTester := newSubprocessTester(localWD, localEnv, perTestTimeout)
	if err := runTests(ctx, localTests, localTester, outputs); err != nil {
		return err
	}

	if len(fuchsiaTests) == 0 {
		return nil
	}

	var t tester
	var err error
	if sshKeyFile != "" {
		if nodename == "" {
			return fmt.Errorf("%s must be set", nodenameEnvVar)
		}
		t, err = newFuchsiaSSHTester(ctx, nodename, sshKeyFile, outputs.outDir, useRuntests, perTestTimeout)
	} else {
		// TODO(fxbug.dev/41930): create a serial test runner in this case.
		return fmt.Errorf("%s must be set", sshKeyEnvVar)
	}
	if err != nil {
		return fmt.Errorf("failed to initialize fuchsia tester: %v", err)
	}
	defer t.Close()

	return runTests(ctx, fuchsiaTests, t, outputs)
}

func runTests(ctx context.Context, tests []testsharder.Test, t tester, outputs *testOutputs) error {
	var sinks []runtests.DataSinkReference
	for _, test := range tests {
		for i := 0; i < test.Runs; i++ {
			result, err := runTest(ctx, test, i, t)
			if errors.Is(err, sshutil.ConnectionError) {
				return err
			}
			if err := outputs.record(*result); err != nil {
				return err
			}
			sinks = append(sinks, result.DataSinks)
		}
	}
	return t.CopySinks(ctx, sinks)
}

// stdioBuffer is a simple thread-safe wrapper around bytes.Buffer. It
// implements the io.Writer interface.
type stdioBuffer struct {
	// Used to protect access to `buf`.
	mu sync.Mutex

	// The underlying buffer.
	buf bytes.Buffer
}

func (b *stdioBuffer) Write(p []byte) (n int, err error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func runTest(ctx context.Context, test testsharder.Test, runIndex int, t tester) (*testrunner.TestResult, error) {
	result := runtests.TestSuccess
	// The test case parser specifically uses stdout, so we need to have a
	// dedicated stdout buffer.
	stdout := bytes.Buffer{}
	stdio := stdioBuffer{}

	multistdout := io.MultiWriter(os.Stdout, &stdio, &stdout)
	multistderr := io.MultiWriter(os.Stderr, &stdio)

	startTime := time.Now()
	dataSinks, err := t.Test(ctx, test, multistdout, multistderr)
	if err != nil {
		result = runtests.TestFailure
		logger.Errorf(ctx, err.Error())
		var etf errTestFailure
		if !errors.As(err, &etf) {
			return nil, err
		}
	}

	endTime := time.Now()

	// Record the test details in the summary.
	return &testrunner.TestResult{
		Name:      test.Name,
		GNLabel:   test.Label,
		Stdio:     stdio.buf.Bytes(),
		Result:    result,
		Cases:     testparser.Parse(stdout.Bytes()),
		StartTime: startTime,
		EndTime:   endTime,
		DataSinks: dataSinks,
		RunIndex:  runIndex,
	}, nil
}
