// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package coverage_verifier

import (
	"context"
	"encoding/json"
	"flag"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"go.fuchsia.dev/fuchsia/tools/botanist/targets"
	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/debug/covargs/api/llvm"
	"go.fuchsia.dev/fuchsia/tools/emulator"
	"go.fuchsia.dev/fuchsia/tools/emulator/emulatortest"
	"go.fuchsia.dev/fuchsia/tools/integration/testsharder"
	"go.fuchsia.dev/fuchsia/tools/testing/runtests"
	"go.fuchsia.dev/fuchsia/tools/testing/tap"
	"go.fuchsia.dev/fuchsia/tools/testing/testrunner"
	fvdpb "go.fuchsia.dev/fuchsia/tools/virtual_device/proto"
)

var (
	configPath = flag.String("config", "", "Path to the configuration file.")
)

const defaultNodename = "fuchsia-5254-0012-3456"

type Binary struct {
	Ffx          string `json:"ffx"`
	LlvmCov      string `json:"llvm_cov"`
	LlvmProfdata string `json:"llvm_profdata"`
	LlvmCxxFilt  string `json:"llvm_cxxfilt"`
	Fvm          string `json:"fvm"`
}

type Function struct {
	Name  string `json:"name"`
	Count int    `json:"count"`
}

type Expectations struct {
	Source    string     `json:"source"`
	Functions []Function `json:"functions"`
}

type TestInfo struct {
	Path       string `json:"path"`
	Name       string `json:"name"`
	ZbiImage   string `json:"zbi_image"`
	BlockImage string `json:"block_image"`
	SshKeyFile string `json:"ssh_key,omitempty"`
}

type Config struct {
	Bin          Binary
	Test         TestInfo
	Expectations []Expectations
}

func execDir(t *testing.T) string {
	ex, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	return filepath.Dir(ex)
}

func ParseConfiguration(t *testing.T) Config {
	data, err := os.ReadFile(*configPath)
	if err != nil {
		t.Fatalf("Failed to read configuration file at '%s'", *configPath)
	}
	var config Config
	err = json.Unmarshal(data, &config)
	if err != nil {
		t.Fatalf("Failed to parse JSON config. Reason: %s", err)
	}
	return config
}

