// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package fint

import (
	"sort"
	"testing"

	"github.com/google/go-cmp/cmp"
	"go.fuchsia.dev/fuchsia/tools/build"
	fintpb "go.fuchsia.dev/fuchsia/tools/integration/fint/proto"
)

type fakeBuildModules struct {
	archives []build.Archive
	images   []build.Image
}

func (m fakeBuildModules) Archives() []build.Archive { return m.archives }
func (m fakeBuildModules) Images() []build.Image     { return m.images }

func TestConstructNinjaTargets(t *testing.T) {
	testCases := []struct {
		name            string
		staticSpec      *fintpb.Static
		modules         fakeBuildModules
		expectedTargets []string
	}{
		{
			name:            "empty spec produces no ninja targets",
			staticSpec:      &fintpb.Static{},
			expectedTargets: nil,
		},
		{
			name: "extra ad-hoc ninja targets",
			staticSpec: &fintpb.Static{
				NinjaTargets: []string{"foo", "bar"},
			},
			expectedTargets: []string{"foo", "bar"},
		},
		{
			name: "images for testing included",
			staticSpec: &fintpb.Static{
				IncludeImages: true,
			},
			modules: fakeBuildModules{
				images: []build.Image{
					{Name: qemuImageNames[0], Path: "qemu_image_path"},
					{Name: "should-be-ignored", Path: "different_path"},
				},
			},
			expectedTargets: append(extraTargetsForImages, "qemu_image_path", "build/images:updates"),
		},
		{
			name: "images and archives included",
			staticSpec: &fintpb.Static{
				IncludeImages:   true,
				IncludeArchives: true,
			},
			modules: fakeBuildModules{
				archives: []build.Archive{
					{Name: "packages", Path: "p.tar.gz"},
					{Name: "archive", Path: "b.tar"},
					{Name: "archive", Path: "b.tgz"},
					{Name: "other", Path: "other.tgz"},
				},
			},
			expectedTargets: append(extraTargetsForImages, "p.tar.gz", "b.tar", "b.tgz"),
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			sort.Strings(tc.expectedTargets)
			got := constructNinjaTargets(tc.modules, tc.staticSpec)
			if diff := cmp.Diff(tc.expectedTargets, got); diff != "" {
				t.Fatalf("Got wrong targets (-want +got):\n%s", diff)
			}
		})
	}
}
