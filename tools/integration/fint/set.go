// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package fint

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"

	fintpb "go.fuchsia.dev/fuchsia/tools/integration/fint/proto"
	"go.fuchsia.dev/fuchsia/tools/lib/hostplatform"
	"go.fuchsia.dev/fuchsia/tools/lib/isatty"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	"go.fuchsia.dev/fuchsia/tools/lib/subprocess"
)

var (
	// Path within a checkout to script which will run a hermetic Python interpreter.
	vendoredPythonScriptPath = []string{"scripts", "fuchsia-vendored-python"}

	// Path within a checkout to script which will clobber a build when new fences appear.
	forceCleanScript = []string{"build", "force_clean", "force_clean_if_needed.py"}
)

// Set runs `gn gen` given a static and context spec. It's intended to be
// consumed as a library function.
func Set(ctx context.Context, staticSpec *fintpb.Static, contextSpec *fintpb.Context, skipLocalArgs bool) (*fintpb.SetArtifacts, error) {
	platform, err := hostplatform.Name()
	if err != nil {
		return nil, err
	}

	// TODO move to setImpl, add unit tests
	if err := forceCleanIfNeeded(ctx, contextSpec, platform); err != nil {
		return nil, err
	}

	artifacts, err := setImpl(ctx, &subprocess.Runner{}, staticSpec, contextSpec, platform, skipLocalArgs)
	if err != nil && artifacts != nil && artifacts.FailureSummary == "" {
		// Fall back to using the error text as the failure summary if the
		// failure summary is unset. It's better than failing without emitting
		// any information.
		artifacts.FailureSummary = err.Error()
	}
	return artifacts, err
}

// setImpl runs `gn gen` along with any post-processing steps, and returns a
// SetArtifacts object containing metadata produced by GN and post-processing.
func setImpl(
	ctx context.Context,
	runner subprocessRunner,
	staticSpec *fintpb.Static,
	contextSpec *fintpb.Context,
	platform string,
	skipLocalArgs bool,
) (*fintpb.SetArtifacts, error) {
	if contextSpec.CheckoutDir == "" {
		return nil, fmt.Errorf("checkout_dir must be set")
	}
	if contextSpec.BuildDir == "" {
		return nil, fmt.Errorf("build_dir must be set")
	}

	genArgs, err := genArgs(ctx, staticSpec, contextSpec, skipLocalArgs)
	if err != nil {
		return nil, err
	}

	artifacts := &fintpb.SetArtifacts{
		UseGoma: staticSpec.UseGoma,
		Metadata: &fintpb.SetArtifacts_Metadata{
			Board:      staticSpec.Board,
			Optimize:   strings.ToLower(staticSpec.Optimize.String()),
			Product:    staticSpec.Product,
			TargetArch: strings.ToLower(staticSpec.TargetArch.String()),
			Variants:   staticSpec.Variants,
		},
		// True if any toolchain is using RBE and needs reproxy to run.
		// Note: bazel+RBE doesn't require reproxy.
		EnableRbe: staticSpec.RustRbeEnable || staticSpec.CxxRbeEnable || staticSpec.LinkRbeEnable,
	}

	if contextSpec.ArtifactDir != "" {
		artifacts.GnTracePath = filepath.Join(contextSpec.ArtifactDir, "gn_trace.json")
	}
	genStdout, err := runGen(ctx, runner, staticSpec, contextSpec, platform, artifacts.GnTracePath, genArgs)
	if err != nil {
		artifacts.FailureSummary = genStdout
		return artifacts, err
	}

	// Only run build graph analysis if the result will be emitted via
	// artifacts, and if we actually care about checking the result.
	if contextSpec.ArtifactDir != "" && staticSpec.SkipIfUnaffected {
		var changedFiles []string
		for _, f := range contextSpec.ChangedFiles {
			changedFiles = append(changedFiles, f.Path)
		}
		sb, err := shouldBuild(ctx, runner, contextSpec.BuildDir, contextSpec.CheckoutDir, platform, changedFiles)
		if err != nil {
			return artifacts, err
		}
		artifacts.SkipBuild = !sb
	}
	return artifacts, err
}