func GetCoverageDataFromTest(t *testing.T, outDir string, config *Config) []string {
	exDir := execDir(t)
	distro := emulatortest.UnpackFrom(t, filepath.Join(exDir, "test_data"), emulator.DistributionParams{
		Emulator: emulator.Qemu,
	})
	arch := distro.TargetCPU()
	device := emulator.DefaultVirtualDevice(string(arch))

	// Resize image to avoid problems due to trimmed FVM if FVM.
	resizeImage := distro.ResizeRawImage(config.Test.BlockImage, config.Bin.Fvm)
	if len(resizeImage) == 0 {
		t.Fatalf("Failed to resize image.")
	}

	// FVM/FXFS path
	device.Drive = &fvdpb.Drive{
		Id:         "disk00",
		Image:      resizeImage,
		IsFilename: true,
		Device:     &fvdpb.Device{Model: "virtio-blk-pci"},
	}
	// ZBI path
	device.Initrd = config.Test.ZbiImage

	// Network
	device.Hw.NetworkDevices = append(device.Hw.NetworkDevices, &fvdpb.Netdev{
		Id:     "qemu",
		Kind:   "tap",
		Device: &fvdpb.Device{Model: "virtio-net-pci"},
	})
	device.KernelArgs = append(device.KernelArgs, "zircon.nodename="+defaultNodename)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	i := distro.CreateContext(ctx, device)
	i.Start()
	i.WaitForLogMessage("initializing platform")
	// Component manager starts up.
	i.WaitForLogMessage("[component_manager] INFO: Component manager is starting up...")
	// Netstack is up. This would contain the line number of the log message. Lets avoid it.
	i.WaitForLogMessage("[netstack] INFO: main.go")

	// Resolve node context.
	t.Log("Resolving target IP.")
	_, ipv6, err := targets.ResolveIP(context.Background(), defaultNodename)
	if err != nil {
		t.Fatalf("Failed to resolved node IP address. Reason: %s\n", err)
	}

	// Create an ssh tester.
	var shard testsharder.Test
	var runner testrunner.Tester

	shard = testsharder.Test{
		Test: build.Test{
			Name:       config.Test.Name,
			PackageURL: config.Test.Name,
		},
		RunAlgorithm: testsharder.StopOnFailure,
		Runs:         1,
	}

	var address net.IPAddr = ipv6

	t.Log("Establishing SSH Session.")
	runner_ctx := context.Background()
	runner, err = testrunner.NewFuchsiaSSHTester(runner_ctx, address, config.Test.SshKeyFile, outDir, "")
	if err != nil {
		t.Fatalf("Error initializing Fuchsia SSH Test. Reason: %s", err)
	}
	defer runner.Close()

	test_ctx := context.Background()
	t.Logf("Running Test: %s", config.Test.Name)
	test_result, err := runner.Test(test_ctx, shard, os.Stdout, os.Stdout, "unused-out-dir")
	if err != nil {
		t.Fatalf("Test execution failed. Reason: %s", err)
	}

	t.Logf("Retrieving test Artifacts. Output dir : %s\n", outDir)
	outputs, err := testrunner.CreateTestOutputs(tap.NewProducer(io.Discard), outDir)
	if err != nil {
		t.Fatalf("Test output creation failed. Reason: %s", err)
	}

	if err := outputs.Record(ctx, *test_result); err != nil {
		t.Fatalf("Failed to record data sinks. Reason: %s", err)
	}

	var sinks []runtests.DataSinkReference

	err = runner.EnsureSinks(ctx, sinks, outputs)
	if err != nil {
		t.Fatalf("Error collecting data sinks from fuchsia device. Reason: %s", err)
	}

	if err := outputs.Close(); err != nil {
		t.Fatalf("Failed to save test outputs. Reason: %s", err)
	}

	t.Log("Processing retrieved profiles.")
	t.Logf("Test count: %d", len(outputs.Summary.Tests))
	var rawProfiles []string
	for curr, test := range outputs.Summary.Tests {
		t.Logf("Test[%d] Name %s Sink count: %d", curr, test.Name, len(test.DataSinks))
		for _, sinks := range test.DataSinks {
			for _, sink := range sinks {
				path_components := strings.Split(sink.Name, string(os.PathSeparator))
				if path_components[0] == "llvm-profile" {
					rawProfiles = append(rawProfiles, filepath.Join(outDir, sink.File))
				} else {
					t.Logf("Unexpected data sink: %s", path_components[0])
				}
			}
		}
	}

	return rawProfiles
}

func GetMergedProfiles(t *testing.T, config *Config, profRaws []string, outDir string) string {
	if len(profRaws) < 1 {
		t.Fatal("Cannot execute llvm-profdata. No profraw files provided.")
	}

	profRawPaths :=
		strings.Join(profRaws, " ")

	validateCmd := []string{
		"show",
		"-binary-ids",
		profRawPaths,
	}

	showCmd := exec.Command(config.Bin.LlvmProfdata, validateCmd...)
	t.Logf("Preparing llvm-profraw validation: %s", showCmd.String())
	showCmdOutput, err := showCmd.CombinedOutput()
	if err != nil {
		if _, ok := err.(*exec.ExitError); ok {
			t.Fatalf("Failed to read profraw file. Reason: %s", string(showCmdOutput))
		} else {
			t.Fatalf("Failed to execute %q. Reason: %s", showCmd, err)
		}
	}
	t.Logf("Data Validated.")

	mergeOutput := filepath.Join(outDir, "coverage.profdata")
	mergeCmds := []string{
		"merge",
		profRawPaths,
		"-o",
		mergeOutput,
	}
	mergeCmd := exec.Command(config.Bin.LlvmProfdata, mergeCmds...)
	t.Logf("Merging profiles: %s", mergeCmd.String())
	if mergeCmdOutput, err := mergeCmd.CombinedOutput(); err != nil {
		if _, ok := err.(*exec.ExitError); ok {
			t.Fatalf("Failed to merge profraw files. Reason: %s", string(mergeCmdOutput))
		} else {
			t.Fatalf("Failed to execute %q. Reason: %s", mergeCmd, err)
		}
	}
	t.Logf("Profiles merged successfully. Merge output in %s", mergeOutput)

	return mergeOutput
}

