// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package tefmocheck

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"go.fuchsia.dev/fuchsia/tools/testing/runtests"

	"github.com/google/go-cmp/cmp"
)

func TestLoadTestSummaryPassesInputSummaryThrough(t *testing.T) {
	inputSummary := runtests.TestSummary{
		Tests: []runtests.TestDetails{
			{Name: "test name"},
		},
	}
	summaryBytes, err := json.Marshal(inputSummary)
	if err != nil {
		t.Fatal("Marshal(inputSummary) failed:", err)
	}
	outputSummary, err := LoadTestSummary(mkTempFile(t, summaryBytes))
	if err != nil {
		t.Errorf("LoadSwarmingTaskSummary failed: %v", err)
	} else if diff := cmp.Diff(outputSummary, &inputSummary); diff != "" {
		t.Errorf("LoadSwarmingTaskSummary reutrned wrong value (-got +want):\n%s", diff)
	}
}

// mkTempFile returns a new temporary file containing the specified content
// that will be cleaned up automatically.
func mkTempFile(t *testing.T, content []byte) string {
	name := filepath.Join(t.TempDir(), "tefmocheck-cmd-test")
	if err := os.WriteFile(name, content, 0o600); err != nil {
		t.Fatal(err)
	}
	return name
}