// forceCleanIfNeeded clobbers the build dir if new clean build fences are found, see
// //build/force_clean/README.md for details.
func forceCleanIfNeeded(ctx context.Context, contextSpec *fintpb.Context, platform string) (err error) {
	if _, err := os.Stat(contextSpec.BuildDir); os.IsNotExist(err) {
		// no need to clean anything if there's nothing there
		return nil
	}
	scriptRunner := &subprocess.Runner{}
	scriptRunner.Dir = contextSpec.CheckoutDir
	return scriptRunner.Run(ctx, []string{
		filepath.Join(append([]string{contextSpec.CheckoutDir}, vendoredPythonScriptPath...)...),
		filepath.Join(append([]string{contextSpec.CheckoutDir}, forceCleanScript...)...),
		"--gn-bin",
		thirdPartyPrebuilt(contextSpec.CheckoutDir, platform, "gn"),
		"--checkout-dir",
		contextSpec.CheckoutDir,
		"--build-dir",
		contextSpec.BuildDir,
	}, subprocess.RunOptions{})
}

func runGen(
	ctx context.Context,
	runner subprocessRunner,
	staticSpec *fintpb.Static,
	contextSpec *fintpb.Context,
	platform string,
	gnTracePath string,
	args []string,
) (genStdout string, err error) {
	gn := thirdPartyPrebuilt(contextSpec.CheckoutDir, platform, "gn")

	formattedArgs := gnFormat(ctx, gn, runner, args)
	logger.Infof(ctx, "GN args:\n%s\n", formattedArgs)

	// gn will return an error if the argument list is too long, so write the
	// args directly to the build dir instead of using the --args flag.
	if f, err := osmisc.CreateFile(filepath.Join(contextSpec.BuildDir, "args.gn")); err != nil {
		return "", fmt.Errorf("failed to create args.gn: %w", err)
	} else if _, err := io.WriteString(f, formattedArgs); err != nil {
		return "", fmt.Errorf("failed to write args.gn: %w", err)
	}

	genCmd := []string{
		gn,
		"gen",
		contextSpec.BuildDir,
		fmt.Sprintf("--root=%s", contextSpec.CheckoutDir),
		"--check=system",
		"--fail-on-unused-args",
		// If --ninja-executable is set, GN runs `ninja -t restat build.ninja`
		// after generating the ninja files, updating the cached modified
		// timestamps of files. This avoids extra regens when running ninja
		// repeatedly under some circumstances.
		fmt.Sprintf("--ninja-executable=%s", thirdPartyPrebuilt(contextSpec.CheckoutDir, platform, "ninja")),
	}

	if isatty.IsTerminal() {
		genCmd = append(genCmd, "--color")
	}
	if gnTracePath != "" {
		genCmd = append(genCmd, fmt.Sprintf("--tracelog=%s", gnTracePath))
	}
	if staticSpec.ExportRustProject {
		genCmd = append(genCmd, "--export-rust-project")
	}
	for _, f := range staticSpec.IdeFiles {
		genCmd = append(genCmd, fmt.Sprintf("--ide=%s", f))
	}
	for _, s := range staticSpec.JsonIdeScripts {
		genCmd = append(genCmd, fmt.Sprintf("--json-ide-script=%s", s))
	}

	// Always generate the ninja_outputs.json file used by //build/api/client
	genCmd = append(genCmd, "--ninja-outputs-file=ninja_outputs.json")

	// When `gn gen` fails, it outputs a brief helpful error message to stdout.
	var stdoutBuf bytes.Buffer
	if err := runner.Run(ctx, genCmd, subprocess.RunOptions{Stdout: io.MultiWriter(&stdoutBuf, os.Stdout)}); err != nil {
		return stdoutBuf.String(), fmt.Errorf("error running gn gen: %w", err)
	}
	return stdoutBuf.String(), nil
}

// findGNIFile returns the relative path to a board or product file in a
// checkout, given a basename. It checks the root of the checkout as well as
// each vendor/* directory for a file matching "<dirname>/<basename>.gni", e.g.
// "boards/core.gni".
func findGNIFile(checkoutDir, dirname, basename string) (string, error) {
	dirs, err := filepath.Glob(filepath.Join(checkoutDir, "vendor", "*", dirname))
	if err != nil {
		return "", err
	}
	dirs = append(dirs, filepath.Join(checkoutDir, dirname))

	for _, dir := range dirs {
		path := filepath.Join(dir, fmt.Sprintf("%s.gni", basename))
		exists, err := osmisc.FileExists(path)
		if err != nil {
			return "", err
		}
		if exists {
			return filepath.Rel(checkoutDir, path)
		}
	}

	return "", nil
}

