// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"encoding/json"
	"flag"
	"os"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/flagmisc"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/asm"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/c"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/go_runtime"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/golang"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/kernel"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/legacy_syscall_cdecl"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/rust"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/rust_syscall"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/syscall_docs"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/zircon_ifs"
)

const (
	cBackend                  string = "c"
	goBackend                 string = "go"
	asmBackend                string = "asm"
	rustBackend               string = "rust"
	zirconIFSBackend          string = "zircon_ifs"
	kernelBackend             string = "kernel"
	legacySyscallCDeclBackend string = "legacy_syscall_cdecl"
	rustSyscallBackend        string = "rust_syscall"
	goRuntimeBackend          string = "go_runtime"
	syscallDocsBackend        string = "syscall_docs"
)

var supportedBackends = []string{
	cBackend,
	goBackend,
	asmBackend,
	rustBackend,
	zirconIFSBackend,
	kernelBackend,
	legacySyscallCDeclBackend,
	rustSyscallBackend,
	goRuntimeBackend,
	syscallDocsBackend,
}

// Flag values, grouped into a struct to be kept out of the global namespace.
var flags struct {
	irFile         string
	backend        string
	outputManifest string
	outputDir      string
	sourceDir      string
	formatter      string
	formatterArgs  flagmisc.StringsValue
}

func init() {
	flag.StringVar(&flags.irFile, "ir", "", "The FIDL IR JSON file from which bindings will be generated")
	flag.StringVar(&flags.backend, "backend", "", "The zither backend.\nSupported values: \""+strings.Join(supportedBackends, "\", \"")+"\"")
	flag.StringVar(&flags.outputManifest, "output-manifest", "", "A path to which a JSON list of the binding output files will be written, if specified. This list excludes the output manifest")
	flag.StringVar(&flags.outputDir, "output-dir", "", "The directory to which the bindings will be written. (The layout is backend-specific.)")
	flag.StringVar(&flags.sourceDir, "source-dir", "", "The path to the associated FIDL codebase, relative to the working directory at which compilation took place")
	flag.StringVar(&flags.formatter, "formatter", "", "The path to a (stdin-to-stdout) formatter, used to format bindings in the appropriate backends")
	flag.Var(&flags.formatterArgs, "formatter-args", "Arguments to pass to the formatter")
}

func main() {
	flag.Parse()

	l := logger.NewLogger(logger.InfoLevel, color.NewColor(color.ColorAuto), os.Stdout, os.Stderr, "zither: ")
	ctx := logger.WithLogger(context.Background(), l)

	if flags.irFile == "" {
		logger.Errorf(ctx, "`-ir` is a required argument")
		os.Exit(1)
	}

	f := fidlgen.NewFormatter(flags.formatter, flags.formatterArgs...)
	var gen generator
	switch flags.backend {
	case cBackend:
		gen = c.NewGenerator(f)
	case asmBackend:
		gen = asm.NewGenerator(f)
	case goBackend:
		gen = golang.NewGenerator(f)
	case rustBackend:
		gen = rust.NewGenerator(f)
	case zirconIFSBackend:
		gen = zircon_ifs.NewGenerator(f)
	case kernelBackend:
		gen = kernel.NewGenerator(f)
	case legacySyscallCDeclBackend:
		gen = legacy_syscall_cdecl.NewGenerator(f)
	case rustSyscallBackend:
		gen = rust_syscall.NewGenerator(f)
	case goRuntimeBackend:
		gen = go_runtime.NewGenerator(f)
	case syscallDocsBackend:
		gen = syscall_docs.NewGenerator(f)
	default:
		logger.Errorf(ctx, "unrecognized `-backend` value: %q", flags.backend)
		os.Exit(1)
	}

	ir, err := fidlgen.ReadJSONIr(flags.irFile)
	if err != nil {
		logger.Errorf(ctx, "%s", err)
		os.Exit(1)
	}

	if err := execute(ctx, gen, ir, flags.sourceDir, flags.outputDir, flags.outputManifest); err != nil {
		logger.Errorf(ctx, "%s", err)
		os.Exit(1)
	}
}

// generator represents an abstract generator of bindings.
type generator interface {
	// DeclOrder gives the declaration order desired by the backend.
	DeclOrder() zither.DeclOrder

	// DeclCallback is a callback intended to be passed to zither.Summarize()
	// which will be called on each Decl in topological 'dependency' order.
	// This can be used to build up additional, backend-specific information
	// during the main phase of FIDL declaration processing.
	DeclCallback(zither.Decl)

	// Generate generates bindings into the provided output directory,
	// returning the list of outputs emitted.
	Generate(summaries []zither.FileSummary, outputDir string) ([]string, error)
}

func execute(ctx context.Context, gen generator, ir fidlgen.Root, sourceDir, outputDir, outputManifest string) error {
	summaries, err := zither.Summarize(ir, sourceDir, gen.DeclOrder(), gen.DeclCallback)
	if err != nil {
		return err
	}

	outputs, err := gen.Generate(summaries, outputDir)
	if err != nil {
		return err
	}

	if outputManifest != "" {
		f, err := os.Create(outputManifest)
		if err != nil {
			return err
		}

		encoder := json.NewEncoder(f)
		encoder.SetIndent("", "\t")
		if err := encoder.Encode(outputs); err != nil {
			f.Close()
			return err
		}

		if err := f.Close(); err != nil {
			return err
		}
	}

	return nil
}