func ConvertMergedProfilesToJson(t *testing.T, mergedOutput string, config *Config, outDir string) llvm.Export {
	exportCmds := []string{
		"export",
		"-format=text",
		"-instr-profile", mergedOutput,
		config.Test.Path,
	}

	// Append sources to filter output only touching the files we care about.
	exportCmds = append(exportCmds, "-sources")
	for _, exp := range config.Expectations {
		exportCmds = append(exportCmds, exp.Source)
	}

	exportCmd := exec.Command(config.Bin.LlvmCov, exportCmds...)
	t.Logf("Exporting coverage data as JSON: %s", exportCmd.String())
	exportCmdOutput, err := exportCmd.CombinedOutput()
	if err != nil {
		if _, ok := err.(*exec.ExitError); ok {
			t.Fatalf("Failed to export merged profiles. Reason: %s", string(exportCmdOutput))
		} else {
			t.Fatalf("Failed to execute %q. Reason: %s", exportCmd, err)
		}
	}
	var coverageJson llvm.Export
	if err = json.Unmarshal(exportCmdOutput, &coverageJson); err != nil {
		t.Fatalf("Failed to parse generated coverage json. Reason: %s", err)
	}

	return coverageJson
}

func demangleName(t *testing.T, config *Config, name *string) string {
	demangleCmd := exec.Command(config.Bin.LlvmCxxFilt, *name)
	demangledOutput, err := demangleCmd.Output()
	if err != nil {
		t.Fatalf("Failed to demangle name. Reason: %s", err)
	}
	return strings.TrimSpace(string(demangledOutput))
}

func TestCoverage(t *testing.T) {
	outDir := t.TempDir()
	cfg := ParseConfiguration(t)
	//profraws
	profrawPaths := GetCoverageDataFromTest(t, outDir, &cfg)
	mergedProfiles := GetMergedProfiles(t, &cfg, profrawPaths, outDir)
	coverageJson := ConvertMergedProfilesToJson(t, mergedProfiles, &cfg, outDir)

	type ExpectCheck struct {
		count       int
		actualCount int
		found       bool
	}

	sourceToFunction := map[string]map[string]ExpectCheck{}

	for _, expectation := range cfg.Expectations {
		for _, functionExpectation := range expectation.Functions {
			var expectedFunction ExpectCheck
			expectedFunction.count = functionExpectation.Count
			expectedFunction.actualCount = 0
			expectedFunction.found = false
			if _, exists := sourceToFunction[expectation.Source]; !exists {
				sourceToFunction[expectation.Source] = map[string]ExpectCheck{}
			}
			sourceToFunction[expectation.Source][functionExpectation.Name] = expectedFunction
		}
	}

	for _, data := range coverageJson.Data {
		for _, function := range data.Functions {
			for _, filename := range function.Filenames {
				if symbolToCount, contained := sourceToFunction[filename]; contained {
					demangledName := demangleName(t, &cfg, &function.Name)
					if expectedFunction, contained := symbolToCount[demangledName]; contained {
						expectedFunction.found = true
						expectedFunction.actualCount = function.Count
						sourceToFunction[filename][demangledName] = expectedFunction
					}
				}
			}
		}
	}

	failures := 0
	for filename, symbolNameToExpectation := range sourceToFunction {
		for symbolName, expectedFunction := range symbolNameToExpectation {
			if !expectedFunction.found {
				t.Logf("Symbol %s in file %s was not found in coverage data.", symbolName, filename)
				failures++
				continue
			}

			if expectedFunction.actualCount != expectedFunction.count {
				t.Logf("Symbol %s in file %s actualCount %d and expectedCount of %d.", symbolName, filename, expectedFunction.actualCount, expectedFunction.count)
				failures++
				continue
			}

		}
	}

	if failures > 0 {
		t.Fatal("Failed to meet all expectations.")
	}
}