func genArgs(ctx context.Context, staticSpec *fintpb.Static, contextSpec *fintpb.Context, skipLocalArgs bool) ([]string, error) {
	// GN variables to set via args (mapping from variable name to value).
	vars := make(map[string]interface{})
	// GN list variables that could historically be set by products, and should go
	// in their own block in args.gn.  Append or assign is controlled when these
	// are formatted as lines.
	targetLists := make(map[string][]string)
	// GN targets to import.
	var imports []string
	// GN vars for managing tests.
	testVars := make(map[string]interface{})

	if staticSpec.TargetArch == fintpb.Static_ARCH_UNSPECIFIED {
		// Board files declare `target_cpu` so it's not necessary to set
		// `target_cpu` as long as we have a board file.
		if staticSpec.Board == "" {
			return nil, fmt.Errorf("target_arch must be set if board is not")
		}
	} else {
		vars["target_cpu"] = strings.ToLower(staticSpec.TargetArch.String())
	}

	if staticSpec.Optimize == fintpb.Static_OPTIMIZE_UNSPECIFIED {
		return nil, fmt.Errorf("optimize is unspecified or invalid")
	}
	vars["is_debug"] = staticSpec.Optimize == fintpb.Static_DEBUG

	if contextSpec.ClangToolchainDir != "" {
		if staticSpec.UseGoma {
			return nil, fmt.Errorf("goma is not supported for builds using a custom clang toolchain")
		}
		vars["clang_prefix"] = filepath.Join(contextSpec.ClangToolchainDir, "bin")
	}
	if contextSpec.GccToolchainDir != "" {
		if staticSpec.UseGoma {
			return nil, fmt.Errorf("goma is not supported for builds using a custom gcc toolchain")
		}
		vars["gcc_tool_dir"] = filepath.Join(contextSpec.GccToolchainDir, "bin")
	}
	if contextSpec.RustToolchainDir != "" {
		vars["rustc_prefix"] = filepath.Join(contextSpec.RustToolchainDir)
	}

	// 'use_goma' is ignored, and effectively false.
	vars["rust_rbe_enable"] = staticSpec.RustRbeEnable
	vars["cxx_rbe_enable"] = staticSpec.CxxRbeEnable
	vars["link_rbe_enable"] = staticSpec.LinkRbeEnable
	vars["enable_bazel_remote_rbe"] = staticSpec.BazelRbeEnable

	if staticSpec.Product != "" {
		basename := filepath.Base(staticSpec.Product)
		vars["build_info_product"] = strings.Split(basename, ".")[0]
		imports = append(imports, staticSpec.Product)
	}

	if staticSpec.Board != "" {
		basename := filepath.Base(staticSpec.Board)
		vars["build_info_board"] = strings.Split(basename, ".")[0]
		imports = append(imports, staticSpec.Board)
	}

	if contextSpec.SdkId != "" {
		vars["sdk_id"] = contextSpec.SdkId
	}

	if contextSpec.ReleaseVersion != "" {
		vars["build_info_version"] = contextSpec.ReleaseVersion
	}

	if staticSpec.TestDurationsFile != "" {
		testDurationsFile := staticSpec.TestDurationsFile
		exists, err := osmisc.FileExists(filepath.Join(contextSpec.CheckoutDir, testDurationsFile))
		if err != nil {
			return nil, fmt.Errorf("failed to check if TestDurationsFile exists: %w", err)
		}
		if !exists {
			testDurationsFile = staticSpec.DefaultTestDurationsFile
		}
		vars["test_durations_file"] = testDurationsFile
	}

	// TODO(ihuh): Remove once builders are including this target in their universe
	// packages.
	if staticSpec.IncludeZbiTests {
		staticSpec.UniversePackages = append(staticSpec.UniversePackages, "//bundles/boot_tests")
	}

	for varName, values := range map[string][]string{
		"base_package_labels":     staticSpec.BasePackages,
		"cache_package_labels":    staticSpec.CachePackages,
		"universe_package_labels": staticSpec.UniversePackages,
	} {
		targetLists[varName] = values
	}

	// These list variables are never initialized by a product, so they can be
	// directly set.
	vars["host_labels"] = staticSpec.HostLabels
	testVars["hermetic_test_package_labels"] = staticSpec.HermeticTestPackages
	testVars["test_package_labels"] = staticSpec.TestPackages
	testVars["e2e_test_labels"] = staticSpec.E2ETestLabels
	testVars["host_test_labels"] = staticSpec.HostTestLabels

	if len(staticSpec.Variants) != 0 {
		vars["select_variant"] = staticSpec.Variants
	}
	if contextSpec.CollectCoverage && len(contextSpec.ChangedFiles) > 0 {
		var profileSourceFiles []string
		for _, file := range contextSpec.ChangedFiles {
			profileSourceFiles = append(profileSourceFiles, fmt.Sprintf("//%s", file.Path))
		}
		// Profile changed files, and only changed files.
		vars["profile_source_files"] = profileSourceFiles
		vars["dont_profile_source_files"] = []string{}
	}

	if contextSpec.PgoProfilePath != "" {
		vars["pgo_profile_path"] = filepath.Join(contextSpec.PgoProfilePath)
	}

	if staticSpec.EnableGoCache {
		vars["gocache_dir"] = filepath.Join(contextSpec.CacheDir, "go_cache")
	} else if staticSpec.UseTemporaryGoCache {
		// We wish to have the go cache directory be deterministic,
		// because the cache directory winds up in various ninja action
		// commandlines, so having the cache directory change between
		// builds means that those action's outputs are dirtied, and we
		// re-run actions on incremental builds that differ only in go
		// cache directory.
		// However, we do still wish to preserve the invariant that the
		// cache directory is empty when UseTemporaryGoCache is
		// requested.  Thus, instead of generating a random directory
		// with os.TempDir(), we generate a predictable, deterministic
		// path to serve as the go cache directory, and ensure that it
		// is empty at the time of `fint set`, which assuming no racing
		// work is occurring on the same machine (which is a safe
		// assumption in our build infra), is semantically equivalent
		// while allowing better build caching.
		dir := filepath.Join(os.TempDir(), "fuchsia_go_cache")
		if err := os.RemoveAll(dir); err != nil {
			return nil, err
		}
		if err := os.MkdirAll(dir, 0700); err != nil {
			return nil, err
		}
		vars["gocache_dir"] = dir
	}

	if staticSpec.EnableRustCache {
		vars["rust_incremental"] = filepath.Join(contextSpec.CacheDir, "rust_cache")
	}

	var importArgs, varArgs, targetListArgs, testArgs, localArgs []string

	// Add comments to make args.gn more readable.
	varArgs = append(varArgs, "\n\n# Basic args:")
	targetListArgs = append(targetListArgs, "\n\n# Target lists:")
	testArgs = append(testArgs, "\n\n# Tests to add to build: (these are validated by test-type)")

	// vars are directly set in the "basic args" block
	for k, v := range vars {
		varArgs = append(varArgs, fmt.Sprintf("%s=%s", k, toGNValue(v)))
	}

	for _, arg := range staticSpec.GnArgs {
		if strings.HasPrefix(arg, "import(") {
			importArgs = append(importArgs, arg)
		} else if strings.Contains(arg, "+=") {
			// any list-append operation is almost always trying to change the target
			// lists set by a product or board, so add it to that block:
			targetListArgs = append(targetListArgs, arg)
		} else {
			// any directly-assigned args are more likely general build arguments.
			varArgs = append(varArgs, arg)
		}
	}
	sort.Strings(varArgs)

	for k, v := range targetLists {
		// Products and Boards are now using their own namespace of GN args, and not
		// using these target lists, which are used only by infra or developers.
		targetListArgs = append(targetListArgs, fmt.Sprintf("%s=%s", k, toGNValue(v)))
	}
	sort.Strings(targetListArgs)

	// Add the "build_only_labels" to the end of appendArgs so that it stays in
	// the "#Target lists:" block, which is semantically where it belongs.
	targetListArgs = append(targetListArgs, fmt.Sprintf("%s=%s", "build_only_labels", toGNValue(staticSpec.BuildOnlyLabels)))

	// The test vars are kept in a particular order to match the BUILD.gn files.
	for _, k := range []string{"hermetic_test_package_labels", "test_package_labels", "e2e_test_labels", "host_test_labels"} {
		testArgs = append(testArgs, fmt.Sprintf("%s=%s", k, toGNValue(testVars[k])))
	}

	if len(staticSpec.DeveloperTestLabels) != 0 && skipLocalArgs {
		return nil, fmt.Errorf("'developer_test_labels' cannot be provided when 'skipLocalArgs' is true")
	}
	testArgs = append(testArgs, "\n\n# Additional tests: (not validated by test-type)")
	testArgs = append(testArgs, fmt.Sprintf("%s=%s", "developer_test_labels", toGNValue(staticSpec.DeveloperTestLabels)))

	for _, p := range imports {
		importArgs = append(importArgs, fmt.Sprintf(`import("//%s")`, p))
	}
	sort.Strings(importArgs)

	if !skipLocalArgs {
		localArgsPath := filepath.Join(contextSpec.CheckoutDir, "local/args.gn")
		localArgsBytes, err := os.ReadFile(localArgsPath)
		localArgsContents := string(localArgsBytes)
		if err == nil {
			logger.Infof(ctx, "Including local args from %s.", localArgsPath)
			localArgs = append(localArgs, fmt.Sprintf("\n\n# Local args from %s:", localArgsPath))
			localArgs = append(localArgs, localArgsContents)
		} else if errors.Is(err, os.ErrNotExist) {
			logger.Infof(ctx, "No local args available.")
		} else {
			return nil, err
		}
	}

	// Ensure that imports come before args that set or modify variables, as
	// otherwise the imported files might blindly redefine variables set or
	// modified by other arguments.
	var finalArgs []string
	finalArgs = append(finalArgs, importArgs...)
	finalArgs = append(finalArgs, varArgs...)
	finalArgs = append(finalArgs, targetListArgs...)
	finalArgs = append(finalArgs, testArgs...)
	finalArgs = append(finalArgs, localArgs...)
	finalArgs = append(finalArgs, "\n")
	return finalArgs, nil
}

