// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

// Program to watch for a specific string to appear from a socket's output and
// then exits successfully.

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/lib/color"
	"go.fuchsia.dev/fuchsia/tools/lib/iomisc"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	testrunnerconstants "go.fuchsia.dev/fuchsia/tools/testing/testrunner/constants"
)

var (
	timeout        time.Duration
	successString  string
	redirectStdout bool
)

func init() {
	flag.DurationVar(&timeout, "timeout", 10*time.Minute, "amount of time to wait for success string")
	flag.BoolVar(&redirectStdout, "stdout", false, "whether to redirect serial output to stdout")
	flag.StringVar(&successString, "success-str", "", "string that - if read - indicates success")
}

func execute(ctx context.Context, serialLogPath string, stdout io.Writer) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	if successString == "" {
		flag.Usage()
		return fmt.Errorf("-success is a required argument")
	}
	if serialLogPath == "" {
		return fmt.Errorf("could not find serial log path in environment")
	}

	serialReader, err := os.Open(serialLogPath)
	if err != nil {
		return fmt.Errorf("failed to open serial log: %w", err)
	}
	logger.Debugf(ctx, "serial log: %s", serialLogPath)
	defer serialReader.Close()
	serialTee := io.TeeReader(serialReader, stdout)

	// Print out a log periodically to give an estimate of the timestamp at which
	// logs are getting read from the socket.
	tickerSecs := math.Min(30, timeout.Seconds()/2)
	ticker := time.NewTicker(time.Duration(tickerSecs) * time.Second)
	defer ticker.Stop()
	go func() {
		for range ticker.C {
			logger.Debugf(ctx, "still running test...")
		}
	}()

	for {
		if match, err := iomisc.ReadUntilMatchString(ctx, serialTee, successString); err != nil {
			if ctx.Err() != nil {
				return fmt.Errorf("timed out before success string %q was read from serial", successString)
			}
			if !errors.Is(err, io.EOF) {
				return fmt.Errorf("error trying to read from serial log: %w", err)
			}
			// The serial log is continuously being written to, so ReadUntilMatchString() may
			// return an EOF if it's read everything that's been written so far. Keep trying
			// to read until the success string is found or the timeout is hit.
			time.Sleep(100 * time.Millisecond)
			continue
		} else if match != successString {
			return fmt.Errorf("match found %q doesn't match successString %q", match, successString)
		}
		break
	}
	logger.Debugf(ctx, "success string found: %q", successString)
	return nil
}

func main() {
	flag.Parse()

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer cancel()
	log := logger.NewLogger(logger.DebugLevel, color.NewColor(color.ColorAuto),
		os.Stdout, os.Stderr, "seriallistener ")
	ctx = logger.WithLogger(ctx, log)

	// Emulator serial is already wired up to stdout
	// TODO(https://fxbug.dev/42067738): Temporarily write serial output
	// to a file for debugging purposes.
	stdout := io.Discard
	if outDir := os.Getenv(testrunnerconstants.TestOutDirEnvKey); outDir != "" {
		if serialOutput, err := osmisc.CreateFile(filepath.Join(outDir, "serial_output")); err != nil {
			logger.Errorf(ctx, "%s", err)
		} else {
			stdout = serialOutput
			// Have the logger write to the file as well to get a
			// better sense of how much is read from the socket before
			// the socket io or ticker timeouts are reached.
			log := logger.NewLogger(logger.DebugLevel, color.NewColor(color.ColorAuto),
				io.MultiWriter(os.Stdout, serialOutput), io.MultiWriter(os.Stderr, serialOutput), "seriallistener ")
			ctx = logger.WithLogger(ctx, log)
			defer serialOutput.Close()
		}
	}
	deviceType := os.Getenv(constants.DeviceTypeEnvKey)
	if deviceType != "QEMU" && deviceType != "AEMU" {
		stdout = os.Stdout
	}

	serialLogPath := os.Getenv(constants.SerialLogEnvKey)
	if err := execute(ctx, serialLogPath, stdout); err != nil {
		logger.Fatalf(ctx, "%s", err)
	}
}
