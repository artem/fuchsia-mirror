// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package tefmocheck

import (
	"bytes"
	"fmt"
	"path"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/bootserver/bootserverconstants"
	botanistconstants "go.fuchsia.dev/fuchsia/tools/botanist/constants"
	syslogconstants "go.fuchsia.dev/fuchsia/tools/lib/syslog/constants"
	netutilconstants "go.fuchsia.dev/fuchsia/tools/net/netutil/constants"
	sshutilconstants "go.fuchsia.dev/fuchsia/tools/net/sshutil/constants"
	testrunnerconstants "go.fuchsia.dev/fuchsia/tools/testing/testrunner/constants"
)

// stringInLogCheck checks if String is found in the log named LogName.
type stringInLogCheck struct {
	// String that will be searched for.
	String string
	// OnlyOnStates will cause Check() to return false if the swarming task
	// state doesn't match with one of these states.
	OnlyOnStates []string
	// ExceptString will cause Check() to return false if present.
	ExceptString string
	// ExceptBlocks will cause Check() to return false if the string is only
	// within these blocks.
	ExceptBlocks []*logBlock
	// SkipPassedTask will cause Check() to return false if the
	// Swarming task succeeded.
	SkipPassedTask bool
	// Type of log that will be checked.
	Type logType
	// Whether to check the per-test Swarming output for this log and emit a
	// check that's specific to the test during which the log appeared.
	AttributeToTest bool

	swarmingResult *SwarmingRpcsTaskResult
	testName       string
	outputFile     string
}

func (c *stringInLogCheck) Check(to *TestingOutputs) bool {
	c.swarmingResult = to.SwarmingSummary.Results
	if c.SkipPassedTask && !c.swarmingResult.Failure && c.swarmingResult.State == "COMPLETED" {
		return false
	}
	matchedState := false
	for _, state := range c.OnlyOnStates {
		if c.swarmingResult.State == state {
			matchedState = true
			break
		}
	}
	if len(c.OnlyOnStates) != 0 && !matchedState {
		return false
	}

	if c.Type == swarmingOutputType && c.AttributeToTest {
		for _, testLog := range to.SwarmingOutputPerTest {
			if c.checkBytes(testLog.Bytes) {
				c.testName = testLog.TestName
				c.outputFile = testLog.FilePath
				return true
			}
		}
	}

	var toCheck []byte
	switch c.Type {
	case serialLogType:
		toCheck = to.SerialLog
	case swarmingOutputType:
		toCheck = to.SwarmingOutput
	case syslogType:
		toCheck = to.Syslog
	}

	return c.checkBytes(toCheck)
}

func (c *stringInLogCheck) checkBytes(toCheck []byte) bool {
	if c.ExceptString != "" && bytes.Contains(toCheck, []byte(c.ExceptString)) {
		return false
	}
	stringBytes := []byte(c.String)
	if len(c.ExceptBlocks) == 0 {
		return bytes.Contains(toCheck, stringBytes)
	}
	index := bytes.Index(toCheck, stringBytes)
	for index >= 0 {
		foundString := true
		beforeBlock := toCheck[:index]
		nextStartIndex := index + len(stringBytes)
		if nextStartIndex > len(toCheck) {
			// The string was found at the end of the log, so it won't be included in
			// any exceptBlocks.
			return true
		}
		afterBlock := toCheck[nextStartIndex:]
		for _, block := range c.ExceptBlocks {
			closestStartIndex := bytes.LastIndex(beforeBlock, []byte(block.startString))
			if closestStartIndex < 0 {
				// There is no start string before this occurrence, so it must not be
				// included in this exceptBlock. Check the next exceptBlock.
				continue
			}
			closestEndIndex := bytes.LastIndex(beforeBlock, []byte(block.endString))
			if closestEndIndex < closestStartIndex {
				// There is no end string between the start string and the string to
				// check, so check if end string appears after. If so, then this
				// occurrence is included in this exceptBlock, so we can break and
				// check the next occurrence of the string.
				if bytes.Contains(afterBlock, []byte(block.endString)) {
					foundString = false
					break
				}
			}
		}
		if foundString {
			return true
		}
		index = bytes.Index(afterBlock, stringBytes)
		if index >= 0 {
			index += nextStartIndex
		}
	}
	return false
}