// gnFormat makes a best-effort attempt to format the input arguments using `gn
// format` so that the output will be more readable in case of any errors like
// unknown variables. If formatting fails (e.g. due to a syntax error) we'll
// just return the unformatted args and let `gn gen` return an error; otherwise
// we'd need duplicated error handling code to handle both syntax errors from
// `gn format` and non-syntax errors from `gn gen`.
func gnFormat(ctx context.Context, gn string, runner subprocessRunner, args []string) string {
	unformatted := strings.Join(args, "\n")
	var output bytes.Buffer
	opts := subprocess.RunOptions{
		Stdout: &output,
		Stderr: io.Discard,
		Stdin:  strings.NewReader(unformatted),
	}
	if err := runner.Run(ctx, []string{gn, "format", "--stdin"}, opts); err != nil {
		return unformatted
	}
	return output.String()
}

// toGNValue converts a Go value to a string representation of the corresponding
// GN value by inspecting the Go value's type. This makes the logic to set GN
// args more readable and less error-prone, with no need for patterns like
// fmt.Sprintf(`"%s"`) repeated everywhere.
//
// For example:
// - toGNValue(true) => `true`
// - toGNValue("foo") => `"foo"` (a string containing literal double-quotes)
// - toGNValue([]string{"foo", "bar"}) => `["foo","bar"]`
func toGNValue(x interface{}) string {
	switch val := x.(type) {
	case bool:
		return fmt.Sprintf("%v", val)
	case string:
		// Apply double-quotes to strings, but not to GN scopes like
		// {variant="asan-fuzzer" target_type=["fuzzed_executable"]}
		if strings.HasPrefix(val, "{") && strings.HasSuffix(val, "}") {
			return val
		}
		return fmt.Sprintf(`"%s"`, val)
	case []string:
		var values []string
		for _, element := range val {
			values = append(values, toGNValue(element))
		}
		return fmt.Sprintf("[%s]", strings.Join(values, ","))
	default:
		panic(fmt.Sprintf("unsupported arg value type %T", val))
	}
}