func (c *stringInLogCheck) Name() string {
	return path.Join("string_in_log", string(c.Type), strings.ReplaceAll(c.String, " ", "_"), c.testName)
}

func (c *stringInLogCheck) DebugText() string {
	debugStr := fmt.Sprintf("Found the string \"%s\" in ", c.String)
	if c.outputFile != "" && c.testName != "" {
		debugStr += fmt.Sprintf("%s of test %s.", filepath.Base(c.outputFile), c.testName)
	} else {
		debugStr += fmt.Sprintf("%s for task %s.", c.Type, c.swarmingResult.TaskId)
	}
	debugStr += "\nThat file should be accessible from the build result page or Sponge.\n"
	if c.ExceptString != "" {
		debugStr += fmt.Sprintf("\nDid not find the exception string \"%s\"", c.ExceptString)
	}
	if len(c.ExceptBlocks) > 0 {
		for _, block := range c.ExceptBlocks {
			debugStr += fmt.Sprintf("\nDid not occur inside a block delimited by:\nSTART: %s\nEND: %s", block.startString, block.endString)
		}
	}
	return debugStr
}

func (c *stringInLogCheck) OutputFiles() []string {
	if c.outputFile == "" {
		return []string{}
	}
	return []string{c.outputFile}
}

// StringInLogsChecks returns checks to detect bad strings in certain logs.
func StringInLogsChecks() []FailureModeCheck {
	ret := []FailureModeCheck{
		// For fxbug.dev/57548.
		// Hardware watchdog tripped, should not happen.
		// This string is specified in u-boot.
		&stringInLogCheck{String: "reboot_mode=watchdog_reboot", Type: serialLogType},
		// For fxbug.dev/47649.
		&stringInLogCheck{String: "kvm run failed Bad address", Type: swarmingOutputType},
		// For fxbug.dev/44779.
		&stringInLogCheck{String: netutilconstants.CannotFindNodeErrMsg, Type: swarmingOutputType},
		// For fxbug.dev/51015.
		&stringInLogCheck{
			String:         bootserverconstants.FailedToSendErrMsg(bootserverconstants.CmdlineNetsvcName),
			Type:           swarmingOutputType,
			SkipPassedTask: true,
		},
		// For fxbug.dev/43188.
		&stringInLogCheck{String: "/dev/net/tun (qemu): Device or resource busy", Type: swarmingOutputType},
		// For fxbug.dev/85875
		// This is printed by Swarming after a Swarming task's command completes, and
		// suggests that a test leaked a subprocess that modified one of the task's
		// output files after the task's command completed but before Swarming finished
		// uploading outputs.
		&stringInLogCheck{String: "error: blob size changed while uploading", Type: swarmingOutputType},
		// For fxbug.dev/55637
		&stringInLogCheck{String: " in fx_logger::GetSeverity() ", Type: swarmingOutputType},
		// For fxbug.dev/71784. Do not check for this in swarming output as this does not indicate
		// an error if logged by unit tests.
		&stringInLogCheck{String: "intel-i915: No displays detected.", Type: serialLogType},
		&stringInLogCheck{String: "intel-i915: No displays detected.", Type: syslogType},
	}

	oopsExceptBlocks := []*logBlock{
		{startString: " lock_dep_dynamic_analysis_tests ", endString: " lock_dep_static_analysis_tests "},
		{startString: "RUN   TestKillCriticalProcess", endString: ": TestKillCriticalProcess"},
		{startString: "RUN   TestKernelLockupDetectorCriticalSection", endString: ": TestKernelLockupDetectorCriticalSection"},
		{startString: "RUN   TestKernelLockupDetectorHeartbeat", endString: ": TestKernelLockupDetectorHeartbeat"},
		{startString: "RUN   TestPmmCheckerOopsAndPanic", endString: ": TestPmmCheckerOopsAndPanic"},
		{startString: "RUN   TestKernelLockupDetectorFatalCriticalSection", endString: ": TestKernelLockupDetectorFatalCriticalSection"},
		{startString: "RUN   TestKernelLockupDetectorFatalHeartbeat", endString: ": TestKernelLockupDetectorFatalHeartbeat"},
	}
	// These are rather generic. New checks should probably go above here so that they run before these.
	allLogTypes := []logType{serialLogType, swarmingOutputType, syslogType}
	for _, lt := range allLogTypes {
		// For fxbug.dev/43355.
		ret = append(ret, []FailureModeCheck{
			&stringInLogCheck{String: "Timed out loading dynamic linker from fuchsia.ldsvc.Loader", Type: lt},
			&stringInLogCheck{String: "ERROR: AddressSanitizer", Type: lt, AttributeToTest: true},
			&stringInLogCheck{String: "ERROR: LeakSanitizer", Type: lt, AttributeToTest: true, ExceptBlocks: []*logBlock{
				// startString and endString should match string in //zircon/system/ulib/c/test/sanitizer/lsan-test.cc.
				{startString: "[===LSAN EXCEPT BLOCK START===]", endString: "[===LSAN EXCEPT BLOCK END===]"},
				// Kernel out-of-memory test "OOMHard" may report false positive leaks.
				{startString: "RUN   TestOOMHard", endString: "PASS: TestOOMHard"},
			}},
			&stringInLogCheck{String: "SUMMARY: UndefinedBehaviorSanitizer", Type: lt, AttributeToTest: true},
			// Match specific OOPS types before finally matching the generic type.
			&stringInLogCheck{String: "lockup_detector: no heartbeat from", Type: lt, AttributeToTest: true, ExceptBlocks: oopsExceptBlocks},
			&stringInLogCheck{String: "ZIRCON KERNEL OOPS", Type: lt, AttributeToTest: true, ExceptBlocks: oopsExceptBlocks},
			&stringInLogCheck{String: "ZIRCON KERNEL PANIC", AttributeToTest: true, Type: lt, ExceptBlocks: []*logBlock{
				// These tests intentionally trigger kernel panics.
				{startString: "RUN   TestBasicCrash", endString: "PASS: TestBasicCrash"},
				{startString: "RUN   TestSMAPViolation", endString: "PASS: TestSMAPViolation"},
				{startString: "RUN   TestPmmCheckerOopsAndPanic", endString: "PASS: TestPmmCheckerOopsAndPanic"},
				{startString: "RUN   TestCrashAssert", endString: "PASS: TestCrashAssert"},
				{startString: "RUN   TestKernelLockupDetectorFatalCriticalSection", endString: ": TestKernelLockupDetectorFatalCriticalSection"},
				{startString: "RUN   TestKernelLockupDetectorFatalHeartbeat", endString: ": TestKernelLockupDetectorFatalHeartbeat"},
				{startString: "RUN   TestMissingCmdlineEntropyPanics", endString: "PASS: TestMissingCmdlineEntropyPanics"},
				{startString: "RUN   TestIncompleteCmdlineEntropyPanics", endString: "PASS: TestIncompleteCmdlineEntropyPanics"},
				{startString: "RUN   TestDisabledJitterEntropyAndRequiredDoesntBoot", endString: "PASS: TestDisabledJitterEntropyAndRequiredDoesntBoot"},
				{startString: "RUN   TestDisabledJitterEntropyAndRequiredForReseedDoesntReachUserspace", endString: "PASS: TestDisabledJitterEntropyAndRequiredForReseedDoesntReachUserspace"},
			}},
			&stringInLogCheck{String: "double fault, halting", Type: lt},
			&stringInLogCheck{String: "entering panic shell loop", Type: lt},
		}...)
	}

	ret = append(ret, []FailureModeCheck{
		// These may be in the output of tests, but the syslogType doesn't contain any test output.
		&stringInLogCheck{String: "ASSERT FAILED", Type: syslogType},
		&stringInLogCheck{String: "DEVICE SUSPEND TIMED OUT", Type: syslogType},
		// testrunner logs this when the serial socket goes away unexpectedly.
		&stringInLogCheck{String: ".sock: write: broken pipe", Type: swarmingOutputType},
		// For fxbug.dev/85596.
		&stringInLogCheck{String: "connect: no route to host", Type: swarmingOutputType},
		// For fxbug.dev/57463.
		&stringInLogCheck{
			String: fmt.Sprintf("%s: signal: segmentation fault", botanistconstants.QEMUInvocationErrorMsg),
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/61452.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.FailedToResolveIPErrorMsg),
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/65073.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.PackageRepoSetupErrorMsg),
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/65073.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.SerialReadErrorMsg),
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/68743.
		&stringInLogCheck{
			String: botanistconstants.FailedToCopyImageMsg,
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/82454.
		&stringInLogCheck{
			String: botanistconstants.FailedToExtendFVMMsg,
			Type:   swarmingOutputType,
		},
		// Error is being logged at https://fuchsia.googlesource.com/fuchsia/+/559948a1a4cbd995d765e26c32923ed862589a61/src/storage/lib/paver/paver.cc#175
		&stringInLogCheck{
			String: "Failed to stream partitions to FVM",
			Type:   swarmingOutputType,
		},
		// Emitted by the GCS Go library during image download.
		&stringInLogCheck{
			String: bootserverconstants.BadCRCErrorMsg,
			Type:   swarmingOutputType,
			// This error is generally transient, so ignore it as long as the
			// download can be retried and eventually succeeds.
			SkipPassedTask: true,
		},
		// For fxbug.dev/53101.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.FailedToStartTargetMsg),
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/51441.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", botanistconstants.ReadConfigFileErrorMsg),
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/59237.
		&stringInLogCheck{
			String: fmt.Sprintf("botanist ERROR: %s", sshutilconstants.TimedOutConnectingMsg),
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/61419.
		// Error is being logged at https://fuchsia.googlesource.com/fuchsia/+/675c6b9cc2452cd7108f075d91e048218b92ae69/garnet/bin/run_test_component/main.cc#431
		&stringInLogCheck{
			String: ".cmx canceled due to timeout.",
			Type:   swarmingOutputType,
			ExceptBlocks: []*logBlock{
				{
					startString: "[ RUN      ] RunFixture.TestTimeout",
					endString:   "RunFixture.TestTimeout (",
				},
			},
			OnlyOnStates: []string{"TIMED_OUT"},
		},
		// For fxbug.dev/61420.
		&stringInLogCheck{
			String:       fmt.Sprintf("syslog: %s", syslogconstants.CtxReconnectError),
			Type:         swarmingOutputType,
			OnlyOnStates: []string{"TIMED_OUT"},
		},
		// For fxbug.dev/52719.
		// Kernel panics and other low-level errors often cause crashes that
		// manifest as SSH failures, so this check must come after all
		// Zircon-related errors to ensure tefmocheck attributes these crashes to
		// the actual root cause.
		&stringInLogCheck{
			String: fmt.Sprintf("testrunner ERROR: %s", testrunnerconstants.FailedToReconnectMsg),
			Type:   swarmingOutputType,
		},
		&stringInLogCheck{
			String: "failed to resolve fuchsia-pkg://fuchsia.com/run_test_component#bin/run-test-component",
			Type:   swarmingOutputType,
		},
		&stringInLogCheck{
			String: "Got no package for fuchsia-pkg://",
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/77689.
		&stringInLogCheck{
			String: testrunnerconstants.FailedToStartSerialTestMsg,
			Type:   swarmingOutputType,
		},
		// For fxbug.dev/56651.
		// This error usually happens due to an SSH failure, so that error should take precedence.
		&stringInLogCheck{
			String: fmt.Sprintf("testrunner ERROR: %s", testrunnerconstants.FailedToRunSnapshotMsg),
			Type:   swarmingOutputType,
		},
	}...)
	return ret
}
